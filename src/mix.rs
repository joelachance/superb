//! Shaping the sound: faders, pan, mute and solo, buses and sends, meters.
//!
//! A level change takes two paths at once: into the document through the
//! undo history, so it is saved and reversible, and onto the live strip's
//! atomics, so it is heard on the next block without a rebuild. Switches
//! (mute, solo, send on/off) take the same two paths but skip the history:
//! they are saved with the document, not undone. Changing the console's
//! shape (a new bus, a new send, a new route) is a structural edit like any
//! other and marks the session for a [`session::steps::rebuild`](crate::session::steps::rebuild).
//!
//! Made of `superb::primitives::session::commands` (the document side) and
//! `superb::primitives::mixer` (`StripParams`, `SendParams`: the live side).

use crate::primitives::mixer::SendTap;
use crate::primitives::session::commands::{self, AddSend, RemoveSend};
use crate::primitives::session::{TrackKind, TrackSend};
use crate::primitives::{Result, SendId, TrackId};
use crate::session::Session;

/// Adds a bus. Route tracks into it with [`track::route`](crate::track::route)
/// and feed it with [`add_send`].
///
/// Made of: [`track::add`](crate::track::add) with `TrackKind::Bus`.
pub fn add_bus(session: &mut Session, name: &str) -> Result<TrackId> {
    crate::track::add(session, name, TrackKind::Bus)
}

/// Adds a send from a track into a bus at `level_db`, tapped before or
/// after the fader (`SendTap::PreFader` / `SendTap::PostFader`).
///
/// Made of: `Document::allocate_send_id` → `TrackSend::with_id` → push
/// `AddSend`; marks the session for a rebuild.
pub fn add_send(
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
    session
        .history
        .push(&mut session.document, AddSend::new(track, send))?;
    session.needs_rebuild = true;
    Ok(id)
}

/// Removes a send.
///
/// Made of: push `RemoveSend`; marks the session for a rebuild.
pub fn remove_send(session: &mut Session, send: SendId) -> Result<()> {
    session
        .history
        .push(&mut session.document, RemoveSend::new(send))?;
    session.needs_rebuild = true;
    Ok(())
}

/// Sets a send's level in dB. Consecutive moves are one undo step.
///
/// Made of: push `set_send_level_db` → `SendParams::set_level_db` on the live send.
pub fn set_send_level_db(session: &mut Session, send: SendId, db: f32) -> Result<()> {
    session
        .history
        .push(&mut session.document, commands::set_send_level_db(send, db))?;
    if let Some(live) = session.live.send(send) {
        live.set_level_db(db);
    }
    Ok(())
}

/// Switches a send on or off. A switch, like mute.
///
/// Made of: `TrackSend::enabled` through `session::switch` →
/// `SendParams::set_enabled` on the live send.
pub fn set_send_enabled(session: &mut Session, send: SendId, on: bool) -> Result<()> {
    crate::session::switch(session, |s| {
        s.send_mut(send)?.enabled = on;
        Ok(())
    })?;
    if let Some(live) = session.live.send(send) {
        live.set_enabled(on);
    }
    Ok(())
}

/// Reads and resets a strip's peak meter: the loudest sample per channel
/// since the last call, as linear amplitude. `None` before the first rebuild.
///
/// Made of: `StripParams::take_peak` for channels 0 and 1.
pub fn peak(session: &Session, track: TrackId) -> Option<[f32; 2]> {
    let strip = session.live.strip(track)?;
    Some([strip.take_peak(0), strip.take_peak(1)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
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
        for (_, strip) in &session.live.strips {
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
        assert!((session.document.track(vox).unwrap().gain_db + 6.0206).abs() < 1e-6);
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
        assert_eq!(session.document.track(vox).unwrap().pan, -1.0);

        track::mute(&mut session, vox, true).unwrap();
        assert!(settled(&mut session).abs() < 1e-6);
        assert!(session.document.track(vox).unwrap().muted);
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
        assert_eq!(session.document.track(vox).unwrap().gain_db, 0.0);
        assert_eq!(session.document.track(vox).unwrap().pan, 0.0);
    }

    #[test]
    fn buses_sends_and_routing() {
        let (mut session, vox) = tone_rig();
        transport::start(&mut session).unwrap();
        balance_all(&session);
        let direct = settled(&mut session);

        let room = add_bus(&mut session, "room").unwrap();
        assert!(session.needs_rebuild);
        let send = add_send(&mut session, vox, room, 0.0, SendTap::PostFader).unwrap();
        assert_eq!(session.document.send(send).unwrap().bus(), room);
        assert!(!session.document.send(send).unwrap().pre_fader);
        session::steps::rebuild(&mut session).unwrap();
        balance_all(&session);
        assert!(
            (settled(&mut session) - direct * 2.0).abs() < 1e-3,
            "track + unity send"
        );

        set_send_level_db(&mut session, send, -6.0206).unwrap();
        assert!(
            (settled(&mut session) - direct * 1.5).abs() < 1e-3,
            "live send level"
        );
        let steps = session.history.undo_len();
        set_send_enabled(&mut session, send, false).unwrap();
        assert!((settled(&mut session) - direct).abs() < 1e-3);
        assert!(!session.document.send(send).unwrap().enabled);
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

        remove_send(&mut session, send).unwrap();
        assert!(session.document.send(send).is_err());
        track::route(&mut session, vox, None).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        balance_all(&session);
        assert!((settled(&mut session) - direct).abs() < 1e-3);
    }

    #[test]
    fn meters_report_and_reset() {
        let (mut session, vox) = tone_rig();
        assert!(peak(&session, TrackId::from_raw(99)).is_none());
        transport::start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 4).unwrap();
        let [l, r] = peak(&session, vox).unwrap();
        assert!(l > 0.3 && r > 0.3, "{l} {r}");
        // Reading resets the hold; nothing rendered since, so zero.
        assert_eq!(peak(&session, vox).unwrap(), [0.0, 0.0]);
    }
}
