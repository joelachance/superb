//! Recording: arm tracks, capture input, get a clip on each of them.
//!
//! A recording flows through a [`CaptureConsumer`]: the input side fills it
//! with blocks and something empties it into the growing audio, because the
//! queue holds only a fraction of a second. With a device (`start`) a
//! collector thread does the emptying. With a source of your own
//! ([`start_from`]) the blocks wait until [`collect`] runs, which
//! [`session::steps::render_blocks`] does after every block. [`stop`] puts the audio in the
//! audio cache and places a clip on every armed track, as one undo step.
//!
//! Made of `superb::primitives::io::capture` (the queue),
//! `superb::primitives::io::device` (hardware input, feature `devices`),
//! the audio cache, and the document's clip commands.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::io::live::EngineCommand;
use crate::primitives::{ClipId, Result, SuperbError, TimeDuration, TimePosition, TrackId};
use crate::session;
use crate::session::Session;
use crate::transport;

/// A recording in progress.
pub struct Recording {
    /// Where the captured blocks are being gathered.
    pub capture: Capture,
    /// Where on the timeline the recording started.
    pub punch_in: TimePosition,
    /// Tracks that were armed when recording started; each gets a clip.
    pub tracks: Vec<TrackId>,
    /// Channels being recorded.
    pub channels: usize,
    /// The input stream keeping the capture alive, when recording from hardware.
    #[cfg(feature = "devices")]
    pub stream: Option<crate::primitives::io::device::InputStream>,
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
}

impl core::fmt::Debug for Recording {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Recording")
            .field("punch_in", &self.punch_in)
            .field("tracks", &self.tracks)
            .field("channels", &self.channels)
            .field("frames", &self.frames())
            .field("dropped", &self.dropped())
            .field("threaded", &matches!(self.capture, Capture::Threaded(_)))
            .finish()
    }
}

/// How a recording's blocks get from the capture queue into memory.
pub enum Capture {
    /// Blocks wait on the queue until [`collect`] moves them into `planes`.
    /// [`session::steps::render_blocks`] collects after every block it renders; with a
    /// device driving the engine you would call [`collect`] from a timer.
    Manual {
        /// The queue's consumer end.
        blocks: CaptureConsumer,
        /// Audio gathered so far, one plane per channel.
        planes: Vec<Vec<f32>>,
    },
    /// A thread empties the queue as it fills; nothing to call.
    Threaded(Collector),
}

impl Capture {
    /// Frames gathered so far.
    pub fn frames(&self) -> usize {
        match self {
            Capture::Manual { planes, .. } => planes.first().map_or(0, Vec::len),
            Capture::Threaded(collector) => collector.frames(),
        }
    }

    /// Blocks the input side had to drop.
    pub fn dropped(&self) -> u64 {
        match self {
            Capture::Manual { blocks, .. } => blocks.dropped(),
            Capture::Threaded(collector) => collector.dropped(),
        }
    }
}

/// A thread that moves captured blocks into memory as they arrive, so a
/// recording can be as long as memory allows no matter how small the queue is.
/// Made by [`steps::spawn_collector`]; [`finish`](Collector::finish) hands
/// the planes back.
pub struct Collector {
    frames: Arc<AtomicUsize>,
    dropped: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Vec<Vec<f32>>>>,
}

impl Collector {
    /// Frames gathered so far.
    pub fn frames(&self) -> usize {
        self.frames.load(Ordering::Acquire)
    }

    /// Blocks the input side had to drop (the thread was late, or the
    /// queue is too small for the block rate).
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Stops the thread after it has drained what is left on the queue and
    /// returns the planes. Drop the input stream first so nothing new
    /// arrives.
    pub fn finish(mut self) -> Result<Vec<Vec<f32>>> {
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

/// `true` while a recording is running.
pub fn is_recording(session: &Session) -> bool {
    session.recording.is_some()
}

/// Starts recording `channels` channels from a hardware input (`None` for
/// the default device, or a name from `devices::inputs`) onto every armed
/// track, from the current playhead position. The transport starts
/// rolling, and a collector thread gathers the audio until [`stop`].
///
/// Made of: an `InputConfig` matching the session → [`steps::open_input`]
/// with 64 queued blocks → [`steps::spawn_collector`] → the same beginning
/// as [`start_from`].
#[cfg(feature = "devices")]
pub fn start(session: &mut Session, device: Option<&str>, channels: usize) -> Result<()> {
    let input = crate::devices::input_config(session, device, channels);
    let (stream, blocks) = steps::open_input(&input, 64)?;
    let channels = stream.channels();
    let collector = steps::spawn_collector(blocks, channels)?;
    begin(session, Capture::Threaded(collector), channels)?;
    if let Some(recording) = session.recording.as_mut() {
        recording.stream = Some(stream);
    }
    Ok(())
}

/// Starts recording from a capture queue you fill yourself (a file, a
/// generator, a test), onto every armed track from the current playhead.
/// Blocks wait on the queue until [`collect`] runs; [`session::steps::render_blocks`] does
/// that after every block, so an offline program never calls it.
///
/// Made of: [`track::armed`](crate::track::armed) → [`session::steps::rebuild`] when needed →
/// `EngineHandle::send(Record)` → [`transport::playhead`] for the punch-in.
pub fn start_from(session: &mut Session, blocks: CaptureConsumer, channels: usize) -> Result<()> {
    begin(
        session,
        Capture::Manual {
            blocks,
            planes: vec![Vec::new(); channels],
        },
        channels,
    )
}

/// What every start has in common once the capture exists.
fn begin(session: &mut Session, capture: Capture, channels: usize) -> Result<()> {
    if session.recording.is_some() {
        return Err(SuperbError::Message(
            "a recording is already running".into(),
        ));
    }
    let tracks = crate::track::armed(session);
    if tracks.is_empty() {
        return Err(SuperbError::Message(
            "arm an audio track before recording".into(),
        ));
    }
    if session.needs_rebuild {
        session::steps::rebuild(session)?;
    }
    session.engine.send(EngineCommand::Record)?;
    session.recording = Some(Recording {
        capture,
        punch_in: transport::playhead(session),
        tracks,
        channels,
        #[cfg(feature = "devices")]
        stream: None,
    });
    Ok(())
}

/// Moves waiting blocks into the recording when the capture is manual (see
/// [`start_from`]); with a collector thread there is nothing to do.
/// Returns the frames gathered in total, or zero when nothing is recording.
///
/// Made of: [`steps::collect`].
pub fn collect(session: &mut Session) -> usize {
    match session.recording.as_mut() {
        Some(recording) => {
            steps::collect(&mut recording.capture);
            recording.frames()
        }
        None => 0,
    }
}

/// Stops the recording and the transport, stores the audio, and places one
/// clip per armed track starting at the punch-in point, each named after
/// its track (`vox 1`, `vox 2`, ...). The clips share one buffer. Returns
/// the new clips (none if nothing was captured). The whole placement is
/// one undo step called "Record".
///
/// Made of: `EngineHandle::send(Stop)` → drop the input stream →
/// [`steps::collect`] or `Collector::finish` → the planes become one
/// buffer → per track, a name no cache entry uses (`vox 1`) →
/// `AudioCache::insert_shared` → [`session::add_audio_clip`], inside one
/// undo group; rebuilds the mixer so the clips play back on their strips.
pub fn stop(session: &mut Session) -> Result<Vec<ClipId>> {
    let recording = session
        .recording
        .take()
        .ok_or_else(|| SuperbError::Message("nothing is being recorded".into()))?;
    session.engine.send(EngineCommand::Stop)?;
    let Recording {
        capture,
        punch_in,
        tracks,
        #[cfg(feature = "devices")]
        stream,
        ..
    } = recording;
    #[cfg(feature = "devices")]
    drop(stream); // nothing new arrives from here
    let planes = match capture {
        Capture::Manual {
            mut blocks,
            mut planes,
        } => {
            steps::append_blocks(&mut blocks, &mut planes);
            planes
        }
        Capture::Threaded(collector) => collector.finish()?,
    };
    let audio = steps::finish_recording(planes);
    if audio.frames() == 0 {
        return Ok(Vec::new());
    }
    let length = TimeDuration::from_samples(audio.frames() as i64);
    let audio = Arc::new(audio);
    session.history.begin_group("Record");
    let mut clips = Vec::with_capacity(tracks.len());
    for track in &tracks {
        let name = match steps::clip_name(session, *track) {
            Ok(name) => name,
            Err(e) => {
                session.history.cancel_group(&mut session.document)?;
                return Err(e);
            }
        };
        session.cache.insert_shared(&name, Arc::clone(&audio));
        match steps::place_clip(session, *track, &name, punch_in, length) {
            Ok(clip) => clips.push(clip),
            Err(e) => {
                session.history.cancel_group(&mut session.document)?;
                return Err(e);
            }
        }
    }
    session.history.end_group();
    session.needs_rebuild = true;
    Ok(clips)
}

/// The steps `start`, [`start_from`] and [`collect`] are made of, for
/// recording from a source of your own.
pub mod steps {
    use super::*;

    /// Opens a hardware input as a capture queue with `slots` pre-allocated
    /// blocks. Returns the stream (drop it to stop) and the consumer end.
    ///
    /// Made of: `device::run_capture`.
    #[cfg(feature = "devices")]
    pub fn open_input(
        config: &crate::primitives::io::device::InputConfig,
        slots: usize,
    ) -> Result<(crate::primitives::io::device::InputStream, CaptureConsumer)> {
        crate::primitives::io::device::run_capture(config, slots)
    }

    /// Starts a thread that empties `blocks` into memory as they arrive.
    /// The thread sleeps briefly when the queue is empty and exits after
    /// [`Collector::finish`].
    pub fn spawn_collector(mut blocks: CaptureConsumer, channels: usize) -> Result<Collector> {
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
                        // The input has been closed; take what is left.
                        let rest = append_blocks(&mut blocks, &mut planes);
                        frames_t.fetch_add(rest, Ordering::Release);
                        break planes;
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

    /// Moves every waiting block into the recording when the capture is manual;
    /// a threaded capture needs nothing. Returns the frames appended.
    pub fn collect(capture: &mut Capture) -> usize {
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

    /// Turns collected planes into one buffer. Empty planes give a buffer
    /// with no frames.
    pub(crate) fn finish_recording(planes: Vec<Vec<f32>>) -> PlanarBuffer {
        let channels = planes.len();
        let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        PlanarBuffer::from_planes(&refs).unwrap_or_else(|_| PlanarBuffer::new(channels, 0))
    }

    /// A name for a new clip on `track` that no cache entry uses yet:
    /// `vox 1`, `vox 2`, ... The clip and the audio it plays share it.
    pub(crate) fn clip_name(session: &Session, track: TrackId) -> Result<String> {
        let base = session.document.track(track)?.name.clone();
        let mut n = 1;
        loop {
            let name = format!("{base} {n}");
            if session.cache.get(std::path::Path::new(&name)).is_none() {
                return Ok(name);
            }
            n += 1;
        }
    }

    /// Places a clip of cached audio on `track`.
    ///
    /// Made of: [`session::add_audio_clip`].
    pub(crate) fn place_clip(
        session: &mut Session,
        track: TrackId,
        audio: &str,
        start: TimePosition,
        length: TimeDuration,
    ) -> Result<ClipId> {
        crate::session::add_audio_clip(session, track, audio, start, length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::io::capture::capture_queue;
    use crate::primitives::session::TrackKind;
    use crate::primitives::SampleRate;
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
        let mut session = session::create("rec", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let gtr = track::add(&mut session, "gtr", TrackKind::Audio).unwrap();
        let _idle = track::add(&mut session, "idle", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        track::arm(&mut session, gtr, true).unwrap();
        assert_eq!(crate::track::armed(&session), [vox, gtr]);
        assert!(!is_recording(&session));

        // Roll to 640 frames, then punch in there.
        transport::start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 10).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        start_from(&mut session, blocks, 1).unwrap();
        assert!(is_recording(&session));
        assert_eq!(
            session.recording.as_ref().unwrap().punch_in,
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
        assert!(!is_recording(&session));
        for (clip, name) in clips.iter().zip(["vox 1", "gtr 1"]) {
            let c = session.document.clip(*clip).unwrap();
            assert_eq!(c.start(), TimePosition::from_samples(640));
            assert_eq!(c.length(), TimeDuration::from_samples(320));
            assert_eq!(c.name, name);
        }
        assert_eq!(session.document.clip_track(clips[0]).unwrap(), vox);
        assert_eq!(session.document.clip_track(clips[1]).unwrap(), gtr);
        assert_eq!(session.cache.len(), 2, "one entry per clip");
        assert!(
            Arc::ptr_eq(
                &session.cache.get("vox 1".as_ref()).unwrap(),
                &session.cache.get("gtr 1".as_ref()).unwrap()
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
        assert_eq!(session.live.strips.len(), 3, "one handle per track");

        // One undo removes both clips.
        assert!(session::undo(&mut session).unwrap());
        assert!(
            session.document.clip(clips[0]).is_err() && session.document.clip(clips[1]).is_err()
        );
        assert!(session::redo(&mut session).unwrap());
        assert!(session.document.clip(clips[0]).is_ok());
    }

    #[test]
    fn clips_get_distinct_names_and_empty_recordings_leave_no_clip() {
        let mut session = session::create("rec", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        for i in 1..=2 {
            let (mut mic, blocks) = capture_queue(2, 64, 4).unwrap();
            start_from(&mut session, blocks, 2).unwrap();
            feed_stereo(&mut mic, 2);
            let clips = stop(&mut session).unwrap();
            assert_eq!(
                session.document.clip(clips[0]).unwrap().name,
                format!("vox {i}")
            );
        }
        assert_eq!(session.cache.len(), 2);
        let (_mic, blocks) = capture_queue(2, 64, 4).unwrap();
        start_from(&mut session, blocks, 2).unwrap();
        assert!(
            stop(&mut session).unwrap().is_empty(),
            "nothing captured, nothing placed"
        );
        assert_eq!(session.document.clip_count(), 2);
    }

    fn feed_stereo(producer: &mut crate::primitives::io::capture::CaptureProducer, blocks: usize) {
        let block = PlanarBuffer::new(2, 64);
        for i in 0..blocks {
            assert!(producer.push(&block, i as u64));
        }
    }

    #[test]
    fn errors_are_explicit() {
        let mut session = session::create("rec", R, 64).unwrap();
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
            start_from(&mut session, blocks, 1).is_err(),
            "no armed track"
        );
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        let (_a, blocks) = capture_queue(1, 64, 2).unwrap();
        start_from(&mut session, blocks, 1).unwrap();
        let (_b, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(
            start_from(&mut session, blocks, 1).is_err(),
            "already recording"
        );
        let _ = format!("{:?}", session.recording.as_ref().unwrap());
    }

    #[test]
    fn a_collector_thread_gathers_the_take_by_itself() {
        let mut session = session::create("rec", R, 64).unwrap();
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
        let clip = session.document.clip(clips[0]).unwrap();
        assert_eq!(
            clip.length(),
            TimeDuration::from_samples(((200 - dropped) * 64) as i64),
            "every block pushed was gathered or counted as dropped"
        );
        assert!(
            dropped < 100,
            "the collector fell far behind: {dropped} dropped"
        );
        let audio = session.cache.get("vox 1".as_ref()).unwrap();
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
        let mut session = session::create("rec", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        track::arm(&mut session, vox, true).unwrap();
        // A queue of only two blocks; the recording is ten blocks long.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        start_from(&mut session, blocks, 1).unwrap();
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
            session.document.clip(clips[0]).unwrap().length(),
            TimeDuration::from_samples(640)
        );
        assert_eq!(collect(&mut session), 0, "nothing recording");

        // Without collecting, the input side reports what it had to drop.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        start_from(&mut session, blocks, 1).unwrap();
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
}
