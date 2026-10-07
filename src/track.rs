//! Tracks: add them, name them, set their state, read their meters, archive
//! them.
//!
//! A track's *state* (mute, solo, arm, monitor, input, volume, pan, where
//! its output goes) is what the strip shows. Monitoring is hearing the
//! track's input live: MIDI through an instrument, audio through the
//! session's audio input. Switches (mute, solo, arm,
//! monitor) are saved with the session, heard at the next block and never
//! undo steps. Volume and pan are edits: they coalesce in the undo history
//! and reach the live strip at once. Adding, archiving and routing change
//! the mixer's shape, so they rebuild it (at once while a device is
//! playing, otherwise at the next start).
//!
//! Made of `superb::primitives::session` (the memory's tracks and their
//! commands) and `superb::primitives::mixer` (`StripParams`, the live side).

use crate::primitives::session::commands::{self, AddTrack, ArchiveTrack, SetTrackOutput};
use crate::primitives::session::{Input, Track, TrackKind};
use crate::primitives::{Result, SuperbError, TrackId};
use crate::session::Session;

/// Adds a track at the end. `TrackKind::Bus` makes a bus.
///
/// Made of: `Memory::allocate_track_id` → `Track::with_id` → push
/// `AddTrack`; rebuilds the mixer.
pub fn add(session: &mut Session, name: &str, kind: TrackKind) -> Result<TrackId> {
    let id = session.memory.allocate_track_id();
    crate::session::structural(session, AddTrack::new(Track::with_id(id, name, kind)))?;
    Ok(id)
}

/// Takes a track off the mixer without deleting it: it keeps its clips and
/// their files but gets no strip, so nothing on it plays, nothing routes
/// into it, it neither records nor monitors, and it no longer counts
/// toward the session's end. [`restore`] brings it back. Nothing is ever
/// removed: this is the one way a track leaves.
///
/// Made of: `ArchiveTrack` through `session::structural`.
pub fn archive(session: &mut Session, track: TrackId) -> Result<()> {
    not_master(session, track, "the master is not archived")?;
    crate::session::structural(session, ArchiveTrack::new(track, true))
}

/// Puts an archived track back on the mixer.
///
/// Made of: `ArchiveTrack` (restoring) through `session::structural`.
pub fn restore(session: &mut Session, track: TrackId) -> Result<()> {
    not_master(session, track, "the master is not archived")?;
    crate::session::structural(session, ArchiveTrack::new(track, false))
}

/// The master is a track for volume, pan, mute and metering, and nothing
/// else.
fn not_master(session: &Session, track: TrackId, what: &'static str) -> Result<()> {
    if session.memory.is_master(track) {
        return Err(SuperbError::InvalidArgument { what });
    }
    Ok(())
}

/// Renames a track. Quick successive renames of one track are one undo step.
///
/// Made of: push `set_track_name`.
pub fn rename(session: &mut Session, track: TrackId, name: &str) -> Result<()> {
    crate::session::edit(session, commands::set_track_name(track, name))
}

/// Sets a track's or bus's volume: the fader, in dB. Moves within
/// [`Session::COALESCE_WINDOW`] of each other are one undo step, so a
/// fader ride undoes in one go.
///
/// Made of: push `set_track_volume_db` → `StripParams::set_volume_db` on the
/// live strip.
pub fn volume(session: &mut Session, track: TrackId, db: f32) -> Result<()> {
    crate::session::edit(session, commands::set_track_volume_db(track, db))?;
    if let Some(strip) = session.mixer.strip(track) {
        strip.set_volume_db(db);
    }
    Ok(())
}

/// Sets pan, `-1.0` (left) to `1.0` (right). An edit, like volume.
///
/// Made of: push `set_track_pan` → `StripParams::set_pan` on the live strip.
pub fn pan(session: &mut Session, track: TrackId, pan: f32) -> Result<()> {
    crate::session::edit(session, commands::set_track_pan(track, pan))?;
    if let Some(strip) = session.mixer.strip(track) {
        strip.set_pan(pan);
    }
    Ok(())
}

/// Mutes or unmutes. State: saved with the session, heard at the next
/// block, not an undo step.
///
/// Made of: `TrackState::mute` through `session::set_state` →
/// `StripParams::set_mute` on the live strip.
pub fn mute(session: &mut Session, track: TrackId, on: bool) -> Result<()> {
    crate::session::set_state(session, |d| {
        d.track_mut(track)?.state.mute = on;
        Ok(())
    })?;
    if let Some(strip) = session.mixer.strip(track) {
        strip.set_mute(on);
    }
    Ok(())
}

/// Solos or un-solos. While anything is soloed, every other track is
/// silent; buses keep passing what reaches them. State, like mute.
///
/// Made of: `TrackState::solo` through `session::set_state` →
/// `StripParams::set_solo` on the live strip.
pub fn solo(session: &mut Session, track: TrackId, on: bool) -> Result<()> {
    not_master(
        session,
        track,
        "the master is not soloed; it is what solo leaves audible",
    )?;
    crate::session::set_state(session, |d| {
        d.track_mut(track)?.state.solo = on;
        Ok(())
    })?;
    if let Some(strip) = session.mixer.strip(track) {
        strip.set_solo(on);
    }
    Ok(())
}

/// Arms or disarms an audio track for recording. Armed tracks receive a
/// clip when a recording stops. State, like mute. MIDI tracks
/// [`monitor`] instead.
///
/// Made of: `TrackState::arm` through `session::set_state`.
pub fn arm(session: &mut Session, track: TrackId, on: bool) -> Result<()> {
    if session.memory.track(track)?.kind != TrackKind::Audio {
        return Err(SuperbError::InvalidArgument {
            what: "only audio tracks are armed for recording",
        });
    }
    crate::session::set_state(session, |d| {
        d.track_mut(track)?.state.arm = on;
        Ok(())
    })
}

/// The audio tracks currently armed, in track order.
pub(crate) fn armed(session: &Session) -> Vec<TrackId> {
    session
        .memory
        .tracks()
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && t.state.arm && !t.state.archived)
        .map(|t| t.id())
        .collect()
}

/// Turns monitoring on or off. A monitoring MIDI track carries a live
/// instrument that plays whatever MIDI reaches the engine. A monitoring
/// audio track plays the session's audio input (`devices::input`, feature
/// `devices`) through its strip, on the channel(s) its [`input`] names (the
/// first two when none is set), so you hear yourself while recording.
/// State, saved with the session; it changes the mixer's shape, so it
/// rebuilds. Buses do not monitor.
///
/// Made of: `TrackState::monitor` through `session::set_state` → rebuild
/// (the instrument or the input tap is a node in the mixer).
pub fn monitor(session: &mut Session, track: TrackId, on: bool) -> Result<()> {
    if session.memory.track(track)?.kind == TrackKind::Bus {
        return Err(SuperbError::InvalidArgument {
            what: "buses do not monitor",
        });
    }
    crate::session::set_state(session, |d| {
        d.track_mut(track)?.state.monitor = on;
        Ok(())
    })?;
    crate::session::changed_structure(session)
}

/// Chooses which channel(s) of the audio input feed an audio track when it
/// records: `Input::Mono(0)` for the first input, `Input::Stereo(0, 1)` for
/// the first pair, `None` for everything the recording captures. State:
/// saved with the session, not an undo step. `record::start` captures as
/// many channels as the armed tracks' inputs need.
///
/// Made of: `TrackState::input` through `session::set_state`.
pub fn input(session: &mut Session, track: TrackId, input: Option<Input>) -> Result<()> {
    if session.memory.track(track)?.kind != TrackKind::Audio {
        return Err(SuperbError::InvalidArgument {
            what: "only audio tracks take an input",
        });
    }
    crate::session::set_state(session, |d| {
        d.track_mut(track)?.state.input = input;
        Ok(())
    })
}

/// Sends a track's (or bus's) output into `bus`, or back to the master
/// with `None`. Bus chains that would loop are refused.
///
/// Made of: push `SetTrackOutput`; rebuilds the mixer.
pub fn route(session: &mut Session, track: TrackId, bus: Option<TrackId>) -> Result<()> {
    crate::session::structural(session, SetTrackOutput::new(track, bus))
}

/// Reads and resets a track's or bus's peak meter: the loudest sample per
/// channel since the last call, as linear amplitude. `None` before the
/// first rebuild.
///
/// Made of: `StripParams::take_peak` for channels 0 and 1 on the live strip.
pub fn peak(session: &Session, track: TrackId) -> Option<[f32; 2]> {
    let strip = session.mixer.strip(track)?;
    Some([strip.take_peak(0), strip.take_peak(1)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::{SampleRate, TimePosition};
    use crate::{session, transport};

    #[test]
    fn meters_report_and_reset() {
        let mut session =
            session::create(session::scratch("meters"), SampleRate::HZ_48K, 64).unwrap();
        let vox = add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 4_000);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        crate::clip::add(&mut session, vox, "tone.wav", TimePosition::ZERO).unwrap();
        assert!(peak(&session, vox).is_none(), "no mixer yet");
        transport::start(&mut session).unwrap();
        assert!(peak(&session, TrackId::from_raw(99)).is_none());
        session::steps::render_blocks(&mut session, 4).unwrap();
        let [l, r] = peak(&session, vox).unwrap();
        assert!(l > 0.3 && r > 0.3, "{l} {r}");
        // Reading resets the hold; nothing rendered since, so zero.
        assert_eq!(peak(&session, vox).unwrap(), [0.0, 0.0]);
    }

    #[test]
    fn an_archived_track_has_no_strip_and_comes_back_whole() {
        use crate::primitives::session::{Memory, TrackSend};
        use crate::{clip, send};
        let mut session =
            session::create(session::scratch("archive"), SampleRate::HZ_48K, 64).unwrap();
        let vox = add(&mut session, "vox", TrackKind::Audio).unwrap();
        let room = add(&mut session, "room", TrackKind::Bus).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        let take = clip::add(&mut session, vox, "tone.wav", TimePosition::ZERO).unwrap();
        let fx = send::add(
            &mut session,
            vox,
            room,
            0.0,
            crate::primitives::mixer::SendTap::PostFader,
        )
        .unwrap();
        arm(&mut session, vox, true).unwrap();
        transport::start(&mut session).unwrap();
        assert!(
            session::steps::render_blocks(&mut session, 2)
                .unwrap()
                .peak()
                > 0.3
        );
        assert_eq!(session.mixer.sends.len(), 1);

        // Archive the bus: the send into it is skipped; the track still plays.
        archive(&mut session, room).unwrap();
        transport::rewind(&mut session).unwrap();
        let out = session::steps::render_blocks(&mut session, 2).unwrap();
        assert!(out.peak() > 0.3);
        assert!(session.mixer.strip(room).is_none());
        assert!(session.mixer.send(fx).is_none());
        assert!(session.memory.track(room).is_ok(), "still in the memory");
        assert!(session.memory.send(fx).is_ok());

        // Archive the track: silent, no strip, not armed, not counted.
        archive(&mut session, vox).unwrap();
        transport::rewind(&mut session).unwrap();
        assert!(session::steps::render_blocks(&mut session, 2)
            .unwrap()
            .is_silent());
        assert!(session.mixer.strip(vox).is_none());
        assert!(armed(&session).is_empty());
        assert_eq!(session.memory.end_position().samples(), 0);
        assert!(session.memory.clip(take).is_ok(), "the clip is still there");
        assert!(session.memory.track(vox).unwrap().state.archived);

        // Restore, and undo, each one step.
        let steps = session.history.undo_len();
        restore(&mut session, vox).unwrap();
        assert_eq!(session.history.undo_len(), steps + 1);
        transport::rewind(&mut session).unwrap();
        assert!(
            session::steps::render_blocks(&mut session, 2)
                .unwrap()
                .peak()
                > 0.3
        );
        assert_eq!(armed(&session), [vox]);
        assert!(session::undo(&mut session).unwrap());
        assert!(session.memory.track(vox).unwrap().state.archived);

        // Archived survives a save.
        let json = session.memory.to_json().unwrap();
        let copy = Memory::from_json(&json).unwrap();
        assert!(copy.track(vox).unwrap().state.archived);
        assert!(!copy.track(vox).unwrap().sends().is_empty());
        let _ = TrackSend::new(room);
    }

    #[test]
    fn the_master_takes_volume_and_mute_and_refuses_the_rest() {
        let mut session =
            session::create(session::scratch("master"), SampleRate::HZ_48K, 64).unwrap();
        let master = session.memory.master().id();
        let vox = add(&mut session, "vox", TrackKind::Audio).unwrap();
        volume(&mut session, master, -6.0).unwrap();
        mute(&mut session, master, true).unwrap();
        pan(&mut session, master, 0.25).unwrap();
        transport::start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 1).unwrap();
        let strip = session
            .mixer
            .strip(master)
            .expect("the master has a handle");
        assert_eq!(strip.volume_db(), -6.0);
        assert!(strip.is_muted());
        assert!(peak(&session, master).is_some());
        assert!(archive(&mut session, master).is_err());
        assert!(solo(&mut session, master, true).is_err());
        assert!(route(&mut session, master, Some(vox)).is_err());
        assert!(arm(&mut session, master, true).is_err(), "a bus");
        assert!(crate::send::add(
            &mut session,
            vox,
            master,
            -6.0,
            crate::primitives::mixer::SendTap::PostFader
        )
        .is_err());
        // Routing a track "to the master" is the default and stays `None`.
        route(&mut session, vox, Some(master)).unwrap();
        assert_eq!(session.memory.track(vox).unwrap().output(), None);
    }
}
