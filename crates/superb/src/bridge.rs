//! Thin session → mixer bridge.
//!
//! The document model ([`superb_session::Session`]) and the realtime console
//! ([`superb_mixer::Mixer`]) use independent [`superb_core::TrackId`] /
//! [`superb_core::SendId`] namespaces. This module keeps a side
//! table that maps session tracks onto mixer strips / buses (and session
//! sends onto mixer sends), pushes fader/mute/solo/pan and send levels from
//! the document onto the atomics, and mirrors the document's routing
//! (track → bus outputs, sends) onto the mixer topology.
//!
//! Clip sources (players, synths) stay with [`crate::playback`] — the
//! bridge only deals with strips, buses, outputs and sends.

use superb_core::{BusId, Result, SendId, TrackId};
use superb_mixer::{Mixer, SendTap};
use superb_session::{Session, TrackKind};

/// One session track mapped onto a mixer strip or bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixerBinding {
    /// Session [`TrackKind::Audio`] or [`TrackKind::Midi`] → mixer track.
    Track(TrackId),
    /// Session [`TrackKind::Bus`] → mixer bus.
    Bus(BusId),
}

/// Side table from session track IDs to mixer strips / buses (and from
/// session sends to mixer sends).
#[derive(Clone, Debug, Default)]
pub struct SessionMixerBridge {
    entries: Vec<(TrackId, MixerBinding)>,
    sends: Vec<(SendId, SendId)>,
    session_revision: u64,
}

impl SessionMixerBridge {
    /// Empty bridge.
    pub fn new() -> Self {
        Self::default()
    }

    /// Session revision last seen by [`sync`](Self::sync) /
    /// [`apply_strip_params`](Self::apply_strip_params) /
    /// [`apply_routing`](Self::apply_routing).
    pub fn session_revision(&self) -> u64 {
        self.session_revision
    }

    /// Binding for a session track, if any.
    pub fn binding(&self, session_track: TrackId) -> Option<MixerBinding> {
        self.entries
            .iter()
            .find(|(id, _)| *id == session_track)
            .map(|(_, b)| *b)
    }

    /// Mixer track for a session audio/MIDI track.
    pub fn mixer_track(&self, session_track: TrackId) -> Option<TrackId> {
        match self.binding(session_track)? {
            MixerBinding::Track(id) => Some(id),
            MixerBinding::Bus(_) => None,
        }
    }

    /// Mixer bus for a session bus track.
    pub fn mixer_bus(&self, session_track: TrackId) -> Option<BusId> {
        match self.binding(session_track)? {
            MixerBinding::Bus(id) => Some(id),
            MixerBinding::Track(_) => None,
        }
    }

    /// Mixer send for a session send, once [`apply_routing`](Self::apply_routing)
    /// has created it.
    pub fn mixer_send(&self, session_send: SendId) -> Option<SendId> {
        self.sends
            .iter()
            .find(|(id, _)| *id == session_send)
            .map(|(_, m)| *m)
    }

    /// All bindings in session order.
    pub fn entries(&self) -> &[(TrackId, MixerBinding)] {
        &self.entries
    }

    /// All send bindings as `(session send, mixer send)`.
    pub fn send_entries(&self) -> &[(SendId, SendId)] {
        &self.sends
    }

    /// Ensures every session track has a mixer counterpart.
    ///
    /// * Creates a mixer track for each unmapped [`TrackKind::Audio`] /
    ///   [`TrackKind::Midi`] session track (name copied).
    /// * Creates a mixer bus for each unmapped [`TrackKind::Bus`].
    /// * Drops bindings whose session track no longer exists.
    /// * Does **not** remove mixer strips/buses that lost their session
    ///   track — the application owns teardown so live audio is not cut
    ///   unexpectedly. (Stale mixer *sends* are removed by
    ///   [`apply_routing`](Self::apply_routing), since a lingering send
    ///   would keep feeding a bus the document no longer routes to.)
    ///
    /// Routing is not touched here; call [`apply_routing`](Self::apply_routing)
    /// afterwards. Returns the number of newly created mixer objects.
    pub fn sync(&mut self, session: &Session, mixer: &mut Mixer) -> usize {
        // Drop stale bindings.
        self.entries.retain(|(sid, _)| session.track(*sid).is_ok());

        let mut created = 0usize;
        for track in session.tracks() {
            if self.binding(track.id()).is_some() {
                continue;
            }
            let binding = match track.kind {
                TrackKind::Audio | TrackKind::Midi => {
                    let id = mixer.add_track(track.name.clone());
                    created += 1;
                    MixerBinding::Track(id)
                }
                TrackKind::Bus => {
                    let id = mixer.add_bus(track.name.clone());
                    created += 1;
                    MixerBinding::Bus(id)
                }
            };
            self.entries.push((track.id(), binding));
        }
        self.session_revision = session.revision();
        created
    }

    /// Copies gain / pan / mute / solo from every mapped session track onto
    /// the corresponding mixer strip (or bus) atomics, and level / enable
    /// from every mapped session send onto its mixer send.
    ///
    /// Safe to call from the control thread whenever the session revision
    /// moves; does not allocate beyond iterating the side tables.
    pub fn apply_strip_params(&mut self, session: &Session, mixer: &Mixer) {
        for (session_id, binding) in &self.entries {
            let Ok(track) = session.track(*session_id) else {
                continue;
            };
            match binding {
                MixerBinding::Track(mid) => {
                    if let Ok(params) = mixer.track_params(*mid) {
                        params.set_gain_db(track.gain_db);
                        params.set_pan(track.pan);
                        params.set_mute(track.muted);
                        params.set_solo(track.soloed);
                    }
                }
                MixerBinding::Bus(bid) => {
                    if let Ok(params) = mixer.bus_params(*bid) {
                        params.set_gain_db(track.gain_db);
                        params.set_pan(track.pan);
                        params.set_mute(track.muted);
                        // Buses have no shared solo count by default; still
                        // mirror the flag for consistency.
                        params.set_solo(track.soloed);
                    }
                }
            }
        }
        self.mirror_send_params(session, mixer);
        self.session_revision = session.revision();
    }

    fn mirror_send_params(&self, session: &Session, mixer: &Mixer) {
        for (session_send, mixer_send) in &self.sends {
            let Ok(send) = session.send(*session_send) else {
                continue;
            };
            if let Ok(params) = mixer.send_params(*mixer_send) {
                params.set_level_db(send.level_db);
                params.set_enabled(send.enabled);
            }
        }
    }

    /// Mirrors the document's routing onto the mixer topology.
    ///
    /// * Every bound track/bus is routed to the mixer bus bound to its
    ///   session [`output`](superb_session::Track::output), or to the master
    ///   when the output is `None` (or its bus is not bound).
    /// * Every session send gets a mixer send with the same destination and
    ///   tap; a send whose destination or tap changed is re-created, and
    ///   mixer sends whose session send disappeared are removed. Send level
    ///   / enable are mirrored as well.
    ///
    /// Call after [`sync`](Self::sync) so every bus is bound. Returns the
    /// number of structural mixer edits (re-routes, sends added or
    /// removed); when it is non-zero the mixer needs a `prepare` before the
    /// next `process`. Strip-parameter mirroring is *not* part of this
    /// call. Mixer objects the application removed behind the bridge's back
    /// are skipped.
    pub fn apply_routing(&mut self, session: &Session, mixer: &mut Mixer) -> Result<usize> {
        let master = mixer.master();
        let mut changed = 0usize;

        // Outputs.
        for (session_id, binding) in &self.entries {
            let Ok(track) = session.track(*session_id) else {
                continue;
            };
            let target = track
                .output()
                .and_then(|bus| self.mixer_bus(bus))
                .unwrap_or(master);
            match binding {
                MixerBinding::Track(mid) => {
                    let Ok(info) = mixer.track(*mid) else {
                        continue;
                    };
                    if info.output != target {
                        mixer.set_track_output(*mid, target)?;
                        changed += 1;
                    }
                }
                MixerBinding::Bus(bid) => {
                    let Ok(info) = mixer.bus(*bid) else { continue };
                    if *bid == master {
                        continue;
                    }
                    if info.output != Some(target) {
                        mixer.set_bus_output(*bid, target)?;
                        changed += 1;
                    }
                }
            }
        }

        // Sends whose session send is gone.
        let mut stale = Vec::new();
        self.sends.retain(|(session_send, mixer_send)| {
            if session.send(*session_send).is_ok() {
                true
            } else {
                stale.push(*mixer_send);
                false
            }
        });
        for mixer_send in stale {
            if mixer.send(mixer_send).is_ok() {
                mixer.remove_send(mixer_send)?;
                changed += 1;
            }
        }

        // Sends: create, re-create on destination/tap change, keep otherwise.
        for track in session.tracks() {
            let Some(MixerBinding::Track(mid)) = self.binding(track.id()) else {
                continue;
            };
            if mixer.track(mid).is_err() {
                continue;
            }
            for send in track.sends() {
                let Some(bus) = self.mixer_bus(send.bus()) else {
                    continue;
                };
                if mixer.bus(bus).is_err() {
                    continue;
                }
                let tap = if send.pre_fader {
                    SendTap::PreFader
                } else {
                    SendTap::PostFader
                };
                if let Some(existing) = self.mixer_send(send.id()) {
                    match mixer.send(existing) {
                        Ok(info) if info.track == mid && info.bus == bus && info.tap == tap => {
                            continue
                        }
                        Ok(_) => {
                            mixer.remove_send(existing)?;
                            changed += 1;
                        }
                        Err(_) => {}
                    }
                    self.sends.retain(|(sid, _)| *sid != send.id());
                }
                let level = superb_buffers::gain::db_to_linear(send.level_db);
                let created = mixer.add_send(mid, bus, level, tap)?;
                mixer.send_params(created)?.set_enabled(send.enabled);
                self.sends.push((send.id(), created));
                changed += 1;
            }
        }

        self.mirror_send_params(session, mixer);
        self.session_revision = session.revision();
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superb_core::SampleRate;
    use superb_session::{TrackKind, TrackSend};

    #[test]
    fn sync_creates_tracks_and_buses_and_maps_ids() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let audio = session.add_track("lead", TrackKind::Audio);
        let midi = session.add_track("keys", TrackKind::Midi);
        let bus = session.add_track("reverb", TrackKind::Bus);

        let mut mixer = Mixer::new(2);
        let mut bridge = SessionMixerBridge::new();
        assert_eq!(bridge.sync(&session, &mut mixer), 3);
        assert_eq!(bridge.sync(&session, &mut mixer), 0); // idempotent

        assert_eq!(mixer.tracks().len(), 2);
        assert!(bridge.mixer_track(audio).is_some());
        assert!(bridge.mixer_track(midi).is_some());
        assert!(bridge.mixer_bus(bus).is_some());
        assert_eq!(bridge.session_revision(), session.revision());
    }

    #[test]
    fn apply_strip_params_mirrors_gain_mute() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let id = session.add_track("lead", TrackKind::Audio);
        session.track_mut(id).unwrap().gain_db = -6.0;
        session.track_mut(id).unwrap().muted = true;
        session.track_mut(id).unwrap().pan = -0.5;

        let mut mixer = Mixer::new(2);
        let mut bridge = SessionMixerBridge::new();
        bridge.sync(&session, &mut mixer);
        bridge.apply_strip_params(&session, &mixer);

        let mid = bridge.mixer_track(id).unwrap();
        let params = mixer.track_params(mid).unwrap();
        assert!((params.gain_db() - -6.0).abs() < 1e-6);
        assert!(params.is_muted());
        assert!((params.pan() - -0.5).abs() < 1e-6);
    }

    #[test]
    fn removing_session_track_drops_binding_but_keeps_mixer_strip() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let id = session.add_track("lead", TrackKind::Audio);
        let mut mixer = Mixer::new(2);
        let mut bridge = SessionMixerBridge::new();
        bridge.sync(&session, &mut mixer);
        let mid = bridge.mixer_track(id).unwrap();
        session.remove_track(id).unwrap();
        assert_eq!(bridge.sync(&session, &mut mixer), 0);
        assert!(bridge.binding(id).is_none());
        assert_eq!(mixer.tracks().len(), 1);
        assert!(mixer.track(mid).is_ok());
    }

    #[test]
    fn apply_routing_mirrors_outputs_and_follows_changes() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let lead = session.add_track("lead", TrackKind::Audio);
        let keys = session.add_track("keys", TrackKind::Midi);
        let sub = session.add_track("sub", TrackKind::Bus);
        let room = session.add_track("room", TrackKind::Bus);
        session.set_track_output(lead, Some(sub)).unwrap();
        session.set_track_output(sub, Some(room)).unwrap();

        let mut mixer = Mixer::new(2);
        let mut bridge = SessionMixerBridge::new();
        bridge.sync(&session, &mut mixer);
        assert_eq!(
            bridge.apply_routing(&session, &mut mixer).unwrap(),
            2,
            "lead → sub, sub → room"
        );
        assert_eq!(
            bridge.apply_routing(&session, &mut mixer).unwrap(),
            0,
            "idempotent"
        );

        let (m_lead, m_keys) = (
            bridge.mixer_track(lead).unwrap(),
            bridge.mixer_track(keys).unwrap(),
        );
        let (m_sub, m_room) = (
            bridge.mixer_bus(sub).unwrap(),
            bridge.mixer_bus(room).unwrap(),
        );
        assert_eq!(mixer.track(m_lead).unwrap().output, m_sub);
        assert_eq!(mixer.track(m_keys).unwrap().output, mixer.master());
        assert_eq!(mixer.bus(m_sub).unwrap().output, Some(m_room));
        assert_eq!(mixer.bus(m_room).unwrap().output, Some(mixer.master()));

        // Back to the master, and a bus that loses its session output.
        session.set_track_output(lead, None).unwrap();
        session.set_track_output(sub, None).unwrap();
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 2);
        assert_eq!(mixer.track(m_lead).unwrap().output, mixer.master());
        assert_eq!(mixer.bus(m_sub).unwrap().output, Some(mixer.master()));

        // Removing the session bus detaches in the document; the mixer bus
        // stays (sync never removes strips) but nothing routes into it.
        session.set_track_output(lead, Some(room)).unwrap();
        bridge.apply_routing(&session, &mut mixer).unwrap();
        session.remove_track(room).unwrap();
        bridge.sync(&session, &mut mixer);
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 1);
        assert_eq!(mixer.track(m_lead).unwrap().output, mixer.master());
        assert!(mixer.bus(m_room).is_ok());
        assert_eq!(bridge.session_revision(), session.revision());
    }

    #[test]
    fn apply_routing_creates_updates_and_removes_sends() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let lead = session.add_track("lead", TrackKind::Audio);
        let fx = session.add_track("fx", TrackKind::Bus);
        let room = session.add_track("room", TrackKind::Bus);
        let a = session
            .add_send(lead, TrackSend::new(fx).level_db(-6.0))
            .unwrap();
        let b = session
            .add_send(lead, TrackSend::new(room).pre_fader())
            .unwrap();
        session.send_mut(b).unwrap().enabled = false;

        let mut mixer = Mixer::new(2);
        let mut bridge = SessionMixerBridge::new();
        bridge.sync(&session, &mut mixer);
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 2);
        assert_eq!(mixer.sends().len(), 2);
        let ma = bridge.mixer_send(a).unwrap();
        let mb = bridge.mixer_send(b).unwrap();
        assert_eq!(bridge.send_entries().len(), 2);
        let info = mixer.send(ma).unwrap();
        assert_eq!(info.track, bridge.mixer_track(lead).unwrap());
        assert_eq!(info.bus, bridge.mixer_bus(fx).unwrap());
        assert_eq!(info.tap, SendTap::PostFader);
        assert!((mixer.send_params(ma).unwrap().level_db() - -6.0).abs() < 1e-4);
        assert!(mixer.send_params(ma).unwrap().is_enabled());
        assert_eq!(mixer.send(mb).unwrap().tap, SendTap::PreFader);
        assert!(!mixer.send_params(mb).unwrap().is_enabled());

        // Parameter-only changes: no structural edit, mirrored by either call.
        session.send_mut(a).unwrap().level_db = -12.0;
        session.send_mut(b).unwrap().enabled = true;
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 0);
        assert!((mixer.send_params(ma).unwrap().level_db() - -12.0).abs() < 1e-4);
        assert!(mixer.send_params(mb).unwrap().is_enabled());
        session.send_mut(a).unwrap().level_db = -1.0;
        bridge.apply_strip_params(&session, &mixer);
        assert!((mixer.send_params(ma).unwrap().level_db() - -1.0).abs() < 1e-4);

        // Tap change re-creates the mixer send under the same session id.
        session.send_mut(b).unwrap().pre_fader = false;
        assert_eq!(
            bridge.apply_routing(&session, &mut mixer).unwrap(),
            2,
            "remove + add"
        );
        let mb2 = bridge.mixer_send(b).unwrap();
        assert_ne!(mb, mb2);
        assert!(mixer.send(mb).is_err());
        assert_eq!(mixer.send(mb2).unwrap().tap, SendTap::PostFader);
        assert_eq!(mixer.sends().len(), 2);

        // Removing the session send removes the mixer send.
        session.remove_send(a).unwrap();
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 1);
        assert!(bridge.mixer_send(a).is_none());
        assert!(mixer.send(ma).is_err());
        assert_eq!(mixer.sends().len(), 1);

        // Removing the destination bus drops the send in the document and
        // therefore in the mixer; the mixer bus itself stays.
        session.remove_track(room).unwrap();
        bridge.sync(&session, &mut mixer);
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 1);
        assert!(mixer.sends().is_empty());
        assert!(bridge.send_entries().is_empty());
        assert_eq!(mixer.buses().len(), 3);
    }

    #[test]
    fn apply_routing_tolerates_mixer_objects_removed_by_the_app() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let lead = session.add_track("lead", TrackKind::Audio);
        let fx = session.add_track("fx", TrackKind::Bus);
        session.set_track_output(lead, Some(fx)).unwrap();
        session.add_send(lead, TrackSend::new(fx)).unwrap();
        let mut mixer = Mixer::new(2);
        let mut bridge = SessionMixerBridge::new();
        bridge.sync(&session, &mut mixer);
        bridge.apply_routing(&session, &mut mixer).unwrap();
        // The app tears down the bus and the strip behind the bridge's back.
        mixer.remove_bus(bridge.mixer_bus(fx).unwrap()).unwrap();
        mixer
            .remove_track(bridge.mixer_track(lead).unwrap())
            .unwrap();
        assert_eq!(bridge.apply_routing(&session, &mut mixer).unwrap(), 0);
        bridge.apply_strip_params(&session, &mixer);
    }
}
