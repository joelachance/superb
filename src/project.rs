//! The song as a document: create it, open and save it, and edit it with
//! undo.
//!
//! Every edit here goes through the undo history, so [`undo`] and [`redo`]
//! always work and a save can tell whether anything changed. Edits that
//! change what the console has to look like (tracks, clips, routing) mark
//! the rig as needing a [`play::sync`](crate::play::sync); the flows that
//! start audio run it for you, or call it yourself to hear a change now.
//!
//! Made of `superb::primitives::session` (the document and its commands)
//! and `superb::primitives::undo` (the history). Each function below lists
//! the primitive calls it makes.

use std::path::{Path, PathBuf};

use crate::primitives::io::live::EngineCommand;
use crate::primitives::session::commands::{
    self, AddClip, AddMarker, AddTrack, MoveClip, RemoveClip, RemoveTrack, SetLoopRange, SetTempo,
};
use crate::primitives::session::{Clip, ClipSource, Marker, MidiNote, Session, Track, TrackKind};
use crate::primitives::transport::LoopRange;
use crate::primitives::undo::Command;
use crate::primitives::{
    BeatPosition, ClipId, MarkerId, Result, SampleRate, TimeDuration, TimePosition, TrackId,
};
use crate::rig::Rig;

/// Starts a new, empty song at 120 bpm in 4/4.
///
/// Made of: `Session::new` → `Rig::new`.
pub fn create(name: &str, sample_rate: SampleRate, block_size: usize) -> Result<Rig> {
    Rig::new(Session::new(name, sample_rate), block_size)
}

/// Opens a song saved with [`save`]. The undo history starts empty and
/// clean.
///
/// Made of: `Session::load` → `Rig::new`.
pub fn open(path: impl AsRef<Path>, block_size: usize) -> Result<Rig> {
    Rig::new(Session::load(path)?, block_size)
}

/// Writes the song as JSON and remembers this state as saved.
///
/// Made of: `Session::save` → `UndoStack::mark_clean`.
pub fn save(rig: &mut Rig, path: impl AsRef<Path>) -> Result<()> {
    rig.session.save(path)?;
    rig.history.mark_clean();
    Ok(())
}

/// `true` if nothing changed since the last [`save`] (or since opening).
/// Switches flipped since (mute, arm, bypass...) count as changes even
/// though they are not undo steps.
pub fn is_saved(rig: &Rig) -> bool {
    rig.history.is_clean()
}

/// Adds a track at the end. `TrackKind::Bus` makes a bus.
///
/// Made of: `Session::allocate_track_id` → `Track::with_id` → push `AddTrack`.
pub fn add_track(rig: &mut Rig, name: &str, kind: TrackKind) -> Result<TrackId> {
    let id = rig.session.allocate_track_id();
    structural(rig, AddTrack::new(Track::with_id(id, name, kind)))?;
    Ok(id)
}

/// Removes a track with everything on it. Undo brings it all back.
///
/// Made of: push `RemoveTrack`.
pub fn remove_track(rig: &mut Rig, track: TrackId) -> Result<()> {
    structural(rig, RemoveTrack::new(track))
}

/// Renames a track. Consecutive renames of the same track are one undo step.
///
/// Made of: push `set_track_name`.
pub fn rename_track(rig: &mut Rig, track: TrackId, name: &str) -> Result<()> {
    edit(rig, commands::set_track_name(track, name))
}

/// Places audio from the cache on a track. `take` is the key the audio was
/// cached under (see [`Rig::cache`]); the clip plays from the start of it.
///
/// Made of: `Session::allocate_clip_id` → `Clip::with_id` → push `AddClip`.
pub fn add_audio_clip(
    rig: &mut Rig,
    track: TrackId,
    take: impl Into<PathBuf>,
    start: TimePosition,
    length: TimeDuration,
) -> Result<ClipId> {
    let take = take.into();
    let name = take
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    let id = rig.session.allocate_clip_id();
    let clip = Clip::with_id(
        id,
        name,
        start,
        length,
        ClipSource::Audio {
            path: take,
            offset: TimeDuration::ZERO,
        },
    )?;
    structural(rig, AddClip::new(track, clip))?;
    Ok(id)
}

/// Places a MIDI clip holding `notes` (beats relative to the clip start).
///
/// Made of: `Session::allocate_clip_id` → `Clip::with_id` → push `AddClip`.
pub fn add_midi_clip(
    rig: &mut Rig,
    track: TrackId,
    name: &str,
    start: TimePosition,
    length: TimeDuration,
    notes: Vec<MidiNote>,
) -> Result<ClipId> {
    let id = rig.session.allocate_clip_id();
    let clip = Clip::with_id(id, name, start, length, ClipSource::Midi { notes })?;
    structural(rig, AddClip::new(track, clip))?;
    Ok(id)
}

/// Moves a clip to start at `start`. Moves of the same clip within
/// [`Rig::COALESCE_WINDOW`] of each other are one undo step, so a drag
/// undoes in one go.
///
/// Made of: push `MoveClip`.
pub fn move_clip(rig: &mut Rig, clip: ClipId, start: TimePosition) -> Result<()> {
    structural(rig, MoveClip::new(clip, start))
}

/// Removes a clip.
///
/// Made of: push `RemoveClip`.
pub fn remove_clip(rig: &mut Rig, clip: ClipId) -> Result<()> {
    structural(rig, RemoveClip::new(clip))
}

/// Drops a named marker on the timeline.
///
/// Made of: `Session::allocate_marker_id` → `Marker::with_id` → push `AddMarker`.
pub fn add_marker(rig: &mut Rig, name: &str, at: TimePosition) -> Result<MarkerId> {
    let id = rig.session.allocate_marker_id();
    edit(rig, AddMarker::new(Marker::with_id(id, name, at)))?;
    Ok(id)
}

/// Sets the song's tempo at the start. Consecutive changes are one undo step.
///
/// Made of: push `SetTempo` at beat 0.
pub fn set_tempo(rig: &mut Rig, bpm: f64) -> Result<()> {
    edit(rig, SetTempo::new(BeatPosition::ZERO, bpm))
}

/// Sets or clears the loop range in the song and on the running engine.
/// Enable looping with [`play::set_loop_enabled`](crate::play::set_loop_enabled).
///
/// Made of: push `SetLoopRange` → `LiveEngine::send(SetLoop)`.
pub fn set_loop(rig: &mut Rig, range: Option<LoopRange>) -> Result<()> {
    edit(rig, SetLoopRange::new(range))?;
    rig.engine.send(EngineCommand::SetLoop(range))
}

/// Reverts the last edit. Returns `false` if there was nothing to undo.
///
/// Made of: `UndoStack::undo`; the console is marked for a sync because the
/// undone edit may have been structural.
pub fn undo(rig: &mut Rig) -> Result<bool> {
    let done = rig.history.undo(&mut rig.session)?;
    if done {
        rig.needs_sync = true;
    }
    Ok(done)
}

/// Re-applies the last undone edit.
///
/// Made of: `UndoStack::redo`, then the same sync marking as [`undo`].
pub fn redo(rig: &mut Rig) -> Result<bool> {
    let done = rig.history.redo(&mut rig.session)?;
    if done {
        rig.needs_sync = true;
    }
    Ok(done)
}

/// Decodes an audio file, converts it to the song's sample rate, caches it
/// and places a clip of its full length at `at`.
///
/// Made of: `AudioCache::load` → [`add_audio_clip`].
#[cfg(feature = "files")]
pub fn import_audio(
    rig: &mut Rig,
    track: TrackId,
    path: impl AsRef<Path>,
    at: TimePosition,
) -> Result<ClipId> {
    let path = path.as_ref();
    let audio = rig.cache.load(path, rig.session.sample_rate())?;
    let length = TimeDuration::from_samples(audio.frames() as i64);
    add_audio_clip(rig, track, path.to_path_buf(), at, length)
}

/// Pushes an edit that does not change the console's structure.
pub(crate) fn edit(rig: &mut Rig, command: impl Command<Session>) -> Result<()> {
    rig.history.push(&mut rig.session, command)
}

/// Pushes an edit that changes what the console must look like.
pub(crate) fn structural(rig: &mut Rig, command: impl Command<Session>) -> Result<()> {
    edit(rig, command)?;
    rig.needs_sync = true;
    Ok(())
}

/// Flips a switch (mute, solo, arm, monitor, bypass, send on/off) on the
/// document. Switches are saved with the session but are not edits: they
/// never enter the undo history, so the history is told its saved state is
/// gone and [`is_saved`] turns false.
pub(crate) fn switch(rig: &mut Rig, flip: impl FnOnce(&mut Session) -> Result<()>) -> Result<()> {
    flip(&mut rig.session)?;
    rig.history.mark_dirty();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    const R: SampleRate = SampleRate::HZ_48K;

    #[test]
    fn create_edit_undo_redo() {
        let mut rig = create("demo", R, 64).unwrap();
        assert!(is_saved(&rig));
        assert!(rig.needs_sync, "a fresh rig has no console yet");
        rig.needs_sync = false;

        let vox = add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        assert!(rig.needs_sync);
        rename_track(&mut rig, vox, "vocals").unwrap();
        assert_eq!(rig.session.track(vox).unwrap().name, "vocals");
        let clip = add_audio_clip(
            &mut rig,
            vox,
            "takes/take 1.wav",
            TimePosition::from_samples(100),
            TimeDuration::from_samples(50),
        )
        .unwrap();
        assert_eq!(rig.session.clip(clip).unwrap().name, "take 1");
        move_clip(&mut rig, clip, TimePosition::from_samples(200)).unwrap();
        move_clip(&mut rig, clip, TimePosition::from_samples(300)).unwrap();
        rig.history.seal(); // "mouse up"; a pause would do the same
        let marker = add_marker(&mut rig, "verse", TimePosition::ZERO).unwrap();
        set_tempo(&mut rig, 90.0).unwrap();
        assert_eq!(
            rig.history.undo_len(),
            6,
            "the drag coalesced into one step"
        );
        assert!(!is_saved(&rig));

        assert!(undo(&mut rig).unwrap()); // tempo
        assert!(undo(&mut rig).unwrap()); // marker
        assert!(rig.session.marker(marker).is_err());
        assert!(undo(&mut rig).unwrap()); // whole drag
        assert_eq!(rig.session.clip(clip).unwrap().start().samples(), 100);
        assert!(redo(&mut rig).unwrap());
        assert_eq!(rig.session.clip(clip).unwrap().start().samples(), 300);
        remove_clip(&mut rig, clip).unwrap();
        assert!(rig.session.clip(clip).is_err());
        remove_track(&mut rig, vox).unwrap();
        assert!(rig.session.track(vox).is_err());
        while undo(&mut rig).unwrap() {}
        assert!(rig.session.track(vox).is_err(), "back to the empty song");
        assert!(!undo(&mut rig).unwrap());
        assert!(is_saved(&rig));
    }

    #[test]
    fn a_pause_between_edits_starts_a_new_undo_step() {
        use std::time::Instant;
        let mut rig = create("demo", R, 64).unwrap();
        let vox = add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let steps = rig.history.undo_len();
        let t0 = Instant::now();
        // Three moves in quick succession: one step.
        for (i, db) in [-1.0, -2.0, -3.0].iter().enumerate() {
            rig.history
                .push_at(
                    &mut rig.session,
                    Box::new(commands::set_track_gain_db(vox, *db)),
                    t0 + Duration::from_millis(50 * i as u64),
                )
                .unwrap();
        }
        assert_eq!(rig.history.undo_len(), steps + 1);
        // A move after a pause longer than the window: a new step.
        rig.history
            .push_at(
                &mut rig.session,
                Box::new(commands::set_track_gain_db(vox, -4.0)),
                t0 + Rig::COALESCE_WINDOW + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(rig.history.undo_len(), steps + 2);
        assert!(undo(&mut rig).unwrap());
        assert_eq!(rig.session.track(vox).unwrap().gain_db, -3.0);
    }

    #[test]
    fn midi_clips_and_loop_range() {
        let mut rig = create("demo", R, 64).unwrap();
        let keys = add_track(&mut rig, "keys", TrackKind::Midi).unwrap();
        let notes = vec![MidiNote {
            beat: 0.0,
            length: 1.0,
            note: 60,
            velocity: 100,
            channel: 0,
        }];
        let clip = add_midi_clip(
            &mut rig,
            keys,
            "chord",
            TimePosition::ZERO,
            TimeDuration::from_samples(48_000),
            notes,
        )
        .unwrap();
        assert!(matches!(
            rig.session.clip(clip).unwrap().source,
            ClipSource::Midi { .. }
        ));
        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(1_000)).unwrap();
        set_loop(&mut rig, Some(range)).unwrap();
        assert_eq!(rig.session.loop_range(), Some(range));
        // The engine received the range too: apply it on the loop side.
        rig.engine_loop.as_mut().unwrap().apply_commands();
        assert_eq!(
            rig.engine_loop
                .as_ref()
                .unwrap()
                .engine()
                .transport()
                .loop_range(),
            Some(range)
        );
        undo(&mut rig).unwrap();
        assert_eq!(rig.session.loop_range(), None);
        // Bus tracks refuse clips; the error comes back and nothing is recorded.
        let bus = add_track(&mut rig, "bus", TrackKind::Bus).unwrap();
        let before = rig.history.undo_len();
        assert!(add_midi_clip(
            &mut rig,
            bus,
            "x",
            TimePosition::ZERO,
            TimeDuration::from_samples(1),
            Vec::new()
        )
        .is_err());
        assert_eq!(rig.history.undo_len(), before);
    }

    #[test]
    fn save_and_open_round_trip() {
        let dir = std::env::temp_dir().join(format!("superb-project-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("song.superb.json");
        let mut rig = create("saved", R, 128).unwrap();
        let t = add_track(&mut rig, "t", TrackKind::Audio).unwrap();
        add_marker(&mut rig, "m", TimePosition::from_samples(7)).unwrap();
        save(&mut rig, &path).unwrap();
        assert!(is_saved(&rig));
        add_track(&mut rig, "later", TrackKind::Audio).unwrap();
        assert!(!is_saved(&rig));

        let reopened = open(&path, 128).unwrap();
        assert_eq!(reopened.session.name(), "saved");
        assert!(reopened.session.track(t).is_ok());
        assert_eq!(reopened.session.markers().len(), 1);
        assert!(is_saved(&reopened));
        assert_eq!(reopened.engine.sample_rate(), R);
        assert_eq!(reopened.engine.max_frames(), 128);
        assert!(open(dir.join("missing.json"), 128).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
