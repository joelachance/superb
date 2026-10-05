//! The session: the one thing you open, edit and play.
//!
//! A [`Session`] holds the [`Document`] (tracks, clips, routing, tempo), its
//! undo history, the audio in memory and the running engine. The functions
//! here create, open and save it and edit its document with undo. Every
//! edit goes through the history, so [`undo`] and [`redo`] always work and a
//! save can tell whether anything changed. Edits that change what the mixer
//! has to look like (tracks, clips, routing) rebuild it for you: at once
//! while a device is playing, otherwise at the next start.
//!
//! Made of `superb::primitives::session` (the document and its commands),
//! `superb::primitives::undo` (the history) and the engine side in
//! [`steps`]. Each function below lists the primitive calls it makes.

pub mod steps;

use std::path::{Path, PathBuf};

use crate::primitives::io::live::EngineCommand;
use crate::primitives::session::commands::{
    AddClip, AddMarker, MoveClip, RemoveClip, SetLoopRange, SetTempo,
};
use crate::primitives::session::{Clip, ClipSource, Document, Marker, MidiNote};
use crate::primitives::transport::LoopRange;
use crate::primitives::undo::Command;
use crate::primitives::{
    BeatPosition, ClipId, MarkerId, Result, SampleRate, TimeDuration, TimePosition, TrackId,
};

use std::sync::Arc;

use crate::primitives::io::cache::AudioCache;
use crate::primitives::io::live::{engine_pair, EngineHandle, EngineLoop};
use crate::primitives::mixer::{SendParams, StripParams};
use crate::primitives::undo::UndoStack;
use crate::primitives::SendId;

/// A session: the document you edit plus everything needed to play it.
///
/// Plain record, public fields, constructors only. Every flow function
/// takes `&mut Session`; reach into any field and call a primitive directly
/// when a flow does not do exactly what you want.
pub struct Session {
    /// The document: tracks, clips, markers, routing, tempo.
    pub document: Document,
    /// Undo history for `document`. Every flow edit pushes through this
    /// stack, so every edit is reversible. Consecutive edits of the same
    /// control (a fader, a knob, a clip being dragged) merge into one step
    /// while they arrive within [`Session::COALESCE_WINDOW`] of each other; a
    /// pause starts a new step. Call `history.seal()` to end a step sooner
    /// (on mouse up, say).
    pub history: UndoStack<Document>,
    /// Audio held in memory, keyed by the path clips refer to.
    pub cache: AudioCache,
    /// Control handle to the engine: commands, graph swaps, position.
    pub engine: EngineHandle,
    /// The engine's audio side. Present until a device takes it
    /// (`devices::output`, feature `devices`);
    /// while present, [`steps::render_blocks`] runs it by hand.
    pub engine_loop: Option<EngineLoop>,
    /// The live controls of the mixer built by the last [`steps::rebuild`]:
    /// one per strip and send, keyed by document id.
    pub mixer: MixerHandles,
    /// A take being recorded, if any.
    pub recording: Option<crate::record::Recording>,
    /// Channels per strip and at the output (2 = stereo).
    pub channels: usize,
    /// Set by every edit that changes the mixer's structure (a new track, a
    /// moved clip, a send) while no device is driving the engine; the next
    /// [`transport::start`](crate::transport::start) or
    /// [`steps::render_blocks`] rebuilds and clears it. With a device
    /// attached the edit rebuilds at once instead.
    pub needs_rebuild: bool,
    /// The running output stream, once started.
    #[cfg(feature = "devices")]
    pub output: Option<crate::primitives::io::device::OutputStream>,
    /// Hardware MIDI inputs connected so far. Dropping one disconnects it.
    #[cfg(feature = "midi-io")]
    pub midi_inputs: Vec<crate::primitives::midi::input::MidiInputBridge>,
}

impl Session {
    /// How long a pause between two edits of the same control turns them
    /// into separate undo steps.
    pub const COALESCE_WINDOW: core::time::Duration = core::time::Duration::from_millis(500);

    /// A stereo session around `document`, with an engine that renders blocks of
    /// at most `block_size` frames at the document's sample rate.
    pub fn new(document: Document, block_size: usize) -> Result<Session> {
        Self::with_channels(document, block_size, 2)
    }

    /// Like [`new`](Self::new) with an explicit channel count.
    pub fn with_channels(
        document: Document,
        block_size: usize,
        channels: usize,
    ) -> Result<Session> {
        let bpm = document
            .tempo_map()
            .tempo_at(crate::primitives::BeatPosition::ZERO);
        let (engine, engine_loop) = engine_pair(document.sample_rate(), block_size, bpm)?;
        let mut history = UndoStack::new();
        history.set_coalesce_window(Some(Self::COALESCE_WINDOW));
        Ok(Session {
            document,
            history,
            cache: AudioCache::new(),
            engine,
            engine_loop: Some(engine_loop),
            mixer: MixerHandles::default(),
            recording: None,
            channels,
            needs_rebuild: true,
            #[cfg(feature = "devices")]
            output: None,
            #[cfg(feature = "midi-io")]
            midi_inputs: Vec::new(),
        })
    }
}

impl core::fmt::Debug for Session {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("document", &self.document.name())
            .field("tracks", &self.document.track_count())
            .field("undo_steps", &self.history.undo_len())
            .field("cached_files", &self.cache.len())
            .field("engine", &self.engine)
            .field("engine_loop_attached", &self.engine_loop.is_some())
            .field("recording", &self.recording.is_some())
            .field("channels", &self.channels)
            .field("needs_rebuild", &self.needs_rebuild)
            .finish()
    }
}

/// The live controls of the mixer the engine is running, keyed by the
/// document object each was built for.
///
/// Filled by every [`steps::rebuild`]. The `Mixer` itself becomes the graph
/// the engine runs; what the control side keeps are these handles, whose
/// atomics the audio thread reads every block, so a change made through
/// them is heard at the next block. Handles from before a rebuild are
/// stale: look them up again.
#[derive(Debug, Default)]
pub struct MixerHandles {
    /// Document track or bus → its strip: volume, pan, mute, solo, pan law
    /// and the peak meter.
    pub strips: Vec<(TrackId, Arc<StripParams>)>,
    /// Document send → its level and on/off.
    pub sends: Vec<(SendId, Arc<SendParams>)>,
}

impl MixerHandles {
    /// The live strip of a document track or bus.
    pub fn strip(&self, track: TrackId) -> Option<&Arc<StripParams>> {
        self.strips
            .iter()
            .find(|(id, _)| *id == track)
            .map(|(_, p)| p)
    }

    /// The live controls of a document send.
    pub fn send(&self, send: SendId) -> Option<&Arc<SendParams>> {
        self.sends
            .iter()
            .find(|(id, _)| *id == send)
            .map(|(_, p)| p)
    }
}

/// Starts a new, empty session at 120 bpm in 4/4.
///
/// Made of: `Document::new` → `Session::new`.
pub fn create(name: &str, sample_rate: SampleRate, block_size: usize) -> Result<Session> {
    Session::new(Document::new(name, sample_rate), block_size)
}

/// Opens a session saved with [`save`]. The undo history starts empty and
/// clean.
///
/// Made of: `Document::load` → `Session::new`.
pub fn open(path: impl AsRef<Path>, block_size: usize) -> Result<Session> {
    Session::new(Document::load(path)?, block_size)
}

/// Writes the document as JSON and remembers this state as saved.
///
/// Made of: `Document::save` → `UndoStack::mark_clean`.
pub fn save(session: &mut Session, path: impl AsRef<Path>) -> Result<()> {
    session.document.save(path)?;
    session.history.mark_clean();
    Ok(())
}

/// `true` if nothing changed since the last [`save`] (or since opening).
/// Switches flipped since (mute, arm, bypass...) count as changes even
/// though they are not undo steps.
pub fn is_saved(session: &Session) -> bool {
    session.history.is_clean()
}

/// Places audio from the cache on a track. `take` is the key the audio was
/// cached under (see [`Session::cache`]); the clip plays from the start of it.
///
/// Made of: `Document::allocate_clip_id` → `Clip::with_id` → push `AddClip`.
pub fn add_audio_clip(
    session: &mut Session,
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
    let id = session.document.allocate_clip_id();
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
    structural(session, AddClip::new(track, clip))?;
    Ok(id)
}

/// Places a MIDI clip holding `notes` (beats relative to the clip start).
///
/// Made of: `Document::allocate_clip_id` → `Clip::with_id` → push `AddClip`.
pub fn add_midi_clip(
    session: &mut Session,
    track: TrackId,
    name: &str,
    start: TimePosition,
    length: TimeDuration,
    notes: Vec<MidiNote>,
) -> Result<ClipId> {
    let id = session.document.allocate_clip_id();
    let clip = Clip::with_id(id, name, start, length, ClipSource::Midi { notes })?;
    structural(session, AddClip::new(track, clip))?;
    Ok(id)
}

/// Moves a clip to start at `start`. Moves of the same clip within
/// [`Session::COALESCE_WINDOW`] of each other are one undo step, so a drag
/// undoes in one go.
///
/// Made of: push `MoveClip`.
pub fn move_clip(session: &mut Session, clip: ClipId, start: TimePosition) -> Result<()> {
    structural(session, MoveClip::new(clip, start))
}

/// Removes a clip.
///
/// Made of: push `RemoveClip`.
pub fn remove_clip(session: &mut Session, clip: ClipId) -> Result<()> {
    structural(session, RemoveClip::new(clip))
}

/// Drops a named marker on the timeline.
///
/// Made of: `Document::allocate_marker_id` → `Marker::with_id` → push `AddMarker`.
pub fn add_marker(session: &mut Session, name: &str, at: TimePosition) -> Result<MarkerId> {
    let id = session.document.allocate_marker_id();
    edit(session, AddMarker::new(Marker::with_id(id, name, at)))?;
    Ok(id)
}

/// Sets the document's tempo at the start. Consecutive changes are one undo step.
///
/// Made of: push `SetTempo` at beat 0.
pub fn set_tempo(session: &mut Session, bpm: f64) -> Result<()> {
    edit(session, SetTempo::new(BeatPosition::ZERO, bpm))
}

/// Sets or clears the loop range in the document and on the running engine.
/// Enable looping with [`transport::set_loop_enabled`](crate::transport::set_loop_enabled).
///
/// Made of: push `SetLoopRange` → `EngineHandle::send(SetLoop)`.
pub fn set_loop(session: &mut Session, range: Option<LoopRange>) -> Result<()> {
    edit(session, SetLoopRange::new(range))?;
    session.engine.send(EngineCommand::SetLoop(range))
}

/// Reverts the last edit. Returns `false` if there was nothing to undo.
///
/// Made of: `UndoStack::undo`; the console is marked for a rebuild because the
/// undone edit may have been structural.
pub fn undo(session: &mut Session) -> Result<bool> {
    let done = session.history.undo(&mut session.document)?;
    if done {
        changed_structure(session)?;
    }
    Ok(done)
}

/// Re-applies the last undone edit.
///
/// Made of: `UndoStack::redo`, then the same rebuild marking as [`undo`].
pub fn redo(session: &mut Session) -> Result<bool> {
    let done = session.history.redo(&mut session.document)?;
    if done {
        changed_structure(session)?;
    }
    Ok(done)
}

/// Decodes an audio file, converts it to the document's sample rate, caches it
/// and places a clip of its full length at `at`.
///
/// Made of: `AudioCache::load` → [`add_audio_clip`].
#[cfg(feature = "files")]
pub fn import_audio(
    session: &mut Session,
    track: TrackId,
    path: impl AsRef<Path>,
    at: TimePosition,
) -> Result<ClipId> {
    let path = path.as_ref();
    let audio = session.cache.load(path, session.document.sample_rate())?;
    let length = TimeDuration::from_samples(audio.frames() as i64);
    add_audio_clip(session, track, path.to_path_buf(), at, length)
}

/// Pushes an edit that does not change the console's structure.
pub(crate) fn edit(session: &mut Session, command: impl Command<Document>) -> Result<()> {
    session.history.push(&mut session.document, command)
}

/// Pushes an edit that changes what the mixer must look like.
pub(crate) fn structural(session: &mut Session, command: impl Command<Document>) -> Result<()> {
    edit(session, command)?;
    changed_structure(session)
}

/// Records that the mixer has to change shape. While a device is driving
/// the engine the mixer is rebuilt right away, so the change is heard
/// within a block; otherwise the rebuild waits for the next
/// [`transport::start`](crate::transport::start) or
/// [`steps::render_blocks`], which is cheaper when many edits arrive at
/// once (opening a file, say).
pub(crate) fn changed_structure(session: &mut Session) -> Result<()> {
    session.needs_rebuild = true;
    #[cfg(feature = "devices")]
    if session.output.is_some() {
        steps::rebuild(session)?;
    }
    Ok(())
}

/// Changes state (mute, solo, arm, monitor, bypass, send on/off) on the
/// document. State is saved with the session but is not an edit: it never
/// enters the undo history, so the history is told its saved point is gone
/// and [`is_saved`] turns false.
pub(crate) fn set_state(
    session: &mut Session,
    flip: impl FnOnce(&mut Document) -> Result<()>,
) -> Result<()> {
    flip(&mut session.document)?;
    session.history.mark_dirty();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::commands;
    use crate::primitives::session::TrackKind;
    use crate::track;
    use core::time::Duration;

    const R: SampleRate = SampleRate::HZ_48K;

    #[test]
    fn create_edit_undo_redo() {
        let mut session = create("demo", R, 64).unwrap();
        assert!(is_saved(&session));
        assert!(session.needs_rebuild, "a fresh session has no console yet");
        session.needs_rebuild = false;

        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        assert!(session.needs_rebuild);
        track::rename(&mut session, vox, "vocals").unwrap();
        assert_eq!(session.document.track(vox).unwrap().name, "vocals");
        let clip = add_audio_clip(
            &mut session,
            vox,
            "takes/take 1.wav",
            TimePosition::from_samples(100),
            TimeDuration::from_samples(50),
        )
        .unwrap();
        assert_eq!(session.document.clip(clip).unwrap().name, "take 1");
        move_clip(&mut session, clip, TimePosition::from_samples(200)).unwrap();
        move_clip(&mut session, clip, TimePosition::from_samples(300)).unwrap();
        session.history.seal(); // "mouse up"; a pause would do the same
        let marker = add_marker(&mut session, "verse", TimePosition::ZERO).unwrap();
        set_tempo(&mut session, 90.0).unwrap();
        assert_eq!(
            session.history.undo_len(),
            6,
            "the drag coalesced into one step"
        );
        assert!(!is_saved(&session));

        assert!(undo(&mut session).unwrap()); // tempo
        assert!(undo(&mut session).unwrap()); // marker
        assert!(session.document.marker(marker).is_err());
        assert!(undo(&mut session).unwrap()); // whole drag
        assert_eq!(session.document.clip(clip).unwrap().start().samples(), 100);
        assert!(redo(&mut session).unwrap());
        assert_eq!(session.document.clip(clip).unwrap().start().samples(), 300);
        remove_clip(&mut session, clip).unwrap();
        assert!(session.document.clip(clip).is_err());
        track::remove(&mut session, vox).unwrap();
        assert!(session.document.track(vox).is_err());
        while undo(&mut session).unwrap() {}
        assert!(
            session.document.track(vox).is_err(),
            "back to the empty document"
        );
        assert!(!undo(&mut session).unwrap());
        assert!(is_saved(&session));
    }

    #[test]
    fn a_pause_between_edits_starts_a_new_undo_step() {
        use std::time::Instant;
        let mut session = create("demo", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let steps = session.history.undo_len();
        let t0 = Instant::now();
        // Three moves in quick succession: one step.
        for (i, db) in [-1.0, -2.0, -3.0].iter().enumerate() {
            session
                .history
                .push_at(
                    &mut session.document,
                    Box::new(commands::set_track_volume_db(vox, *db)),
                    t0 + Duration::from_millis(50 * i as u64),
                )
                .unwrap();
        }
        assert_eq!(session.history.undo_len(), steps + 1);
        // A move after a pause longer than the window: a new step.
        session
            .history
            .push_at(
                &mut session.document,
                Box::new(commands::set_track_volume_db(vox, -4.0)),
                t0 + Session::COALESCE_WINDOW + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(session.history.undo_len(), steps + 2);
        assert!(undo(&mut session).unwrap());
        assert_eq!(session.document.track(vox).unwrap().state.volume_db, -3.0);
    }

    #[test]
    fn midi_clips_and_loop_range() {
        let mut session = create("demo", R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        let notes = vec![MidiNote {
            beat: 0.0,
            length: 1.0,
            note: 60,
            velocity: 100,
            channel: 0,
        }];
        let clip = add_midi_clip(
            &mut session,
            keys,
            "chord",
            TimePosition::ZERO,
            TimeDuration::from_samples(48_000),
            notes,
        )
        .unwrap();
        assert!(matches!(
            session.document.clip(clip).unwrap().source,
            ClipSource::Midi { .. }
        ));
        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(1_000)).unwrap();
        set_loop(&mut session, Some(range)).unwrap();
        assert_eq!(session.document.loop_range(), Some(range));
        // The engine received the range too: apply it on the loop side.
        session.engine_loop.as_mut().unwrap().apply_commands();
        assert_eq!(
            session
                .engine_loop
                .as_ref()
                .unwrap()
                .engine()
                .transport()
                .loop_range(),
            Some(range)
        );
        undo(&mut session).unwrap();
        assert_eq!(session.document.loop_range(), None);
        // Bus tracks refuse clips; the error comes back and nothing is recorded.
        let bus = track::add(&mut session, "bus", TrackKind::Bus).unwrap();
        let before = session.history.undo_len();
        assert!(add_midi_clip(
            &mut session,
            bus,
            "x",
            TimePosition::ZERO,
            TimeDuration::from_samples(1),
            Vec::new()
        )
        .is_err());
        assert_eq!(session.history.undo_len(), before);
    }

    #[test]
    fn save_and_open_round_trip() {
        let dir = std::env::temp_dir().join(format!("superb-project-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("document.superb.json");
        let mut session = create("saved", R, 128).unwrap();
        let t = track::add(&mut session, "t", TrackKind::Audio).unwrap();
        add_marker(&mut session, "m", TimePosition::from_samples(7)).unwrap();
        save(&mut session, &path).unwrap();
        assert!(is_saved(&session));
        track::add(&mut session, "later", TrackKind::Audio).unwrap();
        assert!(!is_saved(&session));

        let reopened = open(&path, 128).unwrap();
        assert_eq!(reopened.document.name(), "saved");
        assert!(reopened.document.track(t).is_ok());
        assert_eq!(reopened.document.markers().len(), 1);
        assert!(is_saved(&reopened));
        assert_eq!(reopened.engine.sample_rate(), R);
        assert_eq!(reopened.engine.max_frames(), 128);
        assert!(open(dir.join("missing.json"), 128).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
