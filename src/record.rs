//! Recording: arm tracks, capture input, get a clip on each of them.
//!
//! [`start`] records from a [`Source`] onto every armed track, from the
//! playhead or from the punch-in point ahead of it ([`punch_in`]), until
//! [`stop`] or the punch-out point ([`punch_out`]); the transport rolls
//! from where it is and drops into and out of record at those points.
//! Every armed audio track's audio streams into its own WAV file in the
//! session folder (`audio/vox 1.wav`) from the first block, flushed about
//! once a second, so a crash costs a second and never the take; [`stop`]
//! finishes the files, keeps the audio in memory, and places a clip on each
//! of those tracks, as one undo step; [`recover`] places what a crash left. An armed MIDI track gets a clip of the
//! notes that reached the engine meanwhile, from any port or
//! `midi::play_note`, stamped where they arrived. The audio arrives through a capture
//! queue that holds only a fraction of a second, so something has to empty
//! it as it fills: a hardware source gets a thread that does so; a queue
//! you fill yourself waits for [`collect`], which
//! [`session::steps::render_blocks`] calls after every block it renders.
//!
//! A device take lands where it was played: its first frame is lined up
//! with where the transport began rolling, comparing the input's capture
//! instants with the output's playback instants on the device clock, so
//! the buffers in and out are taken back by themselves.
//!
//! Made of `aura::primitives::io::capture` (the queue),
//! `aura::primitives::io::wav::Writer` (the files), the session's audio
//! input (`devices::input`, feature `devices`), the audio held on the
//! `Session`, and [`clip::add`](crate::clip::add).

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::io::wav;
use crate::primitives::io::{CapturedMidi, EngineCommand, RollStart};
use crate::primitives::midi::MidiMessage;
use crate::primitives::session::commands::{AddClip, PunchPoint, SetPunch};
use crate::primitives::session::{Clip, ClipSource, Input, Memory, MidiNote, TrackKind};
use crate::primitives::transport::TempoMap;
use crate::primitives::undo::Command;
use crate::primitives::{
    AuraError, BeatPosition, ClipId, RawId, Result, SampleRate, TimeDuration, TimePosition, TrackId,
};
use crate::session;
use crate::session::Session;
use crate::transport;

/// Where a recording's audio comes from.
pub enum Source {
    /// The session's audio input (`devices::input`). If it is not open
    /// yet, this opens it on the named device (`None` for the default).
    /// Every channel the input carries is captured, and a thread gathers
    /// the audio until [`stop`]; each track keeps the channel(s) its
    /// [`track::input`](crate::track::input) names.
    #[cfg(feature = "devices")]
    Device(Option<String>),
    /// A capture queue you fill yourself (a file, a generator, a test) with
    /// blocks of `channels` channels. Blocks wait on the queue until
    /// [`collect`] runs; [`session::steps::render_blocks`] does that after
    /// every block, so an offline program never calls it.
    Queue {
        /// The queue's consumer end (`capture_queue` gives you both ends).
        blocks: CaptureConsumer,
        /// Channels in every block.
        channels: usize,
    },
}

/// A recording in progress: what [`Session::recording`] holds between
/// [`start`] and [`stop`]. Its audio is on disk from the first block: one
/// WAV file per armed audio track in the session folder (`audio/vox
/// 1.wav`), written as the blocks arrive and flushed about once a second,
/// so a crash costs a second of audio, not the take, and the take never
/// has to fit in memory. The counters here are what an application shows
/// while it records: frames captured and sealed on disk, blocks dropped,
/// disk errors, the level of the latest block, and how long ago it came.
pub struct Recording {
    /// The audio capture, when an audio track is armed.
    capture: Option<Capture>,
    /// Incoming MIDI gathered so far, for the armed MIDI tracks.
    midi: Vec<CapturedMidi>,
    started_at: TimePosition,
    punch_in: TimePosition,
    punch_out: Option<TimePosition>,
    tracks: Vec<TrackId>,
    channels: usize,
    /// `EngineHandle::roll_start().rolls` before the transport was told to
    /// roll, so `stop` can tell this recording's roll start from an older one.
    rolls_before: u64,
    /// The files being written, as keys into the session folder.
    keys: Vec<PathBuf>,
    /// What the capture has done so far, shared with the thread that writes.
    health: Arc<Health>,
}

impl Recording {
    /// Frames captured so far: handed to the files, not all on disk yet.
    pub fn frames(&self) -> usize {
        self.health.frames.load(Ordering::Acquire)
    }

    /// Frames on disk for good in every file of the recording: written,
    /// synced and covered by a header a reader accepts. Everything before
    /// this frame survives a crash and will never change, so an uploader
    /// can ship the byte range it covers. Advances about once a second.
    pub fn sealed_frames(&self) -> u64 {
        self.health.sealed.load(Ordering::Acquire)
    }

    /// Writes that failed across the files (disk full, file gone, I/O
    /// error). Anything above zero means a gap in a file; the recording
    /// keeps draining its queue so the audio thread is never held up, and
    /// the application decides whether to stop.
    pub fn disk_errors(&self) -> u64 {
        self.health.errors.load(Ordering::Acquire)
    }

    /// Blocks the input side could not queue because the queue was full.
    /// Anything above zero means a gap in the recording.
    pub fn dropped(&self) -> u64 {
        self.capture.as_ref().map_or(0, Capture::dropped)
    }

    /// Peak per captured channel of the latest block, 0.0 to 1.0 and
    /// above when clipping; all zero before the first block.
    pub fn peaks(&self) -> Vec<f32> {
        self.health
            .peaks
            .iter()
            .map(|p| f32::from_bits(p.load(Ordering::Relaxed)))
            .collect()
    }

    /// How long ago the last block arrived; `None` before the first. A
    /// value that keeps growing while the transport rolls means the source
    /// went quiet: the device was unplugged, or nothing feeds the queue.
    pub fn since_last_block(&self) -> Option<core::time::Duration> {
        let at = self.health.last_block_us.load(Ordering::Acquire);
        (at != u64::MAX).then(|| {
            self.health
                .since
                .elapsed()
                .saturating_sub(core::time::Duration::from_micros(at))
        })
    }

    /// The files being written, one per armed audio track, as keys into
    /// the session folder (`audio/vox 1.wav`). They exist from [`start`]
    /// on, grow as the recording runs, and are the clips' audio after
    /// [`stop`].
    pub fn files(&self) -> &[PathBuf] {
        &self.keys
    }

    /// Where the transport started rolling: at or before the punch-in.
    pub fn started_at(&self) -> TimePosition {
        self.started_at
    }

    /// Where the recording begins and the clips will start.
    pub fn punch_in(&self) -> TimePosition {
        self.punch_in
    }

    /// Where the recording ends on its own, if a punch-out point was set.
    pub fn punch_out(&self) -> Option<TimePosition> {
        self.punch_out
    }

    /// The tracks that were armed when it started; each gets a clip.
    pub fn tracks(&self) -> &[TrackId] {
        &self.tracks
    }

    /// Audio channels being captured; zero when no audio track is armed.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Incoming MIDI messages gathered so far.
    pub fn midi_messages(&self) -> usize {
        self.midi.len()
    }

    /// Moves waiting blocks into a queue recording's files; see [`collect`].
    pub(crate) fn collect(&mut self) -> usize {
        match self.capture.as_mut() {
            Some(Capture::Manual { blocks, files }) => files.append(blocks),
            _ => 0,
        }
    }

    /// Takes the MIDI the engine captured since the last call.
    pub(crate) fn gather_midi(&mut self, engine: &mut crate::primitives::io::EngineHandle) {
        while let Some(message) = engine.captured_midi() {
            self.midi.push(message);
        }
    }
}

impl core::fmt::Debug for Recording {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Recording")
            .field("started_at", &self.started_at)
            .field("punch_in", &self.punch_in)
            .field("punch_out", &self.punch_out)
            .field("tracks", &self.tracks)
            .field("channels", &self.channels)
            .field("files", &self.keys)
            .field("frames", &self.frames())
            .field("sealed", &self.sealed_frames())
            .field("dropped", &self.dropped())
            .field("disk_errors", &self.disk_errors())
            .field(
                "threaded",
                &matches!(self.capture, Some(Capture::Threaded(_))),
            )
            .field("midi", &self.midi.len())
            .finish()
    }
}

/// What a recording's capture has done so far: written by whichever thread
/// drains the queue, read by the application through [`Recording`].
pub(crate) struct Health {
    frames: AtomicUsize,
    sealed: AtomicU64,
    errors: AtomicU64,
    /// Peak of the latest block per captured channel, as `f32` bits.
    peaks: Vec<AtomicU32>,
    /// When the latest block arrived, in microseconds after `since`;
    /// `u64::MAX` before the first.
    last_block_us: AtomicU64,
    since: std::time::Instant,
}

impl Health {
    fn new(channels: usize) -> Arc<Self> {
        Arc::new(Health {
            frames: AtomicUsize::new(0),
            sealed: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            peaks: (0..channels).map(|_| AtomicU32::new(0)).collect(),
            last_block_us: AtomicU64::new(u64::MAX),
            since: std::time::Instant::now(),
        })
    }

    /// Notes a block: its peaks and when it came.
    fn saw(&self, block: &PlanarBuffer) {
        for (ch, peak) in self.peaks.iter().enumerate() {
            let value = if ch < block.channels() {
                block.channel(ch).iter().fold(0.0f32, |m, s| m.max(s.abs()))
            } else {
                0.0
            };
            peak.store(value.to_bits(), Ordering::Relaxed);
        }
        self.last_block_us
            .store(self.since.elapsed().as_micros() as u64, Ordering::Release);
    }
}

/// How a recording's blocks get from the capture queue into its files.
pub(crate) enum Capture {
    /// Blocks wait on the queue until [`collect`] writes them.
    Manual {
        /// The queue's consumer end.
        blocks: CaptureConsumer,
        /// The files being written.
        files: Files,
    },
    /// A thread empties the queue into the files as it fills; nothing to
    /// call. Only a device source makes one, so without `devices` it
    /// exists for tests.
    #[cfg_attr(not(feature = "devices"), allow(dead_code))]
    Threaded(Collector),
}

impl Capture {
    /// Blocks the input side had to drop.
    pub(crate) fn dropped(&self) -> u64 {
        match self {
            Capture::Manual { blocks, .. } => blocks.dropped(),
            Capture::Threaded(collector) => collector.dropped(),
        }
    }
}

/// A thread that writes captured blocks to the recording's files as they
/// arrive, so a recording can be as long as the disk allows no matter how
/// small the queue is. Made by `steps::spawn_collector`;
/// [`finish`](Collector::finish) hands the files back.
pub(crate) struct Collector {
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<(Files, CaptureConsumer)>>,
}

impl Collector {
    /// Blocks the input side had to drop (the thread was late, or the
    /// queue is too small for the block rate).
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops the thread after it has drained what is left on the queue and
    /// returns the files and the queue's consumer end. Stop the input side
    /// first so nothing new arrives.
    pub(crate) fn finish(mut self) -> Result<(Files, CaptureConsumer)> {
        self.stop.store(true, Ordering::Release);
        let handle = self.handle.take().expect("finish is called once");
        handle
            .join()
            .map_err(|_| AuraError::Message("the recording collector thread panicked".into()))
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        // Not finished: let the thread exit on its own. Its files stay on
        // disk, unfinished, with their tags; `recover` finds them.
        self.stop.store(true, Ordering::Release);
    }
}

impl core::fmt::Debug for Collector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Collector")
            .field("dropped", &self.dropped())
            .finish()
    }
}

/// The files a recording writes as it goes: one per armed audio track,
/// each holding the channels the track's [`Input`] selects, plus a tag
/// file beside each (`audio/vox 1.wav.recording`) that says where the clip
/// will go, so [`recover`] can place it after a crash. Flushed every
/// `flush_every` frames.
pub(crate) struct Files {
    entries: Vec<Entry>,
    /// Device-clock stamp of the first block, once one arrived.
    first_us: Option<u64>,
    frames: usize,
    since_flush: usize,
    flush_every: usize,
    /// Silence for a channel the capture does not have.
    silence: Vec<f32>,
    health: Arc<Health>,
}

/// One file of a recording.
pub(crate) struct Entry {
    track: TrackId,
    /// `audio/<track> <n>.wav`, relative to the session folder.
    key: PathBuf,
    /// The file itself.
    path: PathBuf,
    /// Which captured channels it holds, in order.
    pick: Vec<usize>,
    writer: wav::Writer,
    tag: Tag,
}

/// A finished file of a recording.
pub(crate) struct Finished {
    track: TrackId,
    key: PathBuf,
    path: PathBuf,
    frames: u64,
}

/// What `audio/<file>.wav.recording` holds while the file is being written:
/// enough to place the clip without the session that was recording.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Tag {
    format_version: u32,
    track: RawId,
    key: PathBuf,
    /// Timeline sample of the file's first frame.
    started_at: i64,
    /// Where the clip starts.
    punch_in: i64,
    punch_out: Option<i64>,
    sample_rate: u32,
    channels: usize,
    /// Device-clock stamp of the first block, once known.
    first_us: Option<u64>,
}

const TAG_FORMAT_VERSION: u32 = 1;
/// What follows a recording's file name while it is being written.
const TAG_SUFFIX: &str = ".recording";

impl Tag {
    fn path_for(file: &Path) -> PathBuf {
        let mut name = file.as_os_str().to_owned();
        name.push(TAG_SUFFIX);
        PathBuf::from(name)
    }

    fn write(&self, file: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| AuraError::Message(format!("cannot encode the recording tag: {e}")))?;
        std::fs::write(Self::path_for(file), json).map_err(|e| {
            AuraError::Message(format!(
                "cannot write {}: {e}",
                Self::path_for(file).display()
            ))
        })
    }

    fn read(path: &Path) -> Result<Tag> {
        let json = std::fs::read_to_string(path)
            .map_err(|e| AuraError::Message(format!("cannot read {}: {e}", path.display())))?;
        serde_json::from_str(&json).map_err(|e| {
            AuraError::Message(format!("{} is not a recording tag: {e}", path.display()))
        })
    }
}

impl Files {
    /// Frames appended so far.
    #[cfg(test)]
    pub(crate) fn frames(&self) -> usize {
        self.frames
    }

    /// Device-clock stamp of the first block, once one arrived.
    pub(crate) fn first_us(&self) -> Option<u64> {
        self.first_us
    }

    /// Writes every waiting block to the files and hands the blocks back to
    /// the input side. A channel the capture lacks is written as silence;
    /// the first block's device-clock stamp is kept and noted in the tags.
    /// Returns the frames appended. Does I/O; never call it on the audio
    /// thread.
    pub(crate) fn append(&mut self, blocks: &mut CaptureConsumer) -> usize {
        let mut appended = 0;
        while let Some(block) = blocks.pop() {
            let frames = block.frames();
            if self.first_us.is_none() {
                self.first_us = Some(block.block_start_us());
                for entry in &mut self.entries {
                    entry.tag.first_us = self.first_us;
                    let _ = entry.tag.write(&entry.path);
                }
            }
            if frames > self.silence.len() {
                self.silence.resize(frames, 0.0);
            }
            let buffer = block.buffer();
            let silence = &self.silence[..frames];
            for entry in &mut self.entries {
                let planes: Vec<&[f32]> = entry
                    .pick
                    .iter()
                    .map(|&c| {
                        if c < buffer.channels() {
                            buffer.channel(c)
                        } else {
                            silence
                        }
                    })
                    .collect();
                // A failed write is counted by the writer; the queue keeps draining.
                let _ = entry.writer.write(&planes);
            }
            self.health.saw(buffer);
            blocks.recycle(block);
            appended += frames;
            self.frames += frames;
            self.since_flush += frames;
            if self.since_flush >= self.flush_every {
                self.flush();
            }
        }
        self.health.frames.store(self.frames, Ordering::Release);
        self.note_errors();
        appended
    }

    /// Pushes every file to disk and seals what is there.
    pub(crate) fn flush(&mut self) {
        for entry in &mut self.entries {
            let _ = entry.writer.flush();
        }
        self.since_flush = 0;
        let sealed = self
            .entries
            .iter()
            .map(|e| e.writer.counters().sealed_frames())
            .min()
            .unwrap_or(0);
        self.health.sealed.store(sealed, Ordering::Release);
        self.note_errors();
    }

    fn note_errors(&self) {
        let errors: u64 = self
            .entries
            .iter()
            .map(|e| e.writer.counters().disk_errors())
            .sum();
        self.health.errors.store(errors, Ordering::Release);
    }

    /// Finishes every file's header and removes the tags. Returns what each
    /// holds; the first failure is returned after every file was tried.
    pub(crate) fn finish(self) -> Result<Vec<Finished>> {
        let mut finished = Vec::with_capacity(self.entries.len());
        let mut failed = None;
        for entry in self.entries {
            let Entry {
                track,
                key,
                path,
                writer,
                ..
            } = entry;
            match writer.finish() {
                Ok(frames) => finished.push(Finished {
                    track,
                    key,
                    path: path.clone(),
                    frames,
                }),
                Err(e) => {
                    failed.get_or_insert(e);
                }
            }
            let _ = std::fs::remove_file(Tag::path_for(&path));
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(finished),
        }
    }
}

/// Starts recording from `source` onto every armed track. Recording begins
/// at the playhead, or at the punch-in point when one is set ahead of it
/// ([`punch_in`]): the transport rolls from where it is and drops into
/// record there. With a punch-out point ([`punch_out`]) it drops out of
/// record there and keeps playing; [`stop`] finishes either way. A
/// recording that starts at the playhead gets the metronome's count-in
/// (`metronome::count_in`): the transport rolls that many bars early with
/// the click sounding. Every armed audio track gets its file in the
/// session folder now (`audio/vox 1.wav`, with a `.recording` tag beside
/// it) and the audio streams into it from the first block.
///
/// Made of: [`track::arm`](crate::track::arm)ed tracks → for a device,
/// `devices::input` if needed and its recording queue → a `wav::Writer`
/// and a tag per armed audio track → a collector thread for a device →
/// [`session::steps::rebuild`] when needed → `EngineHandle::send(SetPunch)`
/// → `EngineHandle::send(Record)`, or `Play` when the punch-in is ahead →
/// [`transport::playhead`] for where it all started.
pub fn start(session: &mut Session, source: Source) -> Result<()> {
    if session.recording.is_some() {
        return Err(AuraError::Message("a recording is already running".into()));
    }
    if crate::track::armed(session).is_empty() {
        return Err(AuraError::Message("arm a track before recording".into()));
    }
    let audio = crate::track::armed(session).iter().any(|t| {
        session
            .memory
            .track(*t)
            .is_ok_and(|t| t.kind == TrackKind::Audio)
    });
    if !audio {
        // MIDI only: nothing to capture from the audio side.
        return begin(session, None);
    }
    match source {
        #[cfg(feature = "devices")]
        Source::Device(device) => {
            if session.input.is_none() {
                crate::devices::input(session, device.as_deref())?;
            }
            let input = session
                .input
                .as_mut()
                .ok_or_else(|| AuraError::Message("the audio input did not open".into()))?;
            let channels = input.channels();
            let blocks = input.begin_recording()?;
            begin(session, Some((blocks, channels, true)))
        }
        Source::Queue { blocks, channels } => begin(session, Some((blocks, channels, false))),
    }
}

/// What every source has in common once the queue exists: `(the queue's
/// consumer end, its channels, whether a thread should drain it)`.
fn begin(session: &mut Session, capture: Option<(CaptureConsumer, usize, bool)>) -> Result<()> {
    let tracks = crate::track::armed(session);
    if session.needs_rebuild {
        session::steps::rebuild(session)?;
    }
    let playhead = transport::playhead(session);
    let punch_in = session
        .memory
        .punch_in()
        .filter(|p| *p > playhead)
        .unwrap_or(playhead);
    let punch_out = session.memory.punch_out().filter(|p| *p > punch_in);
    let (started_at, count_in) = roll_from(session, punch_in, playhead);
    let rolls_before = session.engine.roll_start().map_or(0, |r| r.rolls);
    let channels = capture.as_ref().map_or(0, |c| c.1);
    let health = Health::new(channels);
    let (capture, keys) = match capture {
        None => (None, Vec::new()),
        Some((blocks, channels, threaded)) => {
            let files = steps::open_files(
                session,
                &tracks,
                channels,
                started_at,
                punch_in,
                punch_out,
                Arc::clone(&health),
            )?;
            let keys = files.entries.iter().map(|e| e.key.clone()).collect();
            let capture = if threaded {
                Capture::Threaded(steps::spawn_collector(blocks, files)?)
            } else {
                Capture::Manual { blocks, files }
            };
            (Some(capture), keys)
        }
    };
    if started_at != playhead {
        session.engine.send(EngineCommand::Seek(started_at))?;
    }
    // Incoming MIDI from here on belongs to this recording.
    while session.engine.captured_midi().is_some() {}
    session.engine.send(EngineCommand::CaptureMidi(true))?;
    session.engine.send(EngineCommand::SetPunch {
        punch_in: (started_at < punch_in).then_some(punch_in),
        punch_out,
    })?;
    session.engine.send(if started_at < punch_in {
        EngineCommand::Play
    } else {
        EngineCommand::Record
    })?;
    session.recording = Some(Recording {
        capture,
        midi: Vec::new(),
        started_at,
        punch_in,
        punch_out,
        tracks,
        channels,
        rolls_before,
        keys,
        health,
    });
    if count_in {
        session.mixer.click.set_count_in_until(Some(punch_in));
    }
    Ok(())
}

/// Where the transport starts rolling for a recording that begins at
/// `punch_in`, and whether that is a count-in: the metronome's bars of
/// count-in before the playhead when the recording starts there, otherwise
/// the playhead itself (or the punch-in, if the playhead is past it).
fn roll_from(
    session: &Session,
    punch_in: TimePosition,
    playhead: TimePosition,
) -> (TimePosition, bool) {
    let bars = session.memory.metronome().count_in;
    if bars == 0 || punch_in != playhead {
        return (playhead.min(punch_in), false);
    }
    let tempo = session.memory.tempo_map();
    let beat = tempo.samples_to_beat(punch_in);
    let per_bar = f64::from(tempo.meter_at(beat).numerator());
    let from = BeatPosition::new((beat.beats() - f64::from(bars) * per_bar).max(0.0));
    let from = tempo.beat_to_samples(from);
    (from, from < punch_in)
}

/// Sets or clears the punch-in point: where the next recording begins when
/// it lies ahead of the playhead. Saved with the session; an edit that
/// coalesces like a drag.
///
/// Made of: `SetPunch` through `session::edit`.
pub fn punch_in(session: &mut Session, at: Option<TimePosition>) -> Result<()> {
    session::edit(session, SetPunch::new(PunchPoint::In, at))
}

/// Sets or clears the punch-out point: where a recording drops out of
/// record on its own and keeps playing. Saved with the session; an edit
/// that coalesces like a drag.
///
/// Made of: `SetPunch` through `session::edit`.
pub fn punch_out(session: &mut Session, at: Option<TimePosition>) -> Result<()> {
    session::edit(session, SetPunch::new(PunchPoint::Out, at))
}

/// Writes waiting audio blocks to the recording's files when the source is
/// a queue ([`Source::Queue`]), and takes the MIDI that arrived since the
/// last call either way; a device recording's audio has its own thread.
/// Returns the frames captured in total, or zero when nothing is recording.
/// [`session::steps::render_blocks`] calls this after every block; with a
/// device driving the engine, call it from a timer during long takes, so
/// the MIDI queue (`MIDI_CAPTURE_CAPACITY` messages) never fills.
pub fn collect(session: &mut Session) -> usize {
    let Some(recording) = session.recording.as_mut() else {
        return 0;
    };
    recording.collect();
    recording.gather_midi(&mut session.engine);
    recording.frames()
}

/// Stops the recording and the transport, finishes the files and places one
/// clip per armed track. An audio clip refers to its track's file
/// (`audio/vox 1.wav`, so it is named `vox 1`) from the punch-in point:
/// what was captured before it (the roll towards it, a count-in) stays in
/// the file ahead of the clip's offset, and what came after the punch-out
/// stays past its end, so the clip can be trimmed out over it later like
/// any other. The file holds the channels the track's [`Input`] selects
/// (tracks without one get the whole capture). A device take lands where
/// it was played: the file's first frame is placed by the device clock,
/// and a capture that began after the transport rolled gives a clip that
/// starts that much later. Returns the new clips (none if nothing was
/// captured). The whole placement is one undo step called "Record":
/// undoing it removes the clips and deletes the files it wrote, redo
/// writes them again from the audio kept in memory.
///
/// Made of: `EngineHandle::send(Stop)` → drain the queue or
/// `Collector::finish` (and hand the audio input its queue back) →
/// `wav::Writer::finish` per file, tags removed → the file's first frame
/// lined up with the `RollStart` when the device clock gives one → per
/// track, the clip's start, offset into the file and length from the punch
/// points → `wav::read` into `AudioCache::insert_shared` →
/// [`clip::add`](crate::clip::add), inside one undo group; rebuilds the
/// mixer so the clips play back on their strips.
pub fn stop(session: &mut Session) -> Result<Vec<ClipId>> {
    let recording = session
        .recording
        .take()
        .ok_or_else(|| AuraError::Message("nothing is being recorded".into()))?;
    session.mixer.click.set_count_in_until(None);
    session.engine.send(EngineCommand::CaptureMidi(false))?;
    let ended_at = session.engine.position();
    session.engine.send(EngineCommand::SetPunch {
        punch_in: None,
        punch_out: None,
    })?;
    session.engine.send(EngineCommand::Stop)?;
    let Recording {
        capture,
        mut midi,
        started_at,
        punch_in,
        punch_out,
        tracks,
        rolls_before,
        ..
    } = recording;
    while let Some(message) = session.engine.captured_midi() {
        midi.push(message);
    }
    let rate = session.memory.sample_rate();
    // The audio side, when an audio track was armed.
    let mut finished = Vec::new();
    let mut origin = started_at.samples();
    if let Some(capture) = capture {
        let files = match capture {
            Capture::Manual {
                mut blocks,
                mut files,
            } => {
                files.append(&mut blocks);
                files
            }
            Capture::Threaded(collector) => {
                let (files, blocks) = collector.finish()?;
                #[cfg(feature = "devices")]
                if let Some(input) = session.input.as_mut() {
                    input.end_recording(blocks);
                }
                #[cfg(not(feature = "devices"))]
                drop(blocks);
                files
            }
        };
        // Frame 0 of the files is where the transport began rolling, when
        // the device clock can say so.
        let lead = steps::capture_lead(
            files.first_us(),
            session.engine.roll_start(),
            rolls_before,
            started_at,
            rate,
        );
        origin -= lead;
        finished = files.finish()?;
    }
    session.history.begin_group("Record");
    let mut clips = Vec::with_capacity(tracks.len());
    for track in &tracks {
        let kind = session
            .memory
            .track(*track)
            .map(|t| t.kind)
            .unwrap_or(TrackKind::Audio);
        if kind == TrackKind::Midi {
            let until = punch_out.unwrap_or(ended_at);
            let Some(clip) = steps::midi_clip(session, *track, &midi, punch_in, until) else {
                continue;
            };
            let id = clip.id();
            if let Err(e) = session::structural(session, AddClip::new(*track, clip)) {
                session.history.cancel_group(&mut session.memory)?;
                return Err(e);
            }
            clips.push(id);
            continue;
        }
        let Some(file) = finished.iter().find(|f| f.track == *track) else {
            continue;
        };
        let Some((start, offset, length)) =
            steps::placement(origin, file.frames, punch_in, punch_out)
        else {
            // Nothing of it falls inside the take: an empty file nobody refers to.
            let _ = std::fs::remove_file(&file.path);
            continue;
        };
        let placed = wav::read(&file.path)
            .map(|(audio, _)| Arc::new(audio))
            .and_then(|audio| {
                session
                    .audio
                    .insert_shared(file.key.clone(), Arc::clone(&audio));
                let id = session.memory.allocate_clip_id();
                let name = file
                    .key
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let clip = Clip::with_id(
                    id,
                    name,
                    start,
                    length,
                    ClipSource::Audio {
                        path: file.key.clone(),
                        offset,
                    },
                )?;
                let folder = session.folder.clone();
                session::structural(
                    session,
                    RecordClip {
                        add: AddClip::new(*track, clip),
                        folder,
                        key: file.key.clone(),
                        audio,
                        rate,
                    },
                )
                .map(|()| id)
            });
        match placed {
            Ok(id) => clips.push(id),
            Err(e) => {
                session.history.cancel_group(&mut session.memory)?;
                return Err(e);
            }
        }
    }
    session.history.end_group();
    session.needs_rebuild = true;
    Ok(clips)
}

/// Recordings a crash left unfinished in the session folder: files that
/// still have their `.recording` tag beside them, as keys (`audio/vox
/// 1.wav`). [`recover`] places them; until then they are not clips and
/// `session::open` only notes them in the log.
pub fn unfinished(session: &Session) -> Vec<PathBuf> {
    steps::tags(&session.folder)
        .into_iter()
        .map(|(_, tag)| tag.key)
        .collect()
}

/// Places the recordings a crash left unfinished ([`unfinished`]): each
/// file's header is finished in place (`wav::repair`), its audio loaded,
/// and a clip placed on the track it was recording for, from the punch-in
/// point the tag remembers, lined up as if the capture began where the
/// transport rolled (the device clock that would say otherwise is gone
/// with the crash). The tags are removed; the files stay, and the
/// placement is one undo step called "Recover" whose undo removes the
/// clips but deletes nothing. A tag whose track is gone, or whose file
/// cannot be read, is left where it is and noted in the log. Refused while
/// a recording runs.
///
/// Made of: the tags in `audio/` → `wav::repair` → `wav::read` into
/// `AudioCache::insert_shared` → `AddClip` through `session::structural`
/// per file, inside one undo group; rebuilds the mixer.
pub fn recover(session: &mut Session) -> Result<Vec<ClipId>> {
    if session.recording.is_some() {
        return Err(AuraError::Message(
            "stop the recording before recovering older ones".into(),
        ));
    }
    let tags = steps::tags(&session.folder);
    if tags.is_empty() {
        return Ok(Vec::new());
    }
    let rate = session.memory.sample_rate();
    session.history.begin_group("Recover");
    let mut clips = Vec::new();
    for (tag_file, tag) in tags {
        let path = session.folder.join(&tag.key);
        let track = TrackId::from_raw(tag.track);
        if session.memory.track(track).is_err() {
            log::warn!(
                "{}: its track {} is gone; left as it is",
                tag.key.display(),
                tag.track
            );
            continue;
        }
        let frames = match wav::repair(&path) {
            Ok(frames) => frames,
            Err(e) => {
                log::warn!("{}: cannot repair: {e}", tag.key.display());
                continue;
            }
        };
        let punch_in = TimePosition::from_samples(tag.punch_in);
        let punch_out = tag.punch_out.map(TimePosition::from_samples);
        let Some((start, offset, length)) =
            steps::placement(tag.started_at, frames, punch_in, punch_out)
        else {
            log::info!(
                "{}: nothing of it inside the take; tag removed",
                tag.key.display()
            );
            if frames == 0 {
                let _ = std::fs::remove_file(&path);
            }
            let _ = std::fs::remove_file(&tag_file);
            continue;
        };
        let (audio, file_rate) = match wav::read(&path) {
            Ok(read) => read,
            Err(e) => {
                log::warn!("{}: cannot read: {e}", tag.key.display());
                continue;
            }
        };
        if file_rate != rate {
            log::warn!(
                "{}: at {} Hz, the session is at {} Hz; left as it is",
                tag.key.display(),
                file_rate.hz(),
                rate.hz()
            );
            continue;
        }
        session.audio.insert(tag.key.clone(), audio);
        let id = session.memory.allocate_clip_id();
        let name = tag
            .key
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let placed = Clip::with_id(
            id,
            name,
            start,
            length,
            ClipSource::Audio {
                path: tag.key.clone(),
                offset,
            },
        )
        .and_then(|clip| session::structural(session, AddClip::new(track, clip)));
        if let Err(e) = placed {
            session.history.cancel_group(&mut session.memory)?;
            return Err(e);
        }
        let _ = std::fs::remove_file(&tag_file);
        clips.push(id);
    }
    session.history.end_group();
    session.needs_rebuild = true;
    Ok(clips)
}

/// Places a recorded clip and owns its file: applying writes the WAV when
/// it is not there (a redo after an undo deleted it), reverting deletes
/// it, so an undone recording leaves nothing on disk. The audio stays in
/// `Session::audio` either way.
struct RecordClip {
    add: AddClip,
    folder: PathBuf,
    key: PathBuf,
    audio: Arc<PlanarBuffer>,
    rate: SampleRate,
}

impl Command<Memory> for RecordClip {
    fn label(&self) -> &str {
        "Record"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        if !self.folder.join(&self.key).exists() {
            crate::session::folder::write_audio(&self.folder, &self.key, &self.audio, self.rate)?;
        }
        self.add.apply(memory)
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        self.add.revert(memory)?;
        match std::fs::remove_file(self.folder.join(&self.key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AuraError::Message(format!(
                "cannot delete {}: {e}",
                self.key.display()
            ))),
        }
    }
}

/// The parts `start`, `collect`, `stop` and `recover` are made of.
pub(crate) mod steps {
    use super::*;

    /// Opens one file per armed audio track among `tracks`, each holding
    /// the channels its [`Input`] selects from a capture of `channels`
    /// channels (all of them without one), with a tag beside it. The file
    /// is created at once, so the next track's name sees it taken.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn open_files(
        session: &Session,
        tracks: &[TrackId],
        channels: usize,
        started_at: TimePosition,
        punch_in: TimePosition,
        punch_out: Option<TimePosition>,
        health: Arc<Health>,
    ) -> Result<Files> {
        let rate = session.memory.sample_rate();
        let audio_dir = session.folder.join(crate::session::folder::AUDIO_DIR);
        std::fs::create_dir_all(&audio_dir).map_err(|e| {
            AuraError::Message(format!("cannot create {}: {e}", audio_dir.display()))
        })?;
        let mut entries = Vec::new();
        for track in tracks {
            let Ok(t) = session.memory.track(*track) else {
                continue;
            };
            if t.kind != TrackKind::Audio {
                continue;
            }
            let pick: Vec<usize> = match t.state.input {
                None => (0..channels.max(1)).collect(),
                Some(Input::Mono(c)) => vec![c],
                Some(Input::Stereo(l, r)) => vec![l, r],
            };
            let key = audio_key(session, *track, &entries)?;
            let path = session.folder.join(&key);
            let writer = wav::Writer::create(&path, pick.len(), rate, wav::WavFormat::Float32)?;
            let tag = Tag {
                format_version: TAG_FORMAT_VERSION,
                track: track.raw(),
                key: key.clone(),
                started_at: started_at.samples(),
                punch_in: punch_in.samples(),
                punch_out: punch_out.map(|p| p.samples()),
                sample_rate: rate.hz(),
                channels: pick.len(),
                first_us: None,
            };
            tag.write(&path)?;
            entries.push(Entry {
                track: *track,
                key,
                path,
                pick,
                writer,
                tag,
            });
        }
        Ok(Files {
            entries,
            first_us: None,
            frames: 0,
            since_flush: 0,
            flush_every: rate.hz() as usize,
            silence: vec![0.0; session.engine.max_frames()],
            health,
        })
    }

    /// Starts a thread that writes `blocks` into `files` as they arrive.
    /// The thread sleeps briefly when the queue is empty and exits after
    /// [`Collector::finish`].
    #[cfg_attr(not(feature = "devices"), allow(dead_code))]
    pub(crate) fn spawn_collector(
        mut blocks: CaptureConsumer,
        mut files: Files,
    ) -> Result<Collector> {
        let dropped = blocks.dropped_handle();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("aura recording collector".into())
            .spawn(move || loop {
                let appended = files.append(&mut blocks);
                if stop_t.load(Ordering::Acquire) {
                    // The input has stopped feeding us; take what is left.
                    files.append(&mut blocks);
                    break (files, blocks);
                }
                if appended == 0 {
                    std::thread::sleep(core::time::Duration::from_millis(2));
                }
            })
            .map_err(|e| {
                AuraError::Message(format!("cannot start the recording collector: {e}"))
            })?;
        Ok(Collector {
            dropped,
            stop,
            handle: Some(handle),
        })
    }

    /// How many input channels the audio tracks' inputs name: one past the
    /// highest channel among them, and at least one.
    #[cfg_attr(not(feature = "devices"), allow(dead_code))]
    pub(crate) fn input_width(session: &Session) -> usize {
        session
            .memory
            .tracks()
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && !t.state.archived)
            .filter_map(|t| t.state.input)
            .map(Input::width)
            .max()
            .unwrap_or(1)
            .max(1)
    }

    /// Frames of the capture that came before the transport began rolling
    /// (positive), or that are missing from its start (negative), from the
    /// first block's device-clock stamp and the engine's [`RollStart`].
    /// Zero when either side has no clock, or when the roll start is not
    /// this recording's (`rolls` has not moved past `rolls_before`).
    pub(crate) fn capture_lead(
        first_us: Option<u64>,
        roll: Option<RollStart>,
        rolls_before: u64,
        started_at: TimePosition,
        rate: SampleRate,
    ) -> i64 {
        let (Some(first_us), Some(roll)) = (first_us, roll) else {
            return 0;
        };
        if roll.rolls <= rolls_before {
            return 0;
        }
        let elapsed_us = i128::from(roll.instant_us) - i128::from(first_us);
        let to_roll = (elapsed_us * i128::from(rate.hz()) / 1_000_000) as i64;
        // Frame `to_roll` of the capture is the roll position; the take
        // begins at `started_at`.
        to_roll + (started_at.samples() - roll.position.samples())
    }

    /// Where the clip for a file goes: `(start, offset into the file,
    /// length)`, for a file of `frames` frames whose frame 0 is at
    /// timeline sample `origin`, inside the punch points. The clip starts
    /// at the punch-in, or at the file's first frame when the capture began
    /// after it, and ends at the punch-out or the file's end. `None` when
    /// nothing of the file falls inside.
    pub(crate) fn placement(
        origin: i64,
        frames: u64,
        punch_in: TimePosition,
        punch_out: Option<TimePosition>,
    ) -> Option<(TimePosition, TimeDuration, TimeDuration)> {
        let file_end = origin + frames as i64;
        let start = punch_in.samples().max(origin);
        let end = punch_out.map_or(file_end, |out| out.samples().min(file_end));
        if end <= start {
            return None;
        }
        Some((
            TimePosition::from_samples(start),
            TimeDuration::from_samples(start - origin),
            TimeDuration::from_samples(end - start),
        ))
    }

    /// The tags in the session folder's `audio/`: `(tag file, tag)`, in
    /// name order. A file that is not a tag is noted in the log and skipped.
    pub(crate) fn tags(folder: &Path) -> Vec<(PathBuf, Tag)> {
        let dir = folder.join(crate::session::folder::AUDIO_DIR);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut tags: Vec<(PathBuf, Tag)> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.to_string_lossy().ends_with(TAG_SUFFIX))
            .filter_map(|p| match Tag::read(&p) {
                Ok(tag) => Some((p, tag)),
                Err(e) => {
                    log::warn!("{e}");
                    None
                }
            })
            .collect();
        tags.sort_by(|a, b| a.0.cmp(&b.0));
        tags
    }

    /// The notes among captured MIDI inside `[from, until)`, as a clip
    /// starting at `from` would hold them: beats from `from`, a note-on
    /// closed by its note-off, a retrigger closing the one before, and
    /// whatever is still down closed at `until`. Other messages are left
    /// out, and so are other channels when `only` names one.
    pub(crate) fn notes_from(
        midi: &[CapturedMidi],
        from: TimePosition,
        until: TimePosition,
        tempo: &TempoMap,
        only: Option<u8>,
    ) -> Vec<MidiNote> {
        let start_beat = tempo.samples_to_beat(from).beats();
        let beat_at = |at: TimePosition| tempo.samples_to_beat(at).beats() - start_beat;
        // (channel, note, onset beat, velocity)
        let mut down: Vec<(u8, u8, f64, u8)> = Vec::new();
        let mut notes = Vec::new();
        let mut close = |down: &mut Vec<(u8, u8, f64, u8)>, channel: u8, note: u8, at: f64| {
            if let Some(i) = down.iter().position(|d| d.0 == channel && d.1 == note) {
                let (_, _, on, velocity) = down.remove(i);
                notes.push(MidiNote {
                    beat: on,
                    length: (at - on).max(1.0 / 960.0),
                    note,
                    velocity,
                    channel,
                });
            }
        };
        let wanted = |m: &&CapturedMidi| match (m.message, only) {
            (
                MidiMessage::NoteOn { channel, .. } | MidiMessage::NoteOff { channel, .. },
                Some(only),
            ) => channel.index() == only,
            _ => true,
        };
        let mut sorted: Vec<&CapturedMidi> = midi
            .iter()
            .filter(|m| m.position >= from && m.position < until)
            .filter(wanted)
            .collect();
        sorted.sort_by_key(|m| m.position);
        for m in sorted {
            let at = beat_at(m.position);
            match m.message {
                MidiMessage::NoteOn {
                    channel,
                    note,
                    velocity,
                } => {
                    close(&mut down, channel.index(), note, at);
                    down.push((channel.index(), note, at, velocity));
                }
                MidiMessage::NoteOff { channel, note, .. } => {
                    close(&mut down, channel.index(), note, at);
                }
                _ => {}
            }
        }
        let end = beat_at(until);
        for (channel, note, on, velocity) in down {
            notes.push(MidiNote {
                beat: on,
                length: (end - on).max(1.0 / 960.0),
                note,
                velocity,
                channel,
            });
        }
        notes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        notes
    }

    /// The MIDI clip a recording leaves on `track`: the notes captured
    /// between `from` and `until`, named after the track. `None` when no
    /// note arrived.
    pub(crate) fn midi_clip(
        session: &mut Session,
        track: TrackId,
        midi: &[CapturedMidi],
        from: TimePosition,
        until: TimePosition,
    ) -> Option<Clip> {
        let only = session
            .memory
            .track(track)
            .ok()
            .and_then(|t| t.state.midi_channel);
        let notes = notes_from(midi, from, until, session.memory.tempo_map(), only);
        let length = TimeDuration::from_samples(until.samples() - from.samples());
        if notes.is_empty() || length.samples() <= 0 {
            return None;
        }
        let name = session
            .memory
            .track(track)
            .map(|t| t.name.clone())
            .unwrap_or_else(|_| "midi".into());
        let id = session.memory.allocate_clip_id();
        Clip::with_id(id, name, from, length, ClipSource::Midi { notes }).ok()
    }

    /// A file for a new recording on `track` that nothing uses yet, as a
    /// path relative to the session folder: `audio/vox 1.wav`,
    /// `audio/vox 2.wav`, ... The clip takes its stem as its name. Taken
    /// names are those in memory, the files already in the folder, and the
    /// files this recording opened so far.
    pub(crate) fn audio_key(
        session: &Session,
        track: TrackId,
        opened: &[Entry],
    ) -> Result<PathBuf> {
        let base = session.memory.track(track)?.name.clone();
        let mut n = 1;
        loop {
            let key = PathBuf::from(format!(
                "{}/{base} {n}.wav",
                crate::session::folder::AUDIO_DIR
            ));
            let in_memory = session.audio.get(&key).is_some();
            let on_disk = session.folder.join(&key).exists();
            let opening = opened.iter().any(|e| e.key == key);
            if !in_memory && !on_disk && !opening {
                return Ok(key);
            }
            n += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::io::capture::capture_queue;
    use crate::primitives::{SampleRate, TimeDuration};
    use crate::track;
    use crate::transport;

    const R: SampleRate = SampleRate::HZ_48K;

    /// A "microphone": a capture queue we fill by hand with a constant value.
    fn feed(
        producer: &mut crate::primitives::io::capture::CaptureProducer,
        blocks: usize,
        value: f32,
    ) {
        let mut block = PlanarBuffer::new(1, 64);
        block.fill(value);
        for i in 0..blocks {
            assert!(producer.push(&block, i as u64 * 1_000), "queue has room");
        }
    }

    #[test]
    fn a_recording_becomes_a_clip_on_every_armed_track() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let gtr = track::add(&mut session, "gtr", TrackKind::Audio).unwrap();
        let _idle = track::add(&mut session, "idle", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        track::arm(&mut session, gtr, true).unwrap();
        assert_eq!(crate::track::armed(&session), [vox, gtr]);
        assert!(session.recording.is_none());

        // Roll to 640 frames, then punch in there.
        transport::start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        assert!(session.recording.is_some());
        assert_eq!(
            session.recording.as_ref().unwrap().punch_in(),
            TimePosition::from_samples(640)
        );
        session::steps::render_blocks(&mut session, 5).unwrap();
        assert_eq!(
            transport::state(&session),
            crate::primitives::transport::TransportState::Recording
        );
        feed(&mut mic, 5, 0.25); // 320 frames of "singing"

        let clips = stop(&mut session).unwrap();
        assert_eq!(clips.len(), 2);
        assert!(session.recording.is_none());
        for (clip, name) in clips.iter().zip(["vox 1", "gtr 1"]) {
            let c = session.memory.clip(*clip).unwrap();
            assert_eq!(c.start(), TimePosition::from_samples(640));
            assert_eq!(c.length(), TimeDuration::from_samples(320));
            assert_eq!(c.name, name);
        }
        assert_eq!(session.memory.clip_track(clips[0]).unwrap(), vox);
        assert_eq!(session.memory.clip_track(clips[1]).unwrap(), gtr);
        assert_eq!(session.audio.len(), 2, "one entry per clip");
        assert_eq!(
            session
                .audio
                .get("audio/vox 1.wav".as_ref())
                .unwrap()
                .frames(),
            320,
            "each track has its own file"
        );
        assert!(matches!(
            &session.memory.clip(clips[0]).unwrap().source,
            ClipSource::Audio { offset, .. } if offset.samples() == 0
        ));
        assert_eq!(session.history.undo_label(), Some("Record"));

        // Stopping stopped the transport; the clips play back from their place.
        session::steps::render_blocks(&mut session, 1).unwrap();
        assert_eq!(
            transport::state(&session),
            crate::primitives::transport::TransportState::Stopped
        );
        transport::seek(&mut session, TimePosition::from_samples(640)).unwrap();
        transport::start(&mut session).unwrap();
        let heard = session::steps::render_blocks(&mut session, 4).unwrap();
        assert!(heard.channel(0)[200].abs() > 0.2, "two tracks of the clip");
        assert_eq!(
            session.mixer.strips.len(),
            4,
            "one handle per track and the master"
        );

        // One undo removes both clips.
        assert!(session::undo(&mut session).unwrap());
        assert!(session.memory.clip(clips[0]).is_err() && session.memory.clip(clips[1]).is_err());
        assert!(session::redo(&mut session).unwrap());
        assert!(session.memory.clip(clips[0]).is_ok());
    }

    #[test]
    fn clips_get_distinct_names_and_empty_recordings_leave_no_clip() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        for i in 1..=2 {
            let (mut mic, blocks) = capture_queue(2, 64, 4).unwrap();
            start(
                &mut session,
                Source::Queue {
                    blocks,
                    channels: 2,
                },
            )
            .unwrap();
            feed_stereo(&mut mic, 2);
            let clips = stop(&mut session).unwrap();
            assert_eq!(
                session.memory.clip(clips[0]).unwrap().name,
                format!("vox {i}")
            );
        }
        assert_eq!(session.audio.len(), 2);
        let (_mic, blocks) = capture_queue(2, 64, 4).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 2,
            },
        )
        .unwrap();
        assert!(
            stop(&mut session).unwrap().is_empty(),
            "nothing captured, nothing placed"
        );
        assert_eq!(session.memory.clip_count(), 2);
    }

    fn feed_stereo(producer: &mut crate::primitives::io::capture::CaptureProducer, blocks: usize) {
        let block = PlanarBuffer::new(2, 64);
        for i in 0..blocks {
            assert!(producer.push(&block, i as u64));
        }
    }

    #[test]
    fn tracks_record_the_input_channels_they_chose() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let gtr = track::add(&mut session, "gtr", TrackKind::Audio).unwrap();
        let room = track::add(&mut session, "room", TrackKind::Audio).unwrap();
        for t in [vox, gtr, room] {
            track::arm(&mut session, t, true).unwrap();
        }
        track::input(&mut session, vox, Some(Input::Mono(0))).unwrap();
        track::input(&mut session, gtr, Some(Input::Mono(1))).unwrap();
        track::input(&mut session, room, Some(Input::Stereo(0, 1))).unwrap();
        assert_eq!(steps::input_width(&session), 2);
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        assert!(track::input(&mut session, keys, Some(Input::Mono(0))).is_err());

        // A two-channel capture: 0.25 on the left, 0.5 on the right.
        let (mut mic, blocks) = capture_queue(2, 64, 8).unwrap();
        let mut block = PlanarBuffer::new(2, 64);
        block.channel_mut(0).fill(0.25);
        block.channel_mut(1).fill(0.5);
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 2,
            },
        )
        .unwrap();
        for i in 0..3 {
            assert!(mic.push(&block, i));
        }
        let clips = stop(&mut session).unwrap();
        assert_eq!(clips.len(), 3);
        {
            let audio = |name: &str| {
                session
                    .audio
                    .get(format!("audio/{name}.wav").as_ref())
                    .unwrap()
            };
            assert_eq!(audio("vox 1").channels(), 1);
            assert!(audio("vox 1").channel(0).iter().all(|&s| s == 0.25));
            assert_eq!(audio("gtr 1").channels(), 1);
            assert!(audio("gtr 1").channel(0).iter().all(|&s| s == 0.5));
            assert_eq!(audio("room 1").channels(), 2);
            assert!(audio("room 1").channel(1).iter().all(|&s| s == 0.5));
            assert_eq!(audio("room 1").frames(), 192);
        }
        // A channel the capture did not have is silence.
        let far = track::add(&mut session, "far", TrackKind::Audio).unwrap();
        track::arm(&mut session, far, true).unwrap();
        track::input(&mut session, far, Some(Input::Mono(5))).unwrap();
        let (mut mic, blocks) = capture_queue(2, 64, 8).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 2,
            },
        )
        .unwrap();
        assert!(mic.push(&block, 0));
        let clips = stop(&mut session).unwrap();
        assert_eq!(clips.len(), 4);
        let silent = session.audio.get("audio/far 1.wav".as_ref()).unwrap();
        assert!(silent.is_silent() && silent.frames() == 64 && silent.channels() == 1);
        // No input set: the whole capture.
        track::input(&mut session, vox, None).unwrap();
        assert_eq!(steps::input_width(&session), 6, "far still needs six");
    }

    #[test]
    fn errors_are_explicit() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let bus = track::add(&mut session, "bus", TrackKind::Bus).unwrap();
        assert!(
            track::arm(&mut session, bus, true).is_err(),
            "buses are not armed"
        );
        assert!(
            matches!(stop(&mut session), Err(AuraError::Message(_))),
            "nothing recording"
        );
        let (_mic, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(
            start(
                &mut session,
                Source::Queue {
                    blocks,
                    channels: 1
                }
            )
            .is_err(),
            "no armed track"
        );
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        let (_a, blocks) = capture_queue(1, 64, 2).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        let (_b, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(
            start(
                &mut session,
                Source::Queue {
                    blocks,
                    channels: 1
                }
            )
            .is_err(),
            "already recording"
        );
        let _ = format!("{:?}", session.recording.as_ref().unwrap());
    }

    #[test]
    fn a_collector_thread_gathers_the_take_by_itself() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        // A queue of 64 blocks fed with 200, paced like a device, and no
        // `collect` calls: the thread keeps up on its own. (A failed push
        // counts as a drop, so the accounting below must balance exactly.)
        let (mut mic, blocks) = capture_queue(1, 64, 64).unwrap();
        begin(&mut session, Some((blocks, 1, true))).unwrap();
        assert!(matches!(
            session.recording.as_ref().unwrap().capture,
            Some(Capture::Threaded(_))
        ));
        let mut block = PlanarBuffer::new(1, 64);
        block.fill(0.5);
        for i in 0..200u64 {
            mic.push(&block, i);
            std::thread::sleep(core::time::Duration::from_micros(300));
        }
        // `collect` has nothing to do for a threaded capture; it reports.
        let _ = collect(&mut session);
        let _ = format!("{:?}", session.recording.as_ref().unwrap());
        let clips = stop(&mut session).unwrap();
        let dropped = mic.dropped() as usize;
        let clip = session.memory.clip(clips[0]).unwrap();
        assert_eq!(
            clip.length(),
            TimeDuration::from_samples(((200 - dropped) * 64) as i64),
            "every block pushed was gathered or counted as dropped"
        );
        assert!(
            dropped < 100,
            "the collector fell far behind: {dropped} dropped"
        );
        let audio = session.audio.get("audio/vox 1.wav".as_ref()).unwrap();
        assert!(audio.channel(0).iter().all(|&s| s == 0.5));

        // An unfinished collector just lets its thread go; its file and
        // tag stay for `recover`.
        let (_mic, blocks) = capture_queue(1, 64, 2).unwrap();
        begin(&mut session, Some((blocks, 1, true))).unwrap();
        let keys = session.recording.as_ref().unwrap().files().to_vec();
        assert_eq!(keys, [PathBuf::from("audio/vox 2.wav")]);
        session.recording = None;
        assert!(session.folder.join("audio/vox 2.wav.recording").is_file());
        assert_eq!(unfinished(&session), keys);
    }

    #[test]
    fn files_pad_missing_channels_and_flush_on_a_cadence() {
        let mut session = session::create(session::scratch("files"), R, 4).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        let health = Health::new(2);
        // A capture said to have two channels; the blocks bring one.
        let mut files = steps::open_files(
            &session,
            &[vox],
            2,
            TimePosition::ZERO,
            TimePosition::ZERO,
            None,
            Arc::clone(&health),
        )
        .unwrap();
        files.flush_every = 8;
        let (mut mic, mut blocks) = capture_queue(1, 4, 4).unwrap();
        let mut block = PlanarBuffer::new(1, 4);
        block.fill(1.0);
        mic.push(&block, 7);
        assert_eq!(files.append(&mut blocks), 4);
        assert_eq!(files.frames(), 4);
        assert_eq!(files.first_us(), Some(7));
        assert_eq!(health.sealed.load(Ordering::Acquire), 0, "not flushed yet");
        mic.push(&block, 8);
        assert_eq!(files.append(&mut blocks), 4);
        assert_eq!(
            health.sealed.load(Ordering::Acquire),
            8,
            "flushed at 8 frames"
        );
        assert_eq!(files.append(&mut blocks), 0);
        assert!(blocks.is_empty());
        let tag = session.folder.join("audio/vox 1.wav.recording");
        assert!(tag.is_file());
        let finished = files.finish().unwrap();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].frames, 8);
        assert!(!tag.exists(), "finished: the tag is gone");
        let (audio, _) = wav::read(&finished[0].path).unwrap();
        assert_eq!(audio.channels(), 2);
        assert_eq!(audio.channel(0), &[1.0; 8]);
        assert_eq!(audio.channel(1), &[0.0; 8]);
    }

    #[test]
    fn collecting_as_you_go_outlasts_the_queue() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        // A queue of only two blocks; the recording is ten blocks long.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        for i in 0..10 {
            feed(&mut mic, 1, 0.5);
            // Advancing the engine collects for us (a UI timer would call
            // `collect` when a device drives the engine).
            session::steps::render_blocks(&mut session, 1).unwrap();
            assert_eq!(session.recording.as_ref().unwrap().frames(), (i + 1) * 64);
        }
        assert_eq!(session.recording.as_ref().unwrap().dropped(), 0);
        assert_eq!(collect(&mut session), 640);
        let clips = stop(&mut session).unwrap();
        assert_eq!(
            session.memory.clip(clips[0]).unwrap().length(),
            TimeDuration::from_samples(640)
        );
        assert_eq!(collect(&mut session), 0, "nothing recording");

        // Without collecting, the input side reports what it had to drop.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        feed_lossy(&mut mic, 5, 0.5);
        assert_eq!(session.recording.as_ref().unwrap().dropped(), 3);
        stop(&mut session).unwrap();
    }

    fn feed_lossy(
        producer: &mut crate::primitives::io::capture::CaptureProducer,
        blocks: usize,
        value: f32,
    ) {
        let mut block = PlanarBuffer::new(1, 64);
        block.fill(value);
        for i in 0..blocks {
            let _ = producer.push(&block, i as u64);
        }
    }

    #[test]
    fn a_recording_is_written_into_the_session_folder_as_it_stops() {
        let dir = session::scratch("rec-folder");
        let mut session = session::create(&dir, R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();

        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        feed(&mut mic, 5, 0.25);
        let clips = stop(&mut session).unwrap();
        let file = dir.join("audio/vox 1.wav");
        assert!(file.is_file(), "written as it stopped, before any save");
        let (read, rate) = crate::primitives::io::wav::read(&file).unwrap();
        assert_eq!(rate, R);
        assert_eq!(read.frames(), 320);
        assert_eq!(read.channels(), 1);
        assert!(read.channel(0).iter().all(|&s| s == 0.25));
        let clip = session.memory.clip(clips[0]).unwrap();
        assert_eq!(clip.name, "vox 1");
        assert!(matches!(
            &clip.source,
            crate::primitives::session::ClipSource::Audio { path, .. }
                if path == std::path::Path::new("audio/vox 1.wav")
        ));

        // Undoing the recording deletes its file; redo writes it again. After
        // an undo nothing refers to the name, so a reopened session reuses it.
        assert!(session::undo(&mut session).unwrap());
        assert!(!file.exists(), "undo deletes the recording's file");
        assert!(session::redo(&mut session).unwrap());
        assert!(file.is_file(), "redo writes it again");
        assert!(session::undo(&mut session).unwrap());
        assert!(!file.exists());
        session::save(&mut session).unwrap();
        let mut again = session::open(&dir, 64).unwrap();
        assert!(
            again.audio.get("audio/vox 1.wav".as_ref()).is_none(),
            "unreferenced"
        );
        track::arm(&mut again, vox, true).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        start(
            &mut again,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        feed(&mut mic, 2, 0.5);
        let clips = stop(&mut again).unwrap();
        assert_eq!(again.memory.clip(clips[0]).unwrap().name, "vox 1");
        assert!(file.is_file());

        // Reopening loads the recording and it plays.
        session::save(&mut again).unwrap();
        let mut played = session::open(&dir, 64).unwrap();
        assert_eq!(played.audio.len(), 1);
        transport::seek(&mut played, TimePosition::ZERO).unwrap();
        transport::start(&mut played).unwrap();
        assert!(
            session::steps::render_blocks(&mut played, 4)
                .unwrap()
                .peak()
                > 0.2
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_crash_leaves_files_that_recover_places() {
        let dir = session::scratch("recover");
        let mut session = session::create(&dir, R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        track::arm(&mut session, keys, true).unwrap();
        assert!(unfinished(&session).is_empty());
        assert!(
            recover(&mut session).unwrap().is_empty(),
            "nothing to recover"
        );

        // Roll to 640, record 10 blocks there, then "crash": the session
        // goes away without `stop`.
        transport::start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        assert_eq!(
            session.recording.as_ref().unwrap().files(),
            [PathBuf::from("audio/vox 1.wav")]
        );
        for i in 0..10 {
            feed(&mut mic, 1, 0.25);
            session::steps::render_blocks(&mut session, 1).unwrap();
            let r = session.recording.as_ref().unwrap();
            assert_eq!(r.frames(), (i + 1) * 64);
            assert_eq!(r.peaks(), [0.25]);
            assert!(r.since_last_block().is_some());
            assert_eq!(r.disk_errors(), 0);
        }
        assert_eq!(
            session.recording.as_ref().unwrap().sealed_frames(),
            0,
            "a second has not passed in frames"
        );
        assert!(recover(&mut session).is_err(), "refused while recording");
        session::save(&mut session).unwrap();
        drop(session);
        let file = dir.join("audio/vox 1.wav");
        assert!(file.is_file() && dir.join("audio/vox 1.wav.recording").is_file());

        // Reopen: the recording is noted, not placed, until `recover`.
        let mut again = session::open(&dir, 64).unwrap();
        assert_eq!(again.memory.clip_count(), 0);
        assert_eq!(unfinished(&again), [PathBuf::from("audio/vox 1.wav")]);
        let clips = recover(&mut again).unwrap();
        assert_eq!(clips.len(), 1);
        let clip = again.memory.clip(clips[0]).unwrap();
        assert_eq!(clip.name, "vox 1");
        assert_eq!(clip.start(), TimePosition::from_samples(640));
        assert_eq!(clip.length(), TimeDuration::from_samples(640));
        assert_eq!(again.memory.clip_track(clips[0]).unwrap(), vox);
        assert!(unfinished(&again).is_empty(), "the tag is gone");
        assert!(!dir.join("audio/vox 1.wav.recording").exists());
        let (audio, _) = wav::read(&file).unwrap();
        assert_eq!(audio.frames(), 640, "repaired to what was written");
        assert!(audio.channel(0).iter().all(|&s| s == 0.25));
        assert_eq!(again.history.undo_label(), Some("Recover"));
        assert!(session::undo(&mut again).unwrap());
        assert!(file.is_file(), "undoing a recovery deletes nothing");
        assert!(recover(&mut again).unwrap().is_empty(), "no tags left");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn punch_points_decide_where_a_recording_begins_and_ends() {
        use crate::primitives::transport::TransportState;
        let mut session = session::create(session::scratch("punch"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        punch_in(&mut session, Some(TimePosition::from_samples(640))).unwrap();
        punch_out(&mut session, Some(TimePosition::from_samples(960))).unwrap();
        assert_eq!(
            session.memory.punch_in(),
            Some(TimePosition::from_samples(640))
        );
        let json = session.memory.to_json().unwrap();
        assert_eq!(
            crate::primitives::session::Memory::from_json(&json)
                .unwrap()
                .punch_out(),
            Some(TimePosition::from_samples(960))
        );

        // Start at zero: the transport rolls, drops into record at 640 and
        // out at 960; the clip holds exactly that span.
        let (mut mic, blocks) = capture_queue(1, 64, 64).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        let recording = session.recording.as_ref().unwrap();
        assert_eq!(recording.started_at(), TimePosition::ZERO);
        assert_eq!(recording.punch_in(), TimePosition::from_samples(640));
        assert_eq!(recording.punch_out(), Some(TimePosition::from_samples(960)));
        let mut seen_recording = false;
        for i in 0..20 {
            feed(&mut mic, 1, 0.25);
            session::steps::render_blocks(&mut session, 1).unwrap();
            let state = transport::state(&session);
            let position = transport::playhead(&session).samples();
            // The state switches as the block that reaches a point ends, so
            // the block starting at 640 already records and 960 does not.
            let expected = if (640..960).contains(&position) {
                TransportState::Recording
            } else {
                TransportState::Playing
            };
            assert_eq!(state, expected, "block {i} at {position}");
            seen_recording |= state == TransportState::Recording;
        }
        assert!(seen_recording);
        let clips = stop(&mut session).unwrap();
        let clip = session.memory.clip(clips[0]).unwrap();
        assert_eq!(clip.start(), TimePosition::from_samples(640));
        assert_eq!(clip.length(), TimeDuration::from_samples(320));
        let audio = session.audio.get("audio/vox 1.wav".as_ref()).unwrap();
        assert!(audio.channel(0).iter().all(|&s| s == 0.25));

        // A punch-in behind the playhead is ignored: recording starts now.
        transport::seek(&mut session, TimePosition::from_samples(2_000)).unwrap();
        session::steps::render_blocks(&mut session, 1).unwrap();
        punch_out(&mut session, None).unwrap();
        let (_mic, blocks) = capture_queue(1, 64, 4).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        assert_eq!(
            session.recording.as_ref().unwrap().punch_in(),
            TimePosition::from_samples(2_000)
        );
        session::steps::render_blocks(&mut session, 1).unwrap();
        assert_eq!(transport::state(&session), TransportState::Recording);
        stop(&mut session).unwrap();
        // The punch points are edits: undo clears the last one set.
        assert!(session::undo(&mut session).unwrap());
        assert_eq!(
            session.memory.punch_out(),
            Some(TimePosition::from_samples(960))
        );
    }

    #[test]
    fn a_count_in_rolls_the_click_before_the_recording_begins() {
        use crate::primitives::transport::TransportState;
        let mut session = session::create(session::scratch("count-in"), R, 64).unwrap();
        // 1200 bpm in 4/4: a beat is 2 400 samples, a bar 9 600.
        session::set_tempo(&mut session, 1200.0).unwrap();
        crate::metronome::count_in(&mut session, 1).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        transport::seek(&mut session, TimePosition::from_samples(9_600)).unwrap();
        session::steps::render_blocks(&mut session, 1).unwrap();

        let (mut mic, blocks) = capture_queue(1, 64, 64).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        let recording = session.recording.as_ref().unwrap();
        assert_eq!(recording.started_at(), TimePosition::ZERO, "one bar early");
        assert_eq!(recording.punch_in(), TimePosition::from_samples(9_600));
        assert_eq!(
            session.mixer.click.count_in_until(),
            Some(TimePosition::from_samples(9_600))
        );
        // The click sounds through the count-in although the metronome is off ...
        feed(&mut mic, 1, 0.25);
        let first = session::steps::render_blocks(&mut session, 1).unwrap();
        assert!(first.peak() > 0.3, "the downbeat tick: {}", first.peak());
        assert_eq!(transport::state(&session), TransportState::Playing);
        for _ in 1..160 {
            feed(&mut mic, 1, 0.25);
            session::steps::render_blocks(&mut session, 1).unwrap();
        }
        assert_eq!(transport::state(&session), TransportState::Recording);
        // ... and stops at the punch-in: well past it, nothing is heard.
        feed(&mut mic, 1, 0.25);
        assert!(session::steps::render_blocks(&mut session, 1)
            .unwrap()
            .is_silent());

        let clips = stop(&mut session).unwrap();
        assert!(session.mixer.click.count_in_until().is_none());
        let clip = session.memory.clip(clips[0]).unwrap();
        assert_eq!(clip.start(), TimePosition::from_samples(9_600));
        assert_eq!(clip.length(), TimeDuration::from_samples(161 * 64 - 9_600));
    }

    #[test]
    fn a_capture_is_lined_up_with_the_roll_start_and_placed_by_offset() {
        use steps::{capture_lead, placement};
        let at = TimePosition::from_samples(1_000);
        let roll = RollStart {
            rolls: 3,
            position: at,
            instant_us: 1_010_000,
        };
        // No stamp, no roll start, or a roll start from an earlier
        // recording: nothing to line up.
        assert_eq!(capture_lead(None, Some(roll), 2, at, R), 0);
        assert_eq!(capture_lead(Some(1_000_000), None, 2, at, R), 0);
        assert_eq!(capture_lead(Some(1_000_000), Some(roll), 3, at, R), 0);
        // Capture began 10 ms before the roll: 480 frames too many.
        assert_eq!(capture_lead(Some(1_000_000), Some(roll), 2, at, R), 480);
        // Capture began 10 ms after the roll: 480 frames missing.
        assert_eq!(capture_lead(Some(1_020_000), Some(roll), 2, at, R), -480);
        // The take begins 100 frames after where the transport rolled from.
        let later = TimePosition::from_samples(1_100);
        assert_eq!(capture_lead(Some(1_010_000), Some(roll), 2, later, R), 100);

        // Where the clip goes for a file whose frame 0 is at `origin`.
        let p = |origin: i64, frames: u64, punch_in: i64, out: Option<i64>| {
            placement(
                origin,
                frames,
                TimePosition::from_samples(punch_in),
                out.map(TimePosition::from_samples),
            )
            .map(|(s, o, l)| (s.samples(), o.samples(), l.samples()))
        };
        assert_eq!(p(0, 100, 0, None), Some((0, 0, 100)), "as captured");
        assert_eq!(
            p(0, 100, 40, None),
            Some((40, 40, 60)),
            "a roll towards the punch-in"
        );
        assert_eq!(
            p(0, 100, 40, Some(70)),
            Some((40, 40, 30)),
            "and out before the end"
        );
        assert_eq!(
            p(1_000, 100, 1_000, None),
            Some((1_000, 0, 100)),
            "elsewhere on the timeline"
        );
        assert_eq!(
            p(1_010, 100, 1_000, None),
            Some((1_010, 0, 100)),
            "capture began after the roll"
        );
        assert_eq!(
            p(990, 100, 1_000, None),
            Some((1_000, 10, 90)),
            "capture began before it"
        );
        assert_eq!(p(0, 100, 100, None), None, "nothing inside");
        assert_eq!(p(0, 0, 0, None), None, "nothing captured");
        assert_eq!(p(0, 100, 40, Some(40)), None, "zero length");
    }

    #[test]
    fn a_midi_take_becomes_a_clip_on_the_armed_midi_track() {
        let mut session = session::create(session::scratch("midi-rec"), R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        let bus = track::add(&mut session, "bus", TrackKind::Bus).unwrap();
        assert!(track::arm(&mut session, bus, true).is_err());
        track::arm(&mut session, keys, true).unwrap();
        transport::start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap(); // the playhead at 640
        let (_mic, blocks) = capture_queue(1, 64, 4).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        assert_eq!(
            session.recording.as_ref().unwrap().channels(),
            0,
            "no audio track armed, no audio captured"
        );
        crate::midi::play_note(&mut session, 0, 60, 100).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap();
        crate::midi::release_note(&mut session, 0, 60).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap();
        crate::midi::play_note(&mut session, 0, 64, 90).unwrap(); // left down
        session::steps::render_blocks(&mut session, 5).unwrap();
        assert_eq!(session.recording.as_ref().unwrap().midi_messages(), 3);

        let clips = stop(&mut session).unwrap();
        assert_eq!(clips.len(), 1);
        assert_eq!(session.memory.clip_track(clips[0]).unwrap(), keys);
        let clip = session.memory.clip(clips[0]).unwrap();
        assert_eq!(clip.name, "keys");
        assert_eq!(clip.start(), TimePosition::from_samples(640));
        assert_eq!(clip.length(), TimeDuration::from_samples(1_600));
        let ClipSource::Midi { notes } = &clip.source else {
            panic!("a MIDI clip");
        };
        assert_eq!(notes.len(), 2);
        let beat = 1.0 / 24_000.0; // one sample, in beats, at 120 bpm and 48 kHz
        assert!(notes[0].beat.abs() < 1e-9);
        assert!((notes[0].length - 640.0 * beat).abs() < 1e-9);
        assert_eq!((notes[0].note, notes[0].velocity), (60, 100));
        assert!((notes[1].beat - 1_280.0 * beat).abs() < 1e-9);
        assert!(
            (notes[1].length - 320.0 * beat).abs() < 1e-9,
            "closed where the recording stopped"
        );
        assert_eq!(notes[1].note, 64);
        assert_eq!(session.history.undo_label(), Some("Record"));
        assert!(session::undo(&mut session).unwrap());
        assert!(session.memory.clip(clips[0]).is_err());
    }

    #[test]
    fn each_midi_track_keeps_the_channel_it_listens_to() {
        let mut session = session::create(session::scratch("midi-ch"), R, 64).unwrap();
        let left = track::add(&mut session, "left", TrackKind::Midi).unwrap();
        let right = track::add(&mut session, "right", TrackKind::Midi).unwrap();
        let all = track::add(&mut session, "all", TrackKind::Midi).unwrap();
        track::midi_channel(&mut session, left, Some(0)).unwrap();
        track::midi_channel(&mut session, right, Some(1)).unwrap();
        for t in [left, right, all] {
            track::arm(&mut session, t, true).unwrap();
        }
        transport::start(&mut session).unwrap();
        let (_mic, blocks) = capture_queue(1, 64, 4).unwrap();
        start(
            &mut session,
            Source::Queue {
                blocks,
                channels: 1,
            },
        )
        .unwrap();
        crate::midi::play_note(&mut session, 0, 60, 100).unwrap();
        crate::midi::play_note(&mut session, 1, 64, 100).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap();
        crate::midi::release_note(&mut session, 0, 60).unwrap();
        crate::midi::release_note(&mut session, 1, 64).unwrap();
        session::steps::render_blocks(&mut session, 5).unwrap();
        let clips = stop(&mut session).unwrap();
        assert_eq!(clips.len(), 3);
        let pitches = |id: ClipId| match &session.memory.clip(id).unwrap().source {
            ClipSource::Midi { notes } => notes.iter().map(|n| n.note).collect::<Vec<_>>(),
            _ => unreachable!(),
        };
        assert_eq!(pitches(clips[0]), [60], "left: channel 0 only");
        assert_eq!(pitches(clips[1]), [64], "right: channel 1 only");
        assert_eq!(pitches(clips[2]), [60, 64], "all: both");
    }
}
