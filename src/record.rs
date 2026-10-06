//! Recording: arm tracks, capture input, get a clip on each of them.
//!
//! [`start`] records from a [`Source`] onto every armed track, from the
//! playhead or from the punch-in point ahead of it ([`punch_in`]), until
//! [`stop`] or the punch-out point ([`punch_out`]); the transport rolls
//! from where it is and drops into and out of record at those points.
//! [`stop`] writes each track's audio as a WAV file into the session folder
//! (`audio/vox 1.wav`), keeps it in memory, and places a clip on each of
//! those tracks, as one undo step. The audio arrives through a capture
//! queue that holds only a fraction of a second, so something has to empty
//! it as it fills: a hardware source gets a thread that does so; a queue
//! you fill yourself waits for [`collect`], which
//! [`session::steps::render_blocks`] calls after every block it renders.
//!
//! Made of `superb::primitives::io::capture` (the queue), the session's
//! audio input (`devices::input`, feature `devices`), the audio held on the
//! `Session`, and [`clip::add`](crate::clip::add).

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::io::EngineCommand;
use crate::primitives::session::commands::{AddClip, PunchPoint, SetPunch};
use crate::primitives::session::{Clip, ClipSource, Input, Memory, TrackKind};
use crate::primitives::undo::Command;
use crate::primitives::{
    ClipId, Result, SampleRate, SuperbError, TimeDuration, TimePosition, TrackId,
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
/// [`start`] and [`stop`].
pub struct Recording {
    capture: Capture,
    started_at: TimePosition,
    punch_in: TimePosition,
    punch_out: Option<TimePosition>,
    tracks: Vec<TrackId>,
    channels: usize,
}

impl Recording {
    /// Frames gathered so far.
    pub fn frames(&self) -> usize {
        self.capture.frames()
    }

    /// Blocks the input side could not queue because the queue was full.
    /// Anything above zero means a gap in the recording.
    pub fn dropped(&self) -> u64 {
        self.capture.dropped()
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

    /// Channels being captured.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Moves waiting blocks into a queue recording; see [`collect`].
    pub(crate) fn collect(&mut self) -> usize {
        steps::collect(&mut self.capture)
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
            .field("frames", &self.frames())
            .field("dropped", &self.dropped())
            .field("threaded", &matches!(self.capture, Capture::Threaded(_)))
            .finish()
    }
}

/// How a recording's blocks get from the capture queue into memory.
pub(crate) enum Capture {
    /// Blocks wait on the queue until [`collect`] moves them into `planes`.
    Manual {
        /// The queue's consumer end.
        blocks: CaptureConsumer,
        /// Audio gathered so far, one plane per channel.
        planes: Vec<Vec<f32>>,
    },
    /// A thread empties the queue as it fills; nothing to call. Only a
    /// device source makes one, so without `devices` it exists for tests.
    #[cfg_attr(not(feature = "devices"), allow(dead_code))]
    Threaded(Collector),
}

impl Capture {
    /// Frames gathered so far.
    pub(crate) fn frames(&self) -> usize {
        match self {
            Capture::Manual { planes, .. } => planes.first().map_or(0, Vec::len),
            Capture::Threaded(collector) => collector.frames(),
        }
    }

    /// Blocks the input side had to drop.
    pub(crate) fn dropped(&self) -> u64 {
        match self {
            Capture::Manual { blocks, .. } => blocks.dropped(),
            Capture::Threaded(collector) => collector.dropped(),
        }
    }
}

/// A thread that moves captured blocks into memory as they arrive, so a
/// recording can be as long as memory allows no matter how small the queue is.
/// Made by `steps::spawn_collector`; [`finish`](Collector::finish) hands
/// the planes back.
pub(crate) struct Collector {
    frames: Arc<AtomicUsize>,
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<(Vec<Vec<f32>>, CaptureConsumer)>>,
}

impl Collector {
    /// Frames gathered so far.
    pub(crate) fn frames(&self) -> usize {
        self.frames.load(Ordering::Acquire)
    }

    /// Blocks the input side had to drop (the thread was late, or the
    /// queue is too small for the block rate).
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops the thread after it has drained what is left on the queue and
    /// returns the planes with the queue's consumer end. Stop the input
    /// side first so nothing new arrives.
    pub(crate) fn finish(mut self) -> Result<(Vec<Vec<f32>>, CaptureConsumer)> {
        self.stop.store(true, Ordering::Release);
        let handle = self.handle.take().expect("finish is called once");
        handle
            .join()
            .map_err(|_| SuperbError::Message("the recording collector thread panicked".into()))
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        // Not finished: let the thread exit on its own; its planes are lost.
        self.stop.store(true, Ordering::Release);
    }
}

impl core::fmt::Debug for Collector {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Collector")
            .field("frames", &self.frames())
            .field("dropped", &self.dropped())
            .finish()
    }
}

/// Starts recording from `source` onto every armed track. Recording begins
/// at the playhead, or at the punch-in point when one is set ahead of it
/// ([`punch_in`]): the transport rolls from where it is and drops into
/// record there. With a punch-out point ([`punch_out`]) it drops out of
/// record there and keeps playing; [`stop`] finishes either way.
///
/// Made of: [`track::arm`](crate::track::arm)ed tracks → for a device,
/// `devices::input` if needed, its recording queue and a collector thread →
/// [`session::steps::rebuild`] when needed → `EngineHandle::send(SetPunch)`
/// → `EngineHandle::send(Record)`, or `Play` when the punch-in is ahead →
/// [`transport::playhead`] for where it all started.
pub fn start(session: &mut Session, source: Source) -> Result<()> {
    if session.recording.is_some() {
        return Err(SuperbError::Message(
            "a recording is already running".into(),
        ));
    }
    if crate::track::armed(session).is_empty() {
        return Err(SuperbError::Message(
            "arm an audio track before recording".into(),
        ));
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
                .ok_or_else(|| SuperbError::Message("the audio input did not open".into()))?;
            let channels = input.channels();
            let blocks = input.begin_recording()?;
            let collector = steps::spawn_collector(blocks, channels)?;
            begin(session, Capture::Threaded(collector), channels)
        }
        Source::Queue { blocks, channels } => begin(
            session,
            Capture::Manual {
                blocks,
                planes: vec![Vec::new(); channels],
            },
            channels,
        ),
    }
}

/// What every source has in common once the capture exists.
fn begin(session: &mut Session, capture: Capture, channels: usize) -> Result<()> {
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
    let started_at = roll_from(session, punch_in, playhead);
    if started_at != playhead {
        session.engine.send(EngineCommand::Seek(started_at))?;
    }
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
        started_at,
        punch_in,
        punch_out,
        tracks,
        channels,
    });
    Ok(())
}

/// Where the transport starts rolling for a recording that begins at
/// `punch_in`: the playhead, when it is at or before the punch-in.
fn roll_from(_session: &Session, punch_in: TimePosition, playhead: TimePosition) -> TimePosition {
    playhead.min(punch_in)
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

/// Moves waiting blocks into the recording when the source is a queue
/// ([`Source::Queue`]); a device recording has its thread and needs
/// nothing. Returns the frames gathered in total, or zero when nothing is
/// recording. [`session::steps::render_blocks`] calls this after every
/// block; with a device driving the engine and a queue source, call it
/// from a timer.
pub fn collect(session: &mut Session) -> usize {
    match session.recording.as_mut() {
        Some(recording) => {
            recording.collect();
            recording.frames()
        }
        None => 0,
    }
}

/// Stops the recording and the transport and places one clip per armed
/// track starting at the punch-in point. Audio captured before the
/// punch-in (while rolling towards it) and after the punch-out is left
/// out. Each clip holds the channels its
/// track's [`Input`] selects (tracks without one share the whole capture)
/// and refers to `audio/<track> <n>.wav` (`audio/vox 1.wav`), so it is
/// named `vox 1`; the file is written into [`Session::folder`] before the
/// clip is placed. Returns the new clips (none if nothing was captured).
/// The whole placement is one undo step called "Record": undoing it
/// removes the clips and deletes the files it wrote, redo writes them
/// again; the audio stays in memory either way.
///
/// Made of: `EngineHandle::send(Stop)` → drain the queue or
/// `Collector::finish` (and hand the audio input its queue back) → the
/// planes become one buffer → per
/// track, a file name nothing uses yet and the channels its `Input`
/// selects → `wav::write` into the session folder → `AudioCache::insert_shared`
/// → [`clip::add`](crate::clip::add), inside one undo group; rebuilds the
/// mixer so the clips play back on their strips.
pub fn stop(session: &mut Session) -> Result<Vec<ClipId>> {
    let recording = session
        .recording
        .take()
        .ok_or_else(|| SuperbError::Message("nothing is being recorded".into()))?;
    session.engine.send(EngineCommand::SetPunch {
        punch_in: None,
        punch_out: None,
    })?;
    session.engine.send(EngineCommand::Stop)?;
    let Recording {
        capture,
        started_at,
        punch_in,
        punch_out,
        tracks,
        ..
    } = recording;
    let planes = match capture {
        Capture::Manual {
            mut blocks,
            mut planes,
        } => {
            steps::append_blocks(&mut blocks, &mut planes);
            planes
        }
        Capture::Threaded(collector) => {
            let (planes, blocks) = collector.finish()?;
            #[cfg(feature = "devices")]
            if let Some(input) = session.input.as_mut() {
                input.end_recording(blocks);
            }
            #[cfg(not(feature = "devices"))]
            drop(blocks);
            planes
        }
    };
    let planes = steps::trim_planes(planes, started_at, punch_in, punch_out);
    let audio = steps::finish_recording(planes);
    if audio.frames() == 0 {
        return Ok(Vec::new());
    }
    let whole = Arc::new(audio);
    let rate = session.memory.sample_rate();
    session.history.begin_group("Record");
    let mut clips = Vec::with_capacity(tracks.len());
    for track in &tracks {
        let (key, input) = match steps::audio_key(session, *track) {
            Ok(key) => (key, session.memory.track(*track)?.state.input),
            Err(e) => {
                session.history.cancel_group(&mut session.memory)?;
                return Err(e);
            }
        };
        let audio = match input {
            None => Arc::clone(&whole),
            Some(input) => Arc::new(steps::select_input(&whole, input)),
        };
        session.audio.insert_shared(key.clone(), Arc::clone(&audio));
        let id = session.memory.allocate_clip_id();
        let name = key
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let length = TimeDuration::from_samples(audio.frames() as i64);
        let placed = Clip::with_id(
            id,
            name,
            punch_in,
            length,
            ClipSource::Audio {
                path: key.clone(),
                offset: TimeDuration::ZERO,
            },
        )
        .and_then(|clip| {
            let folder = session.folder.clone();
            session::structural(
                session,
                RecordClip {
                    add: AddClip::new(*track, clip),
                    folder,
                    key,
                    audio,
                    rate,
                },
            )
        });
        if let Err(e) = placed {
            session.history.cancel_group(&mut session.memory)?;
            return Err(e);
        }
        clips.push(id);
    }
    session.history.end_group();
    session.needs_rebuild = true;
    Ok(clips)
}

/// Places a recorded clip and owns its file: applying writes the WAV, so a
/// redo brings the file back; reverting deletes it, so an undone recording
/// leaves nothing on disk. The audio stays in `Session::audio` either way.
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
        crate::session::folder::write_audio(&self.folder, &self.key, &self.audio, self.rate)?;
        self.add.apply(memory)
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        self.add.revert(memory)?;
        match std::fs::remove_file(self.folder.join(&self.key)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(SuperbError::Message(format!(
                "cannot delete {}: {e}",
                self.key.display()
            ))),
        }
    }
}

/// The parts `start`, `collect` and `stop` are made of.
pub(crate) mod steps {
    use super::*;

    /// Starts a thread that empties `blocks` into memory as they arrive.
    /// The thread sleeps briefly when the queue is empty and exits after
    /// [`Collector::finish`].
    #[cfg_attr(not(feature = "devices"), allow(dead_code))]
    pub(crate) fn spawn_collector(
        mut blocks: CaptureConsumer,
        channels: usize,
    ) -> Result<Collector> {
        let frames = Arc::new(AtomicUsize::new(0));
        let dropped = blocks.dropped_handle();
        let stop = Arc::new(AtomicBool::new(false));
        let (frames_t, stop_t) = (Arc::clone(&frames), Arc::clone(&stop));
        let handle = std::thread::Builder::new()
            .name("superb recording collector".into())
            .spawn(move || {
                let mut planes = vec![Vec::new(); channels];
                loop {
                    let appended = append_blocks(&mut blocks, &mut planes);
                    frames_t.fetch_add(appended, Ordering::Release);
                    if stop_t.load(Ordering::Acquire) {
                        // The input has stopped feeding us; take what is left.
                        let rest = append_blocks(&mut blocks, &mut planes);
                        frames_t.fetch_add(rest, Ordering::Release);
                        break (planes, blocks);
                    }
                    if appended == 0 {
                        std::thread::sleep(core::time::Duration::from_millis(2));
                    }
                }
            })
            .map_err(|e| {
                SuperbError::Message(format!("cannot start the recording collector: {e}"))
            })?;
        Ok(Collector {
            frames,
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

    /// The channel(s) an [`Input`] selects from a captured buffer: one for
    /// `Mono`, two for `Stereo`. A channel the capture did not have is
    /// silence.
    pub(crate) fn select_input(captured: &PlanarBuffer, input: Input) -> PlanarBuffer {
        let frames = captured.frames();
        let pick = |c: usize| -> Vec<f32> {
            if c < captured.channels() {
                captured.channel(c).to_vec()
            } else {
                vec![0.0; frames]
            }
        };
        let planes: Vec<Vec<f32>> = match input {
            Input::Mono(c) => vec![pick(c)],
            Input::Stereo(l, r) => vec![pick(l), pick(r)],
        };
        finish_recording(planes)
    }

    /// Moves every waiting block into the recording when the capture is manual;
    /// a threaded capture needs nothing. Returns the frames appended.
    pub(crate) fn collect(capture: &mut Capture) -> usize {
        match capture {
            Capture::Manual { blocks, planes } => append_blocks(blocks, planes),
            Capture::Threaded(_) => 0,
        }
    }

    /// Appends every waiting block to `planes` and hands the blocks back to
    /// the input side. Blocks with fewer channels than the recording are padded
    /// with silence. Returns the frames appended. Allocates; never call it
    /// on the audio thread.
    pub(crate) fn append_blocks(blocks: &mut CaptureConsumer, planes: &mut [Vec<f32>]) -> usize {
        let mut appended = 0;
        while let Some(block) = blocks.pop() {
            let frames = block.frames();
            for (ch, plane) in planes.iter_mut().enumerate() {
                if ch < block.channels() {
                    plane.extend_from_slice(block.buffer().channel(ch));
                } else {
                    plane.extend(std::iter::repeat_n(0.0, frames));
                }
            }
            appended += frames;
            blocks.recycle(block);
        }
        appended
    }

    /// Drops what was captured before the punch-in (the roll towards it)
    /// and after the punch-out.
    pub(crate) fn trim_planes(
        planes: Vec<Vec<f32>>,
        started_at: TimePosition,
        punch_in: TimePosition,
        punch_out: Option<TimePosition>,
    ) -> Vec<Vec<f32>> {
        let skip = (punch_in.samples() - started_at.samples()).max(0) as usize;
        let keep = punch_out.map(|out| (out.samples() - punch_in.samples()).max(0) as usize);
        planes
            .into_iter()
            .map(|plane| {
                let mut plane = plane.get(skip..).map(<[f32]>::to_vec).unwrap_or_default();
                if let Some(keep) = keep {
                    plane.truncate(keep);
                }
                plane
            })
            .collect()
    }

    /// Turns collected planes into one buffer. Empty planes give a buffer
    /// with no frames.
    pub(crate) fn finish_recording(planes: Vec<Vec<f32>>) -> PlanarBuffer {
        let channels = planes.len();
        let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        PlanarBuffer::from_planes(&refs).unwrap_or_else(|_| PlanarBuffer::new(channels, 0))
    }

    /// A file for a new recording on `track` that nothing uses yet, as a
    /// path relative to the session folder: `audio/vox 1.wav`,
    /// `audio/vox 2.wav`, ... The clip takes its stem as its name. Taken
    /// names are those in memory and the files already in the folder.
    pub(crate) fn audio_key(session: &Session, track: TrackId) -> Result<PathBuf> {
        let base = session.memory.track(track)?.name.clone();
        let mut n = 1;
        loop {
            let key = PathBuf::from(format!(
                "{}/{base} {n}.wav",
                crate::session::folder::AUDIO_DIR
            ));
            let in_memory = session.audio.get(&key).is_some();
            let on_disk = session.folder.join(&key).exists();
            if !in_memory && !on_disk {
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
        assert!(
            Arc::ptr_eq(
                &session.audio.get("audio/vox 1.wav".as_ref()).unwrap(),
                &session.audio.get("audio/gtr 1.wav".as_ref()).unwrap()
            ),
            "sharing one buffer"
        );
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
        assert_eq!(session.mixer.strips.len(), 3, "one handle per track");

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
        // A channel the capture did not have is silence.
        let silent = steps::select_input(&audio("room 1"), Input::Mono(5));
        assert!(silent.is_silent() && silent.frames() == 192);
        // No input set: the whole capture, shared.
        track::input(&mut session, vox, None).unwrap();
        assert_eq!(
            steps::input_width(&session),
            2,
            "gtr and room still need two"
        );
    }

    #[test]
    fn errors_are_explicit() {
        let mut session = session::create(session::scratch("rec"), R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        assert!(
            track::arm(&mut session, keys, true).is_err(),
            "MIDI tracks monitor, they do not record audio"
        );
        assert!(
            matches!(stop(&mut session), Err(SuperbError::Message(_))),
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
        let collector = steps::spawn_collector(blocks, 1).unwrap();
        begin(&mut session, Capture::Threaded(collector), 1).unwrap();
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

        // An unfinished collector just lets its thread go.
        let (_mic, blocks) = capture_queue(1, 64, 2).unwrap();
        drop(steps::spawn_collector(blocks, 1).unwrap());
    }

    #[test]
    fn collect_pads_missing_channels_with_silence() {
        let (mut mic, blocks) = capture_queue(1, 4, 4).unwrap();
        let mut block = PlanarBuffer::new(1, 4);
        block.fill(1.0);
        mic.push(&block, 0);
        mic.push(&block, 1);
        let mut capture = Capture::Manual {
            blocks,
            planes: vec![Vec::new(); 2],
        };
        assert_eq!(steps::collect(&mut capture), 8);
        assert_eq!(capture.frames(), 8);
        assert_eq!(steps::collect(&mut capture), 0);
        let Capture::Manual { blocks, planes } = capture else {
            unreachable!()
        };
        assert!(blocks.is_empty());
        let audio = steps::finish_recording(planes);
        assert_eq!(audio.channels(), 2);
        assert_eq!(audio.channel(0), &[1.0; 8]);
        assert_eq!(audio.channel(1), &[0.0; 8]);
        assert_eq!(steps::finish_recording(vec![Vec::new(); 2]).frames(), 0);
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
}
