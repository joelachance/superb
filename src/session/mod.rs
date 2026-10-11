//! The session: the one thing you open, edit and play.
//!
//! A [`Session`] holds its [`Memory`] (tracks, clips, routing, tempo: what
//! gets saved), the undo history, the audio clips play and the running
//! engine. The functions
//! here create, open and save it and edit its memory with undo. Every
//! edit goes through the history, so [`undo`] and [`redo`] always work and
//! `session.history.is_clean()` tells whether anything changed since the
//! last save. Edits that change what the mixer
//! has to look like (tracks, clips, routing) rebuild it for you: at once
//! while a device is playing, otherwise at the next start.
//!
//! A session is a folder from the moment it is created: `memory.json`
//! beside an `audio/` folder of WAV files, which clips refer to by relative
//! path. Recordings are written there as they stop.
//!
//! Made of `aura::primitives::session` (the memory and its commands),
//! `aura::primitives::undo` (the history), `aura::primitives::io::wav`
//! (the files) and the engine side in [`steps`]. Each function below lists
//! the primitive calls it makes.

pub(crate) mod folder;
pub mod steps;

use std::path::{Path, PathBuf};

use crate::primitives::io::EngineCommand;
use crate::primitives::session::commands::{RemoveTempo, SetLoopRange, SetMeter, SetTempo};
use crate::primitives::session::Memory;
use crate::primitives::transport::LoopRange;
use crate::primitives::undo::Command;
use crate::primitives::{AuraError, BeatPosition, Result, SampleRate, TimeSignature, TrackId};

use std::sync::Arc;

use crate::primitives::graph::nodes::ClickParams;
use crate::primitives::io::cache::AudioCache;
use crate::primitives::io::{AudioThread, Engine, EngineHandle};
use crate::primitives::mixer::{SendParams, StripParams};
use crate::primitives::undo::UndoStack;
use crate::primitives::SendId;

/// A session: the memory you edit plus everything needed to play it.
///
/// Plain record, public fields, constructors only. Every flow function
/// takes `&mut Session`; reach into any field and call a primitive directly
/// when a flow does not do exactly what you want.
pub struct Session {
    /// The memory: tracks, clips, markers, routing, tempo.
    pub memory: Memory,
    /// Undo history for `memory`. Every flow edit pushes through this
    /// stack, so every edit is reversible. Consecutive edits of the same
    /// control (a fader, a knob, a clip being dragged) merge into one step
    /// while they arrive within [`Session::COALESCE_WINDOW`] of each other; a
    /// pause starts a new step. Call `history.seal()` to end a step sooner
    /// (on mouse up, say).
    pub history: UndoStack<Memory>,
    /// Audio held in memory, keyed by the path clips refer to, relative to
    /// the session folder (`audio/vox 1.wav`).
    pub audio: AudioCache,
    /// The folder the session lives in: `memory.json` and the `audio/`
    /// folder of recordings are inside. Set when the session is created or
    /// opened; recordings are written there as they stop.
    pub folder: PathBuf,
    /// Control handle to the engine: commands, graph swaps, position.
    pub engine: EngineHandle,
    /// The engine's audio side. Present until a device takes it
    /// (`devices::output`, feature `devices`); while present,
    /// [`steps::render_blocks`] ticks it by hand.
    pub audio_thread: Option<AudioThread>,
    /// The live controls of the mixer built by the last [`steps::rebuild`]:
    /// one per strip and send, keyed by the memory's ids.
    pub mixer: MixerHandles,
    /// A take being recorded, if any.
    pub recording: Option<crate::record::Recording>,
    /// Channels per strip and at the output: 2, stereo. Change it before
    /// the first rebuild if you need another width.
    pub channels: usize,
    /// Set by every edit that changes the mixer's structure while no device
    /// is driving the engine; the next `transport::start` or
    /// `steps::render_blocks` rebuilds and clears it. With a device attached
    /// the edit rebuilds at once instead.
    pub(crate) needs_rebuild: bool,
    /// The device output, once `devices::output` started one: the audio
    /// thread lives inside it until `devices::close_output`.
    #[cfg(feature = "devices")]
    pub output: Option<crate::primitives::io::LiveOutput>,
    /// The audio input, once `devices::input` opened it: what monitoring
    /// audio tracks hear and device recordings capture.
    #[cfg(feature = "devices")]
    pub input: Option<crate::devices::AudioInput>,
    /// Hardware MIDI inputs connected so far. Dropping one disconnects it.
    #[cfg(feature = "midi-io")]
    pub midi_inputs: Vec<crate::primitives::midi::input::MidiInputBridge>,
    /// Where state is kept, once [`state_file`] named a file; `None` means
    /// state is not saved.
    pub state_file: Option<PathBuf>,
    /// Whether the log has been told that state is not being saved.
    pub(crate) state_warned: bool,
}

impl Session {
    /// How long a pause between two edits of the same control turns them
    /// into separate undo steps.
    pub const COALESCE_WINDOW: core::time::Duration = core::time::Duration::from_millis(500);

    /// Whether the next start rebuilds; for tests of what rebuilds.
    #[cfg(test)]
    pub(crate) fn needs_rebuild_for_test(&self) -> bool {
        self.needs_rebuild
    }

    /// A stereo session around `memory` living in `folder`, with an engine
    /// that renders blocks of at most `block_size` frames at the memory's
    /// sample rate. Records the folder and touches nothing on disk;
    /// [`create`] and [`open`] do.
    pub fn new(memory: Memory, folder: impl Into<PathBuf>, block_size: usize) -> Result<Session> {
        let bpm = memory
            .tempo_map()
            .tempo_at(crate::primitives::BeatPosition::ZERO);
        let (engine, audio_thread) = Engine::live(memory.sample_rate(), block_size, bpm)?;
        let mut history = UndoStack::new();
        history.set_coalesce_window(Some(Self::COALESCE_WINDOW));
        Ok(Session {
            memory,
            history,
            audio: AudioCache::new(),
            folder: folder.into(),
            engine,
            audio_thread: Some(audio_thread),
            mixer: MixerHandles::default(),
            recording: None,
            channels: 2,
            needs_rebuild: true,
            #[cfg(feature = "devices")]
            output: None,
            #[cfg(feature = "devices")]
            input: None,
            #[cfg(feature = "midi-io")]
            midi_inputs: Vec::new(),
            state_file: None,
            state_warned: false,
        })
    }
}

impl core::fmt::Debug for Session {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("memory", &self.memory.name())
            .field("tracks", &self.memory.track_count())
            .field("undo_steps", &self.history.undo_len())
            .field("audio", &self.audio.len())
            .field("folder", &self.folder)
            .field("engine", &self.engine)
            .field("audio_thread", &self.audio_thread.is_some())
            .field("recording", &self.recording.is_some())
            .field("channels", &self.channels)
            .field("needs_rebuild", &self.needs_rebuild)
            .finish()
    }
}

/// The live controls of the mixer the engine is running, keyed by the
/// track or send each was built for.
///
/// Filled by every [`steps::rebuild`]. The `Mixer` itself becomes the graph
/// the engine runs; what the control side keeps are these handles, whose
/// atomics the audio thread reads every block, so a change made through
/// them is heard at the next block. Handles from before a rebuild are
/// stale: look them up again.
#[derive(Debug, Default)]
pub struct MixerHandles {
    /// A track or bus in the memory → its strip: volume, pan, mute, solo,
    /// pan law and the peak meter.
    pub strips: Vec<(TrackId, Arc<StripParams>)>,
    /// A send in the memory → its level and on/off.
    pub sends: Vec<(SendId, Arc<SendParams>)>,
    /// The metronome's click on the master: on/off, level, count-in.
    pub click: Arc<ClickParams>,
}

impl MixerHandles {
    /// The live strip of a track or bus.
    pub fn strip(&self, track: TrackId) -> Option<&Arc<StripParams>> {
        self.strips
            .iter()
            .find(|(id, _)| *id == track)
            .map(|(_, p)| p)
    }

    /// The live controls of a send.
    pub fn send(&self, send: SendId) -> Option<&Arc<SendParams>> {
        self.sends
            .iter()
            .find(|(id, _)| *id == send)
            .map(|(_, p)| p)
    }
}

/// Starts a new, empty session at 120 bpm in 4/4 in `folder`, which is
/// created (with its `audio/` folder) and given its `memory.json` at once,
/// so it can be opened from here on. The session is named after the folder.
///
/// Made of: `create_dir_all` → `Memory::new` → `Session::new` →
/// `Memory::save`.
pub fn create(
    folder: impl AsRef<Path>,
    sample_rate: SampleRate,
    block_size: usize,
) -> Result<Session> {
    let folder = folder.as_ref();
    let name = folder
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".into());
    std::fs::create_dir_all(folder.join(folder::AUDIO_DIR))
        .map_err(|e| AuraError::Message(format!("cannot create {}: {e}", folder.display())))?;
    let session = Session::new(Memory::new(name, sample_rate), folder, block_size)?;
    session.memory.save(folder.join(folder::MEMORY_FILE))?;
    log::info!(
        "{}: created; state (mute, solo, arm, monitor, input, MIDI channel, clip mute, send on/off, looping, click, count-in, playhead) is not saved until session::state_file names a file",
        folder.display()
    );
    Ok(session)
}

/// Opens a session folder written by [`save`]: reads `memory.json` and
/// loads every WAV file the clips refer to into [`Session::audio`]. A file
/// that is missing or at another sample rate is left out, and its clip
/// plays as silence. The undo history starts empty and clean. Recordings
/// a crash left unfinished are noted in the log, not placed:
/// [`record::recover`](crate::record::recover) does that.
///
/// Made of: `Memory::load` → `Session::new` → `wav::read` per audio clip →
/// [`record::unfinished`](crate::record::unfinished).
pub fn open(folder: impl AsRef<Path>, block_size: usize) -> Result<Session> {
    let folder = folder.as_ref();
    let memory = Memory::load(folder.join(folder::MEMORY_FILE))?;
    let mut session = Session::new(memory, folder, block_size)?;
    folder::load_audio(&mut session, folder);
    log::info!(
        "{}: opened; state (mute, solo, arm, monitor, input, MIDI channel, clip mute, send on/off, looping, click, count-in, playhead) is not saved until session::state_file names a file",
        folder.display()
    );
    let unfinished = crate::record::unfinished(&session);
    if !unfinished.is_empty() {
        log::warn!(
            "{}: {} recording(s) a crash left unfinished ({}); record::recover places them",
            folder.display(),
            unfinished.len(),
            unfinished
                .iter()
                .map(|k| k.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(session)
}

/// Saves the session into its folder: the memory as `memory.json`, after
/// writing any audio a clip refers to that is not on disk yet (recordings
/// already are). The history remembers this state as saved
/// (`session.history.is_clean()`).
///
/// Made of: `wav::write` per audio clip not yet on disk → `Memory::save` →
/// `UndoStack::mark_clean`.
pub fn save(session: &mut Session) -> Result<()> {
    let folder = session.folder.clone();
    std::fs::create_dir_all(&folder)
        .map_err(|e| AuraError::Message(format!("cannot create {}: {e}", folder.display())))?;
    folder::write_missing_audio(session, &folder)?;
    session.memory.save(folder.join(folder::MEMORY_FILE))?;
    session.history.mark_clean();
    if session.state_file.is_none() {
        log::info!(
            "{}: memory.json saved; state not saved, no state file named (session::state_file)",
            folder.display()
        );
    }
    write_state(session)
}

/// What [`import_audio`] brought in: the key a clip refers to the audio
/// by, and what the file was, so an application can say "44.1 kHz,
/// 24-bit, stereo, resampled". Whatever the file was, the session stores
/// it as 32-bit float at its own rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    /// `audio/<name>.wav`: the key a clip refers to the audio by.
    pub key: PathBuf,
    /// The file's own sample rate.
    pub source_rate: SampleRate,
    /// The file's own bits per sample when it has a fixed depth (16, 24,
    /// 32); `None` for lossy codecs such as MP3 and AAC, which have none.
    pub source_bits: Option<u32>,
    /// Channels.
    pub channels: usize,
    /// Frames as stored, at the session's rate.
    pub frames: usize,
    /// The session's rate, which the stored audio is at.
    pub stored_rate: SampleRate,
    /// Whether the audio was resampled from `source_rate` to `stored_rate`.
    pub resampled: bool,
}

impl core::fmt::Display for Imported {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}: {} Hz", self.key.display(), self.source_rate.hz())?;
        match self.source_bits {
            Some(bits) => write!(f, ", {bits}-bit")?,
            None => write!(f, ", lossy")?,
        }
        write!(f, ", {} channel(s)", self.channels)?;
        if self.resampled {
            write!(f, ", resampled to {} Hz", self.stored_rate.hz())?;
        }
        write!(f, ", stored as 32-bit float")
    }
}

/// Brings an audio file into the session and says what it was. With the
/// `files` feature any format symphonia decodes is read (WAV, AIFF, FLAC,
/// ALAC, MP3, AAC in MP4 or M4A, Ogg Vorbis, CAF, Matroska) and resampled
/// to the session's rate when it differs; without it, a WAV already at the
/// session's rate. The audio is written into the folder as 32-bit float at
/// the session's rate, `audio/<name>.wav` (`<name> 2.wav` when that is
/// taken), and kept in memory; `clip::add` places it by the returned key.
/// Not an edit: the file joins the folder and stays, placing it is the
/// undo step.
///
/// Made of: `io::files::decode` → `AudioFile::resampled_to` (or
/// `io::wav::read_with_format`) → `folder::free_key` → `folder::write_audio`
/// → `AudioCache::insert`.
pub fn import_audio(session: &mut Session, file: impl AsRef<Path>) -> Result<Imported> {
    let file = file.as_ref();
    let rate = session.memory.sample_rate();
    let (audio, source_rate, source_bits) = read_audio(file, rate)?;
    let resampled = source_rate != rate;
    if resampled {
        log::info!(
            "{}: resampled from {} Hz to the session's {} Hz",
            file.display(),
            source_rate.hz(),
            rate.hz()
        );
    }
    let stem = file
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio".into());
    let key = folder::free_key(session, &stem);
    folder::write_audio(&session.folder, &key, &audio, rate)?;
    let (channels, frames) = (audio.channels(), audio.frames());
    session.audio.insert(key.clone(), audio);
    Ok(Imported {
        key,
        source_rate,
        source_bits,
        channels,
        frames,
        stored_rate: rate,
        resampled,
    })
}

/// A file's audio at the session's rate, with the rate and depth it had.
#[cfg(feature = "files")]
fn read_audio(
    file: &Path,
    rate: SampleRate,
) -> Result<(
    crate::primitives::buffers::PlanarBuffer,
    SampleRate,
    Option<u32>,
)> {
    let decoded = crate::primitives::io::files::decode(file)?;
    let (source_rate, bits) = (decoded.sample_rate, decoded.bits_per_sample);
    Ok((decoded.resampled_to(rate)?.buffer, source_rate, bits))
}

/// A WAV file's audio, which has to be at the session's rate already.
#[cfg(not(feature = "files"))]
fn read_audio(
    file: &Path,
    rate: SampleRate,
) -> Result<(
    crate::primitives::buffers::PlanarBuffer,
    SampleRate,
    Option<u32>,
)> {
    let (audio, file_rate, format) = crate::primitives::io::wav::read_with_format(file)?;
    if file_rate != rate {
        return Err(AuraError::Message(format!(
            "{} is at {} Hz and the session at {} Hz; the `files` feature resamples and reads other formats",
            file.display(),
            file_rate.hz(),
            rate.hz()
        )));
    }
    Ok((audio, file_rate, Some(format.bits())))
}

/// Says where state is kept, or that it is not. State (a track's mute,
/// solo, arm, monitor and input, a clip's mute, a send's on/off, looping,
/// the click and its count-in, the playhead) is never written into
/// `memory.json`: until an application names a file here, a session opens
/// with all of it off. With a path, the file is read now if it exists (its
/// state lands on the memory, the mixer and the engine) and rewritten
/// after every state change and every [`save`], with the playhead. `None`
/// stops writing; the file stays.
///
/// Made of: `Memory::load_state` → `Memory::apply_state` →
/// `EngineHandle::send(SetLoopEnabled, Seek)` → `session::changed_structure`;
/// then `Memory::save_state` from `session::set_state` and [`save`].
pub fn state_file(session: &mut Session, path: Option<PathBuf>) -> Result<()> {
    session.state_file = path.clone();
    match path {
        Some(path) if path.exists() => {
            let state = Memory::load_state(&path)?;
            session.memory.apply_state(&state)?;
            session
                .engine
                .send(EngineCommand::SetLoopEnabled(session.memory.looping()))?;
            session
                .engine
                .send(EngineCommand::Seek(session.memory.playhead()))?;
            session
                .mixer
                .click
                .set_enabled(session.memory.metronome().enabled);
            changed_structure(session)
        }
        // A new file: what the session has now, so the file exists from here.
        Some(_) => write_state(session),
        None => Ok(()),
    }
}

/// Writes the state file, if one is named, with where the playhead is.
pub(crate) fn write_state(session: &mut Session) -> Result<()> {
    let Some(path) = session.state_file.clone() else {
        return Ok(());
    };
    let playhead = session.engine.position();
    session.memory.set_playhead(playhead);
    session.memory.save_state(path)
}

/// A fresh folder under the system temp directory for a test session.
#[cfg(test)]
pub(crate) fn scratch(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir().join("aura-tests").join(format!(
        "{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Sets the memory's tempo at the start. Consecutive changes are one undo
/// step. MIDI clips are scheduled through the tempo map, so the mixer
/// rebuilds, and the rebuild hands the engine the new map.
///
/// Made of: `SetTempo` at beat 0 through `session::structural`.
pub fn set_tempo(session: &mut Session, bpm: f64) -> Result<()> {
    structural(session, SetTempo::new(BeatPosition::ZERO, bpm))
}

/// Adds a tempo change at `beat` (`Some(bpm)`), or removes the one there
/// (`None`); the tempo at beat zero is set with [`set_tempo`] and cannot
/// be removed. Changes to the same beat coalesce. Rebuilds the mixer,
/// which hands the engine the new map.
///
/// Made of: `SetTempo` or `RemoveTempo` through `session::structural`.
pub fn tempo_change(session: &mut Session, beat: BeatPosition, bpm: Option<f64>) -> Result<()> {
    match bpm {
        Some(bpm) => structural(session, SetTempo::new(beat, bpm)),
        None => structural(session, RemoveTempo::new(beat)),
    }
}

/// Sets the meter (time signature) from `bar` on (`Some`), or removes the
/// change at that bar (`None`); bar 1 always has a meter. Beats stay
/// quarter notes whatever the signature, as in Logic and Cubase; the
/// click ticks on the notated beat. Rebuilds the mixer, which hands the
/// engine the new map.
///
/// Made of: `SetMeter` through `session::structural`.
pub fn meter_change(
    session: &mut Session,
    bar: u32,
    signature: Option<TimeSignature>,
) -> Result<()> {
    structural(session, SetMeter::new(bar, signature))
}

/// Sets or clears the loop range in the memory and on the running engine.
/// Turn looping on with [`transport::looping`](crate::transport::looping).
///
/// Made of: push `SetLoopRange` → `EngineHandle::send(SetLoop)`.
pub fn set_loop(session: &mut Session, range: Option<LoopRange>) -> Result<()> {
    edit(session, SetLoopRange::new(range))?;
    session.engine.send(EngineCommand::SetLoop(range))
}

/// Reverts the last edit. Returns `false` if there was nothing to undo.
///
/// Made of: `UndoStack::undo`; the mixer is marked for a rebuild because the
/// undone edit may have been structural.
pub fn undo(session: &mut Session) -> Result<bool> {
    let done = session.history.undo(&mut session.memory)?;
    if done {
        changed_structure(session)?;
    }
    Ok(done)
}

/// Re-applies the last undone edit.
///
/// Made of: `UndoStack::redo`, then the same rebuild marking as [`undo`].
pub fn redo(session: &mut Session) -> Result<bool> {
    let done = session.history.redo(&mut session.memory)?;
    if done {
        changed_structure(session)?;
    }
    Ok(done)
}

/// Pushes an edit that does not change the mixer's structure.
pub(crate) fn edit(session: &mut Session, command: impl Command<Memory>) -> Result<()> {
    session.history.push(&mut session.memory, command)
}

/// Pushes an edit that changes what the mixer must look like.
pub(crate) fn structural(session: &mut Session, command: impl Command<Memory>) -> Result<()> {
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

/// Changes state (mute, solo, arm, monitor, input, clip mute, send on/off,
/// looping, the click, count-in) on the memory. State is not an edit: it
/// never enters the undo history and does not make the session unsaved;
/// it is written to the state file at once, if [`state_file`] named one.
pub(crate) fn set_state(
    session: &mut Session,
    flip: impl FnOnce(&mut Memory) -> Result<()>,
) -> Result<()> {
    flip(&mut session.memory)?;
    if session.state_file.is_none() && !session.state_warned {
        session.state_warned = true;
        log::warn!(
            "{}: state changed with no state file named; it will not survive reopening the session (session::state_file)",
            session.folder.display()
        );
    }
    write_state(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::session::commands;
    use crate::primitives::session::{ClipSource, MidiNote, TrackKind};
    use crate::primitives::{TimeDuration, TimePosition};
    use crate::{clip, track};
    use core::time::Duration;

    const R: SampleRate = SampleRate::HZ_48K;

    #[test]
    fn create_edit_undo_redo() {
        let mut session = create(scratch("demo"), R, 64).unwrap();
        assert!(session.history.is_clean());
        assert!(session.needs_rebuild, "a fresh session has no console yet");
        session.needs_rebuild = false;

        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        assert!(session.needs_rebuild);
        track::rename(&mut session, vox, "vocals").unwrap();
        assert_eq!(session.memory.track(vox).unwrap().name, "vocals");
        session
            .audio
            .insert("takes/take 1.wav", PlanarBuffer::new(2, 50));
        let clip = clip::add(
            &mut session,
            vox,
            "takes/take 1.wav",
            TimePosition::from_samples(100),
        )
        .unwrap();
        assert_eq!(session.memory.clip(clip).unwrap().name, "take 1");
        assert_eq!(
            session.memory.clip(clip).unwrap().length(),
            TimeDuration::from_samples(50)
        );
        clip::move_to(&mut session, clip, vox, TimePosition::from_samples(200)).unwrap();
        clip::move_to(&mut session, clip, vox, TimePosition::from_samples(300)).unwrap();
        session.history.seal(); // "mouse up"; a pause would do the same
        let marker = crate::marker::add(&mut session, "verse", TimePosition::ZERO).unwrap();
        set_tempo(&mut session, 90.0).unwrap();
        assert_eq!(
            session.history.undo_len(),
            6,
            "the drag coalesced into one step"
        );
        assert!(!session.history.is_clean());

        assert!(undo(&mut session).unwrap()); // tempo
        assert!(undo(&mut session).unwrap()); // marker
        assert!(session.memory.marker(marker).is_err());
        assert!(undo(&mut session).unwrap()); // whole drag
        assert_eq!(session.memory.clip(clip).unwrap().start().samples(), 100);
        assert!(redo(&mut session).unwrap());
        assert_eq!(session.memory.clip(clip).unwrap().start().samples(), 300);
        clip::archive(&mut session, clip).unwrap();
        assert!(session.memory.clip(clip).unwrap().state.archived);
        track::archive(&mut session, vox).unwrap();
        assert!(session.memory.track(vox).unwrap().state.archived);
        while undo(&mut session).unwrap() {}
        assert!(
            session.memory.track(vox).is_err(),
            "back to the empty memory"
        );
        assert!(!undo(&mut session).unwrap());
        assert!(session.history.is_clean());
    }

    #[test]
    fn a_pause_between_edits_starts_a_new_undo_step() {
        use std::time::Instant;
        let mut session = create(scratch("demo"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let steps = session.history.undo_len();
        let t0 = Instant::now();
        // Three moves in quick succession: one step.
        for (i, db) in [-1.0, -2.0, -3.0].iter().enumerate() {
            session
                .history
                .push_at(
                    &mut session.memory,
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
                &mut session.memory,
                Box::new(commands::set_track_volume_db(vox, -4.0)),
                t0 + Session::COALESCE_WINDOW + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(session.history.undo_len(), steps + 2);
        assert!(undo(&mut session).unwrap());
        assert_eq!(session.memory.track(vox).unwrap().state.volume_db, -3.0);
    }

    #[test]
    fn midi_clips_and_loop_range() {
        let mut session = create(scratch("demo"), R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        let notes = vec![MidiNote {
            beat: 0.0,
            length: 1.0,
            note: 60,
            velocity: 100,
            channel: 0,
        }];
        let clip = clip::add(&mut session, keys, notes.clone(), TimePosition::ZERO).unwrap();
        assert!(matches!(
            session.memory.clip(clip).unwrap().source,
            ClipSource::Midi { .. }
        ));
        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(1_000)).unwrap();
        set_loop(&mut session, Some(range)).unwrap();
        assert_eq!(session.memory.loop_range(), Some(range));
        // The engine received the range too: apply it on the audio side.
        session.audio_thread.as_mut().unwrap().apply_commands();
        assert_eq!(
            session
                .audio_thread
                .as_ref()
                .unwrap()
                .engine()
                .transport()
                .loop_range(),
            Some(range)
        );
        undo(&mut session).unwrap();
        assert_eq!(session.memory.loop_range(), None);
        // Bus tracks refuse clips; the error comes back and nothing is recorded.
        let bus = track::add(&mut session, "bus", TrackKind::Bus).unwrap();
        let before = session.history.undo_len();
        assert!(clip::add(&mut session, bus, notes, TimePosition::ZERO).is_err());
        assert_eq!(session.history.undo_len(), before);
    }

    #[test]
    fn a_session_folder_holds_the_memory_and_the_audio() {
        let dir = scratch("saved");
        let mut session = create(&dir, R, 128).unwrap();
        assert_eq!(
            session.memory.name(),
            dir.file_name().unwrap().to_str().unwrap()
        );
        assert!(
            dir.join("memory.json").is_file(),
            "a session is a folder from the start"
        );
        assert!(dir.join("audio").is_dir());
        assert!(session.history.is_clean());
        let t = track::add(&mut session, "t", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 256);
        tone.fill(0.5);
        session.audio.insert("audio/tone.wav", tone);
        let clip = clip::add(&mut session, t, "audio/tone.wav", TimePosition::ZERO).unwrap();
        crate::marker::add(&mut session, "m", TimePosition::from_samples(7)).unwrap();
        assert!(!session.history.is_clean());
        assert_eq!(session.folder, dir);

        save(&mut session).unwrap();
        assert!(session.history.is_clean());
        assert!(
            dir.join("audio/tone.wav").is_file(),
            "the clip's audio was written"
        );
        track::add(&mut session, "later", TrackKind::Audio).unwrap();
        assert!(!session.history.is_clean());

        let mut reopened = open(&dir, 128).unwrap();
        assert_eq!(reopened.memory.name(), session.memory.name());
        assert!(reopened.memory.track(t).is_ok());
        assert_eq!(reopened.memory.markers().len(), 1);
        assert!(reopened.history.is_clean());
        assert_eq!(reopened.folder, dir);
        assert_eq!(reopened.engine.sample_rate(), R);
        assert_eq!(reopened.engine.max_frames(), 128);
        let audio = reopened
            .audio
            .get("audio/tone.wav".as_ref())
            .expect("audio loaded");
        assert_eq!(audio.frames(), 256);
        assert_eq!(audio.channel(0)[10], 0.5);
        crate::transport::start(&mut reopened).unwrap();
        assert!(steps::render_blocks(&mut reopened, 2).unwrap().peak() > 0.3);

        // A second save writes nothing twice and keeps the file.
        save(&mut reopened).unwrap();
        assert!(dir.join("audio/tone.wav").is_file());

        // A missing file opens fine; its clip is silent.
        std::fs::remove_file(dir.join("audio/tone.wav")).unwrap();
        let mut silent = open(&dir, 128).unwrap();
        assert!(silent.audio.get("audio/tone.wav".as_ref()).is_none());
        assert!(silent.memory.clip(clip).is_ok());
        crate::transport::start(&mut silent).unwrap();
        assert!(steps::render_blocks(&mut silent, 2).unwrap().is_silent());

        assert!(open(dir.join("missing"), 128).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tempo_and_meter_changes_are_edits_that_reach_the_engine() {
        let mut session = create(scratch("tempo_meter"), R, 64).unwrap();
        let steps = session.history.undo_len();
        tempo_change(&mut session, BeatPosition::new(4.0), Some(240.0)).unwrap();
        meter_change(&mut session, 2, Some(TimeSignature::new(3, 4).unwrap())).unwrap();
        assert_eq!(session.history.undo_len(), steps + 2);
        assert_eq!(
            session.memory.tempo_map().tempo_at(BeatPosition::new(5.0)),
            240.0
        );
        assert_eq!(
            session.memory.tempo_map().meter_at(BeatPosition::new(4.0)),
            TimeSignature::new(3, 4).unwrap()
        );
        assert!(
            meter_change(&mut session, 1, None).is_err(),
            "bar 1 keeps a meter"
        );
        assert!(tempo_change(&mut session, BeatPosition::new(9.0), None).is_err());

        // The engine gets the map at the next rebuild.
        crate::transport::start(&mut session).unwrap();
        steps::render_blocks(&mut session, 1).unwrap();
        let engine_map = session
            .audio_thread
            .as_ref()
            .unwrap()
            .engine()
            .transport()
            .tempo_map();
        assert_eq!(engine_map.tempo_at(BeatPosition::new(5.0)), 240.0);
        assert_eq!(engine_map.meter_points().len(), 2);

        // Undo takes both back, redo returns them, and removing works.
        assert!(undo(&mut session).unwrap());
        assert!(undo(&mut session).unwrap());
        assert_eq!(session.memory.tempo_map().tempo_points().len(), 1);
        assert_eq!(session.memory.tempo_map().meter_points().len(), 1);
        assert!(redo(&mut session).unwrap());
        assert!(redo(&mut session).unwrap());
        tempo_change(&mut session, BeatPosition::new(4.0), None).unwrap();
        meter_change(&mut session, 2, None).unwrap();
        assert_eq!(session.memory.tempo_map().tempo_points().len(), 1);
        assert_eq!(session.memory.tempo_map().meter_points().len(), 1);
        assert!(undo(&mut session).unwrap());
        assert_eq!(session.memory.tempo_map().meter_points().len(), 2);
    }

    #[test]
    fn the_playhead_is_remembered_across_save_and_open() {
        let folder = scratch("playhead");
        let mut session = create(&folder, R, 64).unwrap();
        state_file(&mut session, Some(folder.join("state.json"))).unwrap();
        crate::transport::seek(&mut session, TimePosition::from_samples(4_800)).unwrap();
        steps::render_blocks(&mut session, 1).unwrap();
        save(&mut session).unwrap();
        assert_eq!(session.memory.playhead(), TimePosition::from_samples(4_800));
        let mut again = open(&folder, 64).unwrap();
        state_file(&mut again, Some(folder.join("state.json"))).unwrap();
        steps::render_blocks(&mut again, 1).unwrap();
        assert_eq!(
            crate::transport::playhead(&again),
            TimePosition::from_samples(4_800)
        );
    }

    #[test]
    fn state_is_saved_only_where_the_application_says() {
        let folder = scratch("statefile");
        let mut session = create(&folder, R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        save(&mut session).unwrap();
        // Without a state file, a switch is not saved and does not make the
        // session unsaved either.
        track::mute(&mut session, vox, true).unwrap();
        assert!(session.history.is_clean(), "state is not unsaved work");
        save(&mut session).unwrap();
        let text = std::fs::read_to_string(folder.join("memory.json")).unwrap();
        assert!(
            !text.contains("\"mute\""),
            "memory.json carries no state: {text}"
        );
        let plain = open(&folder, 64).unwrap();
        assert!(!plain.memory.track(vox).unwrap().state.mute);

        // With one, every switch lands there at once, and comes back.
        let state = folder.join("state.json");
        state_file(&mut session, Some(state.clone())).unwrap();
        assert!(state.is_file());
        track::solo(&mut session, vox, true).unwrap();
        crate::transport::looping(&mut session, true).unwrap();
        crate::metronome::enable(&mut session, true).unwrap();
        crate::metronome::count_in(&mut session, 1).unwrap();
        let written = std::fs::read_to_string(&state).unwrap();
        assert!(written.contains("\"solo\": true") && written.contains("\"looping\": true"));
        crate::transport::seek(&mut session, TimePosition::from_samples(960)).unwrap();
        steps::render_blocks(&mut session, 1).unwrap();
        save(&mut session).unwrap();

        let mut again = open(&folder, 64).unwrap();
        assert!(
            !again.memory.track(vox).unwrap().state.mute,
            "not until asked"
        );
        state_file(&mut again, Some(state.clone())).unwrap();
        let t = again.memory.track(vox).unwrap();
        assert!(t.state.mute && t.state.solo);
        assert!(again.memory.looping());
        assert!(again.memory.metronome().enabled);
        assert_eq!(again.memory.metronome().count_in, 1);
        assert_eq!(again.memory.playhead(), TimePosition::from_samples(960));
        steps::render_blocks(&mut again, 1).unwrap();
        assert_eq!(
            crate::transport::playhead(&again),
            TimePosition::from_samples(960)
        );
        assert!(again.history.is_clean());

        // `None` stops writing; the file stays as it was.
        state_file(&mut again, None).unwrap();
        track::mute(&mut again, vox, false).unwrap();
        assert!(std::fs::read_to_string(&state)
            .unwrap()
            .contains("\"mute\": true"));
    }

    #[test]
    fn a_file_is_imported_into_the_folder_under_a_free_name() {
        let folder = scratch("import");
        let mut session = create(&folder, R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(1, 4_800);
        tone.fill(0.25);
        let file = scratch("import-src").join("tone.wav");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        crate::primitives::io::wav::write(
            &file,
            &tone,
            R,
            crate::primitives::io::wav::WavFormat::Float32,
        )
        .unwrap();
        let steps = session.history.undo_len();
        let imported = import_audio(&mut session, &file).unwrap();
        let key = imported.key.clone();
        assert_eq!(key, PathBuf::from("audio/tone.wav"));
        assert_eq!(imported.source_rate, R);
        assert_eq!(imported.source_bits, Some(32));
        assert!(!imported.resampled);
        assert_eq!((imported.channels, imported.frames), (1, 4_800));
        assert!(imported.to_string().contains("32-bit"), "{imported}");
        assert!(
            folder.join(&key).is_file(),
            "copied into the session folder"
        );
        assert_eq!(session.audio.get(&key).unwrap().frames(), 4_800);
        assert_eq!(session.history.undo_len(), steps, "not an edit");
        let clip = clip::add(&mut session, vox, key.clone(), TimePosition::ZERO).unwrap();
        assert_eq!(session.history.undo_len(), steps + 1, "placing it is");
        assert_eq!(
            session.memory.clip(clip).unwrap().length(),
            TimeDuration::from_samples(4_800)
        );
        let again = import_audio(&mut session, &file).unwrap();
        assert_eq!(again.key, PathBuf::from("audio/tone 2.wav"), "a free name");
        assert!(import_audio(&mut session, folder.join("missing.wav")).is_err());

        // A file at another rate: resampled with `files`, refused without.
        let other = file.with_file_name("slow.wav");
        crate::primitives::io::wav::write(
            &other,
            &tone,
            SampleRate::CD,
            crate::primitives::io::wav::WavFormat::Float32,
        )
        .unwrap();
        let slow = import_audio(&mut session, &other);
        if cfg!(feature = "files") {
            let slow = slow.unwrap();
            assert!(
                slow.resampled && slow.source_rate == SampleRate::CD,
                "{slow}"
            );
            let frames = session.audio.get(&slow.key).unwrap().frames();
            assert!(
                (frames as i64 - 4_800 * 48_000 / 44_100).abs() <= 2,
                "{frames}"
            );
        } else {
            assert!(slow.is_err());
        }
    }
}
