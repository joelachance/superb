//! Shaping the sound: faders, pan, mute and solo, buses and sends, meters.
//!
//! A level change takes two paths at once: into the document through the
//! undo history, so it is saved and reversible, and onto the live strip's
//! atomics, so it is heard on the next block without a sync. Switches
//! (mute, solo, send on/off) take the same two paths but skip the history:
//! they are saved with the session, not undone. Changing the console's
//! shape (a new bus, a new send, a new route) is a structural edit like any
//! other and marks the rig for a [`play::sync`](crate::play::sync).
//!
//! Made of `superb::primitives::session::commands` (the document side) and
//! `superb::primitives::mixer` (`StripParams`, `SendParams`: the live side).

use crate::primitives::session::commands::{self, AddSend, RemoveSend, SetTrackOutput};
use crate::primitives::session::{TrackKind, TrackSend};
use crate::primitives::{Result, SendId, TrackId};
use crate::rig::Rig;

/// Sets a track's or bus's fader in dB. Moves within
/// [`Rig::COALESCE_WINDOW`] of each other are one undo step, so a fader
/// ride undoes in one go.
///
/// Made of: push `set_track_gain_db` → `StripParams::set_gain_db` on the live strip.
pub fn set_gain_db(rig: &mut Rig, track: TrackId, db: f32) -> Result<()> {
    rig.history
        .push(&mut rig.session, commands::set_track_gain_db(track, db))?;
    if let Some(strip) = rig.live.strip(track) {
        strip.set_gain_db(db);
    }
    Ok(())
}

/// Sets pan, `-1.0` (left) to `1.0` (right).
///
/// Made of: push `set_track_pan` → `StripParams::set_pan` on the live strip.
pub fn set_pan(rig: &mut Rig, track: TrackId, pan: f32) -> Result<()> {
    rig.history
        .push(&mut rig.session, commands::set_track_pan(track, pan))?;
    if let Some(strip) = rig.live.strip(track) {
        strip.set_pan(pan);
    }
    Ok(())
}

/// Mutes or unmutes. A switch: saved with the session, heard at the next
/// block, not an undo step.
///
/// Made of: `Track::muted` through `project::switch` → `StripParams::set_mute`
/// on the live strip.
pub fn set_mute(rig: &mut Rig, track: TrackId, on: bool) -> Result<()> {
    crate::project::switch(rig, |s| {
        s.track_mut(track)?.muted = on;
        Ok(())
    })?;
    if let Some(strip) = rig.live.strip(track) {
        strip.set_mute(on);
    }
    Ok(())
}

/// Solos or un-solos. While anything is soloed, every other track is
/// silent; buses keep passing what reaches them. A switch, like mute.
///
/// Made of: `Track::soloed` through `project::switch` → `StripParams::set_solo`
/// on the live strip.
pub fn set_solo(rig: &mut Rig, track: TrackId, on: bool) -> Result<()> {
    crate::project::switch(rig, |s| {
        s.track_mut(track)?.soloed = on;
        Ok(())
    })?;
    if let Some(strip) = rig.live.strip(track) {
        strip.set_solo(on);
    }
    Ok(())
}

/// Adds a bus. Route tracks into it with [`route`] and feed it with
/// [`add_send`].
///
/// Made of: [`project::add_track`](crate::project::add_track) with `TrackKind::Bus`.
pub fn add_bus(rig: &mut Rig, name: &str) -> Result<TrackId> {
    crate::project::add_track(rig, name, TrackKind::Bus)
}

/// Sends a track's (or bus's) output into `bus`, or back to the master
/// with `None`. Bus chains that would loop are refused.
///
/// Made of: push `SetTrackOutput`; marks the rig for a sync.
pub fn route(rig: &mut Rig, track: TrackId, bus: Option<TrackId>) -> Result<()> {
    rig.history
        .push(&mut rig.session, SetTrackOutput::new(track, bus))?;
    rig.needs_sync = true;
    Ok(())
}

/// Adds a send from a track into a bus at `level_db`, tapped before or
/// after the fader.
///
/// Made of: `Session::allocate_send_id` → `TrackSend::with_id` → push
/// `AddSend`; marks the rig for a sync.
pub fn add_send(
    rig: &mut Rig,
    track: TrackId,
    bus: TrackId,
    level_db: f32,
    pre_fader: bool,
) -> Result<SendId> {
    let id = rig.session.allocate_send_id();
    let mut send = TrackSend::with_id(id, bus).level_db(level_db);
    if pre_fader {
        send = send.pre_fader();
    }
    rig.history
        .push(&mut rig.session, AddSend::new(track, send))?;
    rig.needs_sync = true;
    Ok(id)
}

/// Removes a send.
///
/// Made of: push `RemoveSend`; marks the rig for a sync.
pub fn remove_send(rig: &mut Rig, send: SendId) -> Result<()> {
    rig.history.push(&mut rig.session, RemoveSend::new(send))?;
    rig.needs_sync = true;
    Ok(())
}

/// Sets a send's level in dB. Consecutive moves are one undo step.
///
/// Made of: push `set_send_level_db` → `SendParams::set_level_db` on the live send.
pub fn set_send_level_db(rig: &mut Rig, send: SendId, db: f32) -> Result<()> {
    rig.history
        .push(&mut rig.session, commands::set_send_level_db(send, db))?;
    if let Some(live) = rig.live.send(send) {
        live.set_level_db(db);
    }
    Ok(())
}

/// Switches a send on or off. A switch, like mute.
///
/// Made of: `TrackSend::enabled` through `project::switch` →
/// `SendParams::set_enabled` on the live send.
pub fn set_send_enabled(rig: &mut Rig, send: SendId, on: bool) -> Result<()> {
    crate::project::switch(rig, |s| {
        s.send_mut(send)?.enabled = on;
        Ok(())
    })?;
    if let Some(live) = rig.live.send(send) {
        live.set_enabled(on);
    }
    Ok(())
}

/// Reads and resets a strip's peak meter: the loudest sample per channel
/// since the last call, as linear amplitude. `None` before the first sync.
///
/// Made of: `StripParams::take_peak` for channels 0 and 1.
pub fn peak(rig: &Rig, track: TrackId) -> Option<[f32; 2]> {
    let strip = rig.live.strip(track)?;
    Some([strip.take_peak(0), strip.take_peak(1)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use crate::project;

    const R: SampleRate = SampleRate::HZ_48K;

    /// One audio track with a cached 0.5 tone covering 0..4_000 frames.
    fn tone_rig() -> (Rig, TrackId) {
        let mut rig = project::create("mix", R, 64).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 4_000);
        tone.fill(0.5);
        rig.cache.insert("tone.wav", tone);
        project::add_audio_clip(
            &mut rig,
            vox,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_samples(4_000),
        )
        .unwrap();
        // Balance pan law on every strip keeps the arithmetic obvious.
        play::sync(&mut rig).unwrap();
        (rig, vox)
    }

    /// Level of the last sample after enough blocks for ramps to settle.
    fn settled(rig: &mut Rig) -> f32 {
        let audio = play::advance(rig, 4).unwrap();
        audio.channel(0)[audio.frames() - 1]
    }

    fn balance_all(rig: &Rig) {
        for (_, strip) in &rig.live.strips {
            strip.set_pan_law(crate::primitives::mixer::PanLaw::Balance);
        }
    }

    #[test]
    fn fader_pan_mute_solo_reach_document_and_live_strip() {
        let (mut rig, vox) = tone_rig();
        balance_all(&rig);
        play::start(&mut rig).unwrap();
        assert!((settled(&mut rig) - 0.5).abs() < 1e-5);
        let steps_before = rig.history.undo_len();

        set_gain_db(&mut rig, vox, -6.0206).unwrap();
        set_gain_db(&mut rig, vox, -6.0206).unwrap(); // a drag: one step
        rig.history.seal();
        assert!((rig.session.track(vox).unwrap().gain_db + 6.0206).abs() < 1e-6);
        assert!(
            (settled(&mut rig) - 0.25).abs() < 1e-3,
            "heard without a sync"
        );
        assert!(!rig.needs_sync);

        set_pan(&mut rig, vox, -1.0).unwrap();
        let audio = play::advance(&mut rig, 4).unwrap();
        assert!((audio.channel(0)[255] - 0.25).abs() < 1e-3);
        assert!(
            audio.channel(1)[255].abs() < 1e-6,
            "hard left: right is silent"
        );
        assert_eq!(rig.session.track(vox).unwrap().pan, -1.0);

        set_mute(&mut rig, vox, true).unwrap();
        assert!(settled(&mut rig).abs() < 1e-6);
        assert!(rig.session.track(vox).unwrap().muted);
        set_mute(&mut rig, vox, false).unwrap();

        let other = project::add_track(&mut rig, "other", TrackKind::Audio).unwrap();
        play::sync(&mut rig).unwrap();
        balance_all(&rig);
        set_solo(&mut rig, other, true).unwrap();
        assert!(
            settled(&mut rig).abs() < 1e-6,
            "vox is silenced by the other solo"
        );
        set_solo(&mut rig, other, false).unwrap();
        assert!(settled(&mut rig).abs() > 0.2);

        // The gain drag (one step), pan and the added track are undo steps;
        // mute and solo are switches and never enter the history.
        assert_eq!(rig.history.undo_len() - steps_before, 3);
        while rig.history.undo_len() > steps_before {
            assert!(project::undo(&mut rig).unwrap());
        }
        assert_eq!(rig.session.track(vox).unwrap().gain_db, 0.0);
        assert_eq!(rig.session.track(vox).unwrap().pan, 0.0);
    }

    #[test]
    fn buses_sends_and_routing() {
        let (mut rig, vox) = tone_rig();
        play::start(&mut rig).unwrap();
        balance_all(&rig);
        let direct = settled(&mut rig);

        let room = add_bus(&mut rig, "room").unwrap();
        assert!(rig.needs_sync);
        let send = add_send(&mut rig, vox, room, 0.0, false).unwrap();
        assert_eq!(rig.session.send(send).unwrap().bus(), room);
        assert!(!rig.session.send(send).unwrap().pre_fader);
        play::sync(&mut rig).unwrap();
        balance_all(&rig);
        assert!(
            (settled(&mut rig) - direct * 2.0).abs() < 1e-3,
            "track + unity send"
        );

        set_send_level_db(&mut rig, send, -6.0206).unwrap();
        assert!(
            (settled(&mut rig) - direct * 1.5).abs() < 1e-3,
            "live send level"
        );
        let steps = rig.history.undo_len();
        set_send_enabled(&mut rig, send, false).unwrap();
        assert!((settled(&mut rig) - direct).abs() < 1e-3);
        assert!(!rig.session.send(send).unwrap().enabled);
        assert_eq!(rig.history.undo_len(), steps, "a switch, not an edit");
        assert!(!project::is_saved(&rig), "but it does count as unsaved");

        // Route the track through the bus and turn the bus down.
        route(&mut rig, vox, Some(room)).unwrap();
        play::sync(&mut rig).unwrap();
        balance_all(&rig);
        set_gain_db(&mut rig, room, -6.0206).unwrap();
        assert!((settled(&mut rig) - direct * 0.5).abs() < 1e-3);
        assert!(
            route(&mut rig, room, Some(room)).is_err(),
            "a bus cannot feed itself"
        );

        remove_send(&mut rig, send).unwrap();
        assert!(rig.session.send(send).is_err());
        route(&mut rig, vox, None).unwrap();
        play::sync(&mut rig).unwrap();
        balance_all(&rig);
        assert!((settled(&mut rig) - direct).abs() < 1e-3);
    }

    #[test]
    fn meters_report_and_reset() {
        let (mut rig, vox) = tone_rig();
        assert!(peak(&rig, TrackId::from_raw(99)).is_none());
        play::start(&mut rig).unwrap();
        play::advance(&mut rig, 4).unwrap();
        let [l, r] = peak(&rig, vox).unwrap();
        assert!(l > 0.3 && r > 0.3, "{l} {r}");
        // Reading resets the hold; nothing rendered since, so zero.
        assert_eq!(peak(&rig, vox).unwrap(), [0.0, 0.0]);
    }
}
