//! Tracks: add them, name them, set their state, read their meters.
//!
//! A track's *state* (mute, solo, arm, monitor, input, volume, pan, where
//! its output goes) is what the strip shows. Switches (mute, solo, arm,
//! monitor) are saved with the session, heard at the next block and never
//! undo steps. Volume and pan are edits: they coalesce in the undo history
//! and reach the live strip at once. Adding, removing and routing change
//! the mixer's shape, so they rebuild it (at once while a device is
//! playing, otherwise at the next start).
//!
//! Made of `superb::primitives::session` (the memory's tracks and their
//! commands) and `superb::primitives::mixer` (`StripParams`, the live side).

use crate::primitives::session::commands::{self, AddTrack, RemoveTrack, SetTrackOutput};
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

/// Removes a track with everything on it. Undo brings it all back.
///
/// Made of: push `RemoveTrack`; rebuilds the mixer.
pub fn remove(session: &mut Session, track: TrackId) -> Result<()> {
    crate::session::structural(session, RemoveTrack::new(track))
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
        .filter(|t| t.kind == TrackKind::Audio && t.state.arm)
        .map(|t| t.id())
        .collect()
}

/// Turns live monitoring on or off for a MIDI track. While on, the track
/// carries an instrument that plays whatever MIDI reaches the engine.
/// State, saved as the track's arm flag; it changes the mixer's shape, so
/// it rebuilds.
///
/// Made of: `TrackState::arm` through `session::set_state` → rebuild (the
/// instrument is a node in the mixer).
pub fn monitor(session: &mut Session, track: TrackId, on: bool) -> Result<()> {
    if session.memory.track(track)?.kind != TrackKind::Midi {
        return Err(SuperbError::InvalidArgument {
            what: "only MIDI tracks monitor live MIDI",
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
}
