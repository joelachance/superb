//! Sends: a copy of a track into a bus.
//!
//! A send taps a track's signal before or after its fader and feeds a bus
//! (a track of kind `Bus`, added with [`track::add`](crate::track::add);
//! route a whole track into one with [`track::route`](crate::track::route)).
//! Adding or removing a send changes the mixer's shape, so it rebuilds. A
//! send's level is an edit: into the document through the undo history,
//! coalescing, and onto the live send at once. Whether a send is on is
//! state: saved, heard at the next block, never an undo step.
//!
//! Made of `superb::primitives::session::commands` (the document side) and
//! `superb::primitives::mixer::SendParams` (the live side).

use crate::primitives::mixer::SendTap;
use crate::primitives::session::commands::{self, AddSend, RemoveSend};
use crate::primitives::session::TrackSend;
use crate::primitives::{Result, SendId, TrackId};
use crate::session::Session;

/// Adds a send from a track into a bus at `level_db`, tapped before or
/// after the fader (`SendTap::PreFader` / `SendTap::PostFader`).
///
/// Made of: `Document::allocate_send_id` → `TrackSend::with_id` → `AddSend`
/// through `session::structural`; rebuilds the mixer.
pub fn add(
    session: &mut Session,
    track: TrackId,
    bus: TrackId,
    level_db: f32,
    tap: SendTap,
) -> Result<SendId> {
    let id = session.document.allocate_send_id();
    let mut send = TrackSend::with_id(id, bus).level_db(level_db);
    if tap == SendTap::PreFader {
        send = send.pre_fader();
    }
    crate::session::structural(session, AddSend::new(track, send))?;
    Ok(id)
}

/// Removes a send.
///
/// Made of: `RemoveSend` through `session::structural`; rebuilds the mixer.
pub fn remove(session: &mut Session, send: SendId) -> Result<()> {
    crate::session::structural(session, RemoveSend::new(send))
}

/// Sets a send's level in dB. Moves within [`Session::COALESCE_WINDOW`] of
/// each other are one undo step.
///
/// Made of: `set_send_level_db` through `session::edit` →
/// `SendParams::set_level_db` on the live send.
pub fn level(session: &mut Session, send: SendId, db: f32) -> Result<()> {
    crate::session::edit(session, commands::set_send_level_db(send, db))?;
    if let Some(live) = session.mixer.send(send) {
        live.set_level_db(db);
    }
    Ok(())
}

/// Switches a send on or off. State, like mute: saved, heard at the next
/// block, not an undo step.
///
/// Made of: `SendState::enabled` through `session::set_state` →
/// `SendParams::set_enabled` on the live send.
pub fn enable(session: &mut Session, send: SendId, on: bool) -> Result<()> {
    crate::session::set_state(session, |s| {
        s.send_mut(send)?.state.enabled = on;
        Ok(())
    })?;
    if let Some(live) = session.mixer.send(send) {
        live.set_enabled(on);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use crate::session;
    use crate::track;
    use crate::transport;

    const R: SampleRate = SampleRate::HZ_48K;

    /// One audio track with a cached 0.5 tone covering 0..4_000 frames.
    fn tone_rig() -> (Session, TrackId) {
        let mut session = session::create("mix", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 4_000);
        tone.fill(0.5);
        session.cache.insert("tone.wav", tone);
        session::add_audio_clip(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_samples(4_000),
        )
        .unwrap();
        // Balance pan law on every strip keeps the arithmetic obvious.
        session::steps::rebuild(&mut session).unwrap();
        (session, vox)
    }

    /// Level of the last sample after enough blocks for ramps to settle.
    fn settled(session: &mut Session) -> f32 {
        let audio = session::steps::render_blocks(session, 4).unwrap();
        audio.channel(0)[audio.frames() - 1]
    }

    fn balance_all(session: &Session) {
        for (_, strip) in &session.mixer.strips {
            strip.set_pan_law(crate::primitives::mixer::PanLaw::Balance);
        }
    }

    #[test]
    fn fader_pan_mute_solo_reach_document_and_live_strip() {
        let (mut session, vox) = tone_rig();
        balance_all(&session);
        transport::start(&mut session).unwrap();
        assert!((settled(&mut session) - 0.5).abs() < 1e-5);
        let steps_before = session.history.undo_len();

        track::volume(&mut session, vox, -6.0206).unwrap();
        track::volume(&mut session, vox, -6.0206).unwrap(); // a drag: one step
        session.history.seal();
        assert!((session.document.track(vox).unwrap().state.volume_db + 6.0206).abs() < 1e-6);
        assert!(
            (settled(&mut session) - 0.25).abs() < 1e-3,
            "heard without a rebuild"
        );
        assert!(!session.needs_rebuild);

        track::pan(&mut session, vox, -1.0).unwrap();
        let audio = session::steps::render_blocks(&mut session, 4).unwrap();
        assert!((audio.channel(0)[255] - 0.25).abs() < 1e-3);
        assert!(
            audio.channel(1)[255].abs() < 1e-6,
            "hard left: right is silent"
        );
        assert_eq!(session.document.track(vox).unwrap().state.pan, -1.0);

        track::mute(&mut session, vox, true).unwrap();
        assert!(settled(&mut session).abs() < 1e-6);
        assert!(session.document.track(vox).unwrap().state.mute);
        track::mute(&mut session, vox, false).unwrap();

        let other = track::add(&mut session, "other", TrackKind::Audio).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        balance_all(&session);
        track::solo(&mut session, other, true).unwrap();
        assert!(
            settled(&mut session).abs() < 1e-6,
            "vox is silenced by the other solo"
        );
        track::solo(&mut session, other, false).unwrap();
        assert!(settled(&mut session).abs() > 0.2);

        // The gain drag (one step), pan and the added track are undo steps;
        // mute and solo are switches and never enter the history.
        assert_eq!(session.history.undo_len() - steps_before, 3);
        while session.history.undo_len() > steps_before {
            assert!(session::undo(&mut session).unwrap());
        }
        assert_eq!(session.document.track(vox).unwrap().state.volume_db, 0.0);
        assert_eq!(session.document.track(vox).unwrap().state.pan, 0.0);
    }

    #[test]
    fn buses_sends_and_routing() {
        let (mut session, vox) = tone_rig();
        transport::start(&mut session).unwrap();
        balance_all(&session);
        let direct = settled(&mut session);

        let room = track::add(&mut session, "room", TrackKind::Bus).unwrap();
        assert!(session.needs_rebuild);
        let send = add(&mut session, vox, room, 0.0, SendTap::PostFader).unwrap();
        assert_eq!(session.document.send(send).unwrap().bus(), room);
        assert!(!session.document.send(send).unwrap().pre_fader);
        session::steps::rebuild(&mut session).unwrap();
        balance_all(&session);
        assert!(
            (settled(&mut session) - direct * 2.0).abs() < 1e-3,
            "track + unity send"
        );

        level(&mut session, send, -6.0206).unwrap();
        assert!(
            (settled(&mut session) - direct * 1.5).abs() < 1e-3,
            "live send level"
        );
        let steps = session.history.undo_len();
        enable(&mut session, send, false).unwrap();
        assert!((settled(&mut session) - direct).abs() < 1e-3);
        assert!(!session.document.send(send).unwrap().state.enabled);
        assert_eq!(session.history.undo_len(), steps, "a switch, not an edit");
        assert!(!session::is_saved(&session), "but it does count as unsaved");

        // Route the track through the bus and turn the bus down.
        track::route(&mut session, vox, Some(room)).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        balance_all(&session);
        track::volume(&mut session, room, -6.0206).unwrap();
        assert!((settled(&mut session) - direct * 0.5).abs() < 1e-3);
        assert!(
            track::route(&mut session, room, Some(room)).is_err(),
            "a bus cannot feed itself"
        );

        remove(&mut session, send).unwrap();
        assert!(session.document.send(send).is_err());
        track::route(&mut session, vox, None).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        balance_all(&session);
        assert!((settled(&mut session) - direct).abs() < 1e-3);
    }
}
