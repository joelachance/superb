//! Thin document → mixer bridge.
//!
//! The document model ([`Document`]) and the realtime console
//! ([`Mixer`]) use independent [`TrackId`] /
//! [`SendId`] namespaces. This module keeps a side
//! table that maps document tracks onto mixer strips / buses (and document
//! sends onto mixer sends), pushes fader/mute/solo/pan and send levels from
//! the document onto the atomics, and mirrors the document's routing
//! (track → bus outputs, sends) onto the mixer topology.
//!
//! Clip sources (players, synths) stay with [`crate::session::wiring`] — the
//! bridge only deals with strips, buses, outputs and sends.

use crate::primitives::mixer::{Mixer, SendTap};
use crate::primitives::session::{Document, TrackKind};
use crate::primitives::{BusId, Result, SendId, TrackId};

/// One document track mapped onto a mixer strip or bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixerBinding {
    /// Document [`TrackKind::Audio`] or [`TrackKind::Midi`] → mixer track.
    Track(TrackId),
    /// Document [`TrackKind::Bus`] → mixer bus.
    Bus(BusId),
}

/// Side table from document track IDs to mixer strips / buses (and from
/// document sends to mixer sends).
#[derive(Clone, Debug, Default)]
pub struct MixerBridge {
    entries: Vec<(TrackId, MixerBinding)>,
    sends: Vec<(SendId, SendId)>,
    document_revision: u64,
}

impl MixerBridge {
    /// Empty bridge.
    pub fn new() -> Self {
        Self::default()
    }

    /// Document revision last seen by [`bind`](Self::bind) /
    /// [`apply_strip_params`](Self::apply_strip_params) /
    /// [`apply_routing`](Self::apply_routing).
    #[cfg(test)]
    pub fn document_revision(&self) -> u64 {
        self.document_revision
    }

    /// Binding for a document track, if any.
    pub fn binding(&self, track: TrackId) -> Option<MixerBinding> {
        self.entries
            .iter()
            .find(|(id, _)| *id == track)
            .map(|(_, b)| *b)
    }

    /// Mixer track for a document audio/MIDI track.
    #[cfg(test)]
    pub fn mixer_track(&self, track: TrackId) -> Option<TrackId> {
        match self.binding(track)? {
            MixerBinding::Track(id) => Some(id),
            MixerBinding::Bus(_) => None,
        }
    }

    /// Mixer bus for a document bus track.
    pub fn mixer_bus(&self, track: TrackId) -> Option<BusId> {
        match self.binding(track)? {
            MixerBinding::Bus(id) => Some(id),
            MixerBinding::Track(_) => None,
        }
    }

    /// Mixer send for a document send, once [`apply_routing`](Self::apply_routing)
    /// has created it.
    pub fn mixer_send(&self, send: SendId) -> Option<SendId> {
        self.sends
            .iter()
            .find(|(id, _)| *id == send)
            .map(|(_, m)| *m)
    }

    /// All bindings in document order.
    pub fn entries(&self) -> &[(TrackId, MixerBinding)] {
        &self.entries
    }

    /// All send bindings as `(document send, mixer send)`.
    pub fn send_entries(&self) -> &[(SendId, SendId)] {
        &self.sends
    }

    /// Ensures every document track has a mixer counterpart.
    ///
    /// * Creates a mixer track for each unmapped [`TrackKind::Audio`] /
    ///   [`TrackKind::Midi`] document track (name copied).
    /// * Creates a mixer bus for each unmapped [`TrackKind::Bus`].
    /// * Drops bindings whose document track no longer exists.
    /// * Does **not** remove mixer strips/buses that lost their document
    ///   track — the application owns teardown so live audio is not cut
    ///   unexpectedly. (Stale mixer *sends* are removed by
    ///   [`apply_routing`](Self::apply_routing), since a lingering send
    ///   would keep feeding a bus the document no longer routes to.)
    ///
    /// Routing is not touched here; call [`apply_routing`](Self::apply_routing)
    /// afterwards. Returns the number of newly created mixer objects.
    pub fn bind(&mut self, document: &Document, mixer: &mut Mixer) -> usize {
        // Drop stale bindings.
        self.entries.retain(|(sid, _)| document.track(*sid).is_ok());

        let mut created = 0usize;
        for track in document.tracks() {
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
        self.document_revision = document.revision();
        created
    }

    /// Copies gain / pan / mute / solo from every mapped document track onto
    /// the corresponding mixer strip (or bus) atomics, and level / enable
    /// from every mapped document send onto its mixer send.
    ///
    /// Safe to call from the control thread whenever the document revision
    /// moves; does not allocate beyond iterating the side tables.
    pub fn apply_strip_params(&mut self, document: &Document, mixer: &Mixer) {
        for (session_id, binding) in &self.entries {
            let Ok(track) = document.track(*session_id) else {
                continue;
            };
            match binding {
                MixerBinding::Track(mid) => {
                    if let Ok(params) = mixer.track_params(*mid) {
                        params.set_volume_db(track.state.volume_db);
                        params.set_pan(track.state.pan);
                        params.set_mute(track.state.mute);
                        params.set_solo(track.state.solo);
                    }
                }
                MixerBinding::Bus(bid) => {
                    if let Ok(params) = mixer.bus_params(*bid) {
                        params.set_volume_db(track.state.volume_db);
                        params.set_pan(track.state.pan);
                        params.set_mute(track.state.mute);
                        // Buses have no shared solo count by default; still
                        // mirror the flag for consistency.
                        params.set_solo(track.state.solo);
                    }
                }
            }
        }
        self.mirror_send_params(document, mixer);
        self.document_revision = document.revision();
    }

    fn mirror_send_params(&self, document: &Document, mixer: &Mixer) {
        for (send, mixer_send) in &self.sends {
            let Ok(send) = document.send(*send) else {
                continue;
            };
            if let Ok(params) = mixer.send_params(*mixer_send) {
                params.set_level_db(send.state.level_db);
                params.set_enabled(send.state.enabled);
            }
        }
    }

    /// Mirrors the document's routing onto the mixer topology.
    ///
    /// * Every bound track/bus is routed to the mixer bus bound to its
    ///   document [`output`](crate::primitives::session::Track::output), or to the master
    ///   when the output is `None` (or its bus is not bound).
    /// * Every document send gets a mixer send with the same destination and
    ///   tap; a send whose destination or tap changed is re-created, and
    ///   mixer sends whose document send disappeared are removed. Send level
    ///   / enable are mirrored as well.
    ///
    /// Call after [`bind`](Self::bind) so every bus is bound. Returns the
    /// number of structural mixer edits (re-routes, sends added or
    /// removed); when it is non-zero the mixer needs a `prepare` before the
    /// next `process`. Strip-parameter mirroring is *not* part of this
    /// call. Mixer objects the application removed behind the bridge's back
    /// are skipped.
    pub fn apply_routing(&mut self, document: &Document, mixer: &mut Mixer) -> Result<usize> {
        let master = mixer.master();
        let mut changed = 0usize;

        // Outputs.
        for (session_id, binding) in &self.entries {
            let Ok(track) = document.track(*session_id) else {
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

        // Sends whose document send is gone.
        let mut stale = Vec::new();
        self.sends.retain(|(send, mixer_send)| {
            if document.send(*send).is_ok() {
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
        for track in document.tracks() {
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
                let level = crate::primitives::buffers::gain::db_to_linear(send.state.level_db);
                let created = mixer.add_send(mid, bus, level, tap)?;
                mixer.send_params(created)?.set_enabled(send.state.enabled);
                self.sends.push((send.id(), created));
                changed += 1;
            }
        }

        self.mirror_send_params(document, mixer);
        self.document_revision = document.revision();
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::{TrackKind, TrackSend};
    use crate::primitives::SampleRate;

    #[test]
    fn sync_creates_tracks_and_buses_and_maps_ids() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let audio = document.add_track("lead", TrackKind::Audio);
        let midi = document.add_track("keys", TrackKind::Midi);
        let bus = document.add_track("reverb", TrackKind::Bus);

        let mut mixer = Mixer::new(2);
        let mut bridge = MixerBridge::new();
        assert_eq!(bridge.bind(&document, &mut mixer), 3);
        assert_eq!(bridge.bind(&document, &mut mixer), 0); // idempotent

        assert_eq!(mixer.tracks().len(), 2);
        assert!(bridge.mixer_track(audio).is_some());
        assert!(bridge.mixer_track(midi).is_some());
        assert!(bridge.mixer_bus(bus).is_some());
        assert_eq!(bridge.document_revision(), document.revision());
    }

    #[test]
    fn apply_strip_params_mirrors_gain_mute() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let id = document.add_track("lead", TrackKind::Audio);
        document.track_mut(id).unwrap().state.volume_db = -6.0;
        document.track_mut(id).unwrap().state.mute = true;
        document.track_mut(id).unwrap().state.pan = -0.5;

        let mut mixer = Mixer::new(2);
        let mut bridge = MixerBridge::new();
        bridge.bind(&document, &mut mixer);
        bridge.apply_strip_params(&document, &mixer);

        let mid = bridge.mixer_track(id).unwrap();
        let params = mixer.track_params(mid).unwrap();
        assert!((params.volume_db() - -6.0).abs() < 1e-6);
        assert!(params.is_muted());
        assert!((params.pan() - -0.5).abs() < 1e-6);
    }

    #[test]
    fn removing_session_track_drops_binding_but_keeps_mixer_strip() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let id = document.add_track("lead", TrackKind::Audio);
        let mut mixer = Mixer::new(2);
        let mut bridge = MixerBridge::new();
        bridge.bind(&document, &mut mixer);
        let mid = bridge.mixer_track(id).unwrap();
        document.remove_track(id).unwrap();
        assert_eq!(bridge.bind(&document, &mut mixer), 0);
        assert!(bridge.binding(id).is_none());
        assert_eq!(mixer.tracks().len(), 1);
        assert!(mixer.track(mid).is_ok());
    }

    #[test]
    fn apply_routing_mirrors_outputs_and_follows_changes() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let lead = document.add_track("lead", TrackKind::Audio);
        let keys = document.add_track("keys", TrackKind::Midi);
        let sub = document.add_track("sub", TrackKind::Bus);
        let room = document.add_track("room", TrackKind::Bus);
        document.set_track_output(lead, Some(sub)).unwrap();
        document.set_track_output(sub, Some(room)).unwrap();

        let mut mixer = Mixer::new(2);
        let mut bridge = MixerBridge::new();
        bridge.bind(&document, &mut mixer);
        assert_eq!(
            bridge.apply_routing(&document, &mut mixer).unwrap(),
            2,
            "lead → sub, sub → room"
        );
        assert_eq!(
            bridge.apply_routing(&document, &mut mixer).unwrap(),
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

        // Back to the master, and a bus that loses its document output.
        document.set_track_output(lead, None).unwrap();
        document.set_track_output(sub, None).unwrap();
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 2);
        assert_eq!(mixer.track(m_lead).unwrap().output, mixer.master());
        assert_eq!(mixer.bus(m_sub).unwrap().output, Some(mixer.master()));

        // Removing the document bus detaches in the document; the mixer bus
        // stays (bind never removes strips) but nothing routes into it.
        document.set_track_output(lead, Some(room)).unwrap();
        bridge.apply_routing(&document, &mut mixer).unwrap();
        document.remove_track(room).unwrap();
        bridge.bind(&document, &mut mixer);
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 1);
        assert_eq!(mixer.track(m_lead).unwrap().output, mixer.master());
        assert!(mixer.bus(m_room).is_ok());
        assert_eq!(bridge.document_revision(), document.revision());
    }

    #[test]
    fn apply_routing_creates_updates_and_removes_sends() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let lead = document.add_track("lead", TrackKind::Audio);
        let fx = document.add_track("fx", TrackKind::Bus);
        let room = document.add_track("room", TrackKind::Bus);
        let a = document
            .add_send(lead, TrackSend::new(fx).level_db(-6.0))
            .unwrap();
        let b = document
            .add_send(lead, TrackSend::new(room).pre_fader())
            .unwrap();
        document.send_mut(b).unwrap().state.enabled = false;

        let mut mixer = Mixer::new(2);
        let mut bridge = MixerBridge::new();
        bridge.bind(&document, &mut mixer);
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 2);
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
        document.send_mut(a).unwrap().state.level_db = -12.0;
        document.send_mut(b).unwrap().state.enabled = true;
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 0);
        assert!((mixer.send_params(ma).unwrap().level_db() - -12.0).abs() < 1e-4);
        assert!(mixer.send_params(mb).unwrap().is_enabled());
        document.send_mut(a).unwrap().state.level_db = -1.0;
        bridge.apply_strip_params(&document, &mixer);
        assert!((mixer.send_params(ma).unwrap().level_db() - -1.0).abs() < 1e-4);

        // Tap change re-creates the mixer send under the same document id.
        document.send_mut(b).unwrap().pre_fader = false;
        assert_eq!(
            bridge.apply_routing(&document, &mut mixer).unwrap(),
            2,
            "remove + add"
        );
        let mb2 = bridge.mixer_send(b).unwrap();
        assert_ne!(mb, mb2);
        assert!(mixer.send(mb).is_err());
        assert_eq!(mixer.send(mb2).unwrap().tap, SendTap::PostFader);
        assert_eq!(mixer.sends().len(), 2);

        // Removing the document send removes the mixer send.
        document.remove_send(a).unwrap();
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 1);
        assert!(bridge.mixer_send(a).is_none());
        assert!(mixer.send(ma).is_err());
        assert_eq!(mixer.sends().len(), 1);

        // Removing the destination bus drops the send in the document and
        // therefore in the mixer; the mixer bus itself stays.
        document.remove_track(room).unwrap();
        bridge.bind(&document, &mut mixer);
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 1);
        assert!(mixer.sends().is_empty());
        assert!(bridge.send_entries().is_empty());
        assert_eq!(mixer.buses().len(), 3);
    }

    #[test]
    fn apply_routing_tolerates_mixer_objects_removed_by_the_app() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let lead = document.add_track("lead", TrackKind::Audio);
        let fx = document.add_track("fx", TrackKind::Bus);
        document.set_track_output(lead, Some(fx)).unwrap();
        document.add_send(lead, TrackSend::new(fx)).unwrap();
        let mut mixer = Mixer::new(2);
        let mut bridge = MixerBridge::new();
        bridge.bind(&document, &mut mixer);
        bridge.apply_routing(&document, &mut mixer).unwrap();
        // The app tears down the bus and the strip behind the bridge's back.
        mixer.remove_bus(bridge.mixer_bus(fx).unwrap()).unwrap();
        mixer
            .remove_track(bridge.mixer_track(lead).unwrap())
            .unwrap();
        assert_eq!(bridge.apply_routing(&document, &mut mixer).unwrap(), 0);
        bridge.apply_strip_params(&document, &mixer);
    }
}
