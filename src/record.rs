//! Recording: arm tracks, capture input, turn the take into a clip.
//!
//! A take flows through a [`CaptureConsumer`]: the input side fills it with
//! blocks and something empties it into the growing take, because the
//! queue holds only a fraction of a second. With a device (`start`) a
//! collector thread does the emptying. With a source of your own
//! ([`start_from`]) the blocks wait until [`collect`] runs, which
//! [`play::drive`] does after every block. [`stop`] puts the take in the
//! audio cache and places a clip on every armed track, as one undo step.
//!
//! Made of `superb::primitives::io::capture` (the queue),
//! `superb::primitives::io::device` (hardware input, feature `devices`),
//! the audio cache, and the document's clip commands.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use crate::play;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::io::live::EngineCommand;
use crate::primitives::session::TrackKind;
use crate::primitives::{ClipId, Result, SuperbError, TimeDuration, TimePosition, TrackId};
use crate::rig::Rig;

/// A take in progress.
pub struct Recording {
    /// Where the captured blocks are being gathered.
    pub capture: Capture,
    /// Where on the timeline the take started.
    pub punch_in: TimePosition,
    /// Tracks that were armed when recording started; each gets a clip.
    pub tracks: Vec<TrackId>,
    /// Channels in the take.
    pub channels: usize,
    /// The input stream keeping the capture alive, when recording from hardware.
    #[cfg(feature = "devices")]
    pub stream: Option<crate::primitives::io::device::InputStream>,
}

impl Recording {
    /// Frames gathered into the take so far.
    pub fn frames(&self) -> usize {
        self.capture.frames()
    }

    /// Blocks the input side could not queue because the queue was full.
    /// Anything above zero means a gap in the take.
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

/// How a take's blocks get from the capture queue into memory.
pub enum Capture {
    /// Blocks wait on the queue until [`collect`] moves them into `planes`.
    /// [`play::drive`] collects after every block it renders; with a
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
/// take can be as long as memory allows no matter how small the queue is.
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
            .map_err(|_| SuperbError::Message("the take collector thread panicked".into()))
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

/// Arms or disarms an audio track. Armed tracks receive a clip when a take
/// is stopped. A switch: saved with the document, not an undo step.
///
/// Made of: `Track::armed` through `session::switch`.
pub fn arm(rig: &mut Rig, track: TrackId, on: bool) -> Result<()> {
    if rig.document.track(track)?.kind != TrackKind::Audio {
        return Err(SuperbError::InvalidArgument {
            what: "only audio tracks are armed for recording",
        });
    }
    crate::session::switch(rig, |s| {
        s.track_mut(track)?.armed = on;
        Ok(())
    })
}

/// The audio tracks currently armed, in track order.
pub fn armed_tracks(rig: &Rig) -> Vec<TrackId> {
    rig.document
        .tracks()
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && t.armed)
        .map(|t| t.id())
        .collect()
}

/// `true` while a take is being captured.
pub fn is_recording(rig: &Rig) -> bool {
    rig.recording.is_some()
}

/// Starts recording from a hardware input onto every armed track, from the
/// current playhead position. The transport starts rolling, and a collector
/// thread gathers the take until [`stop`].
///
/// Made of: [`steps::open_input`] with 64 queued blocks →
/// [`steps::spawn_collector`] → the same beginning as [`start_from`].
#[cfg(feature = "devices")]
pub fn start(rig: &mut Rig, input: &crate::primitives::io::device::InputConfig) -> Result<()> {
    let (stream, blocks) = steps::open_input(input, 64)?;
    let channels = stream.channels();
    let collector = steps::spawn_collector(blocks, channels)?;
    begin(rig, Capture::Threaded(collector), channels)?;
    if let Some(recording) = rig.recording.as_mut() {
        recording.stream = Some(stream);
    }
    Ok(())
}

/// Starts recording from a capture queue you fill yourself (a file, a
/// generator, a test), onto every armed track from the current playhead.
/// Blocks wait on the queue until [`collect`] runs; [`play::drive`] does
/// that after every block, so an offline program never calls it.
///
/// Made of: [`armed_tracks`] → [`play::rebuild`] when needed →
/// `EngineHandle::send(Record)` → [`play::position`] for the punch-in.
pub fn start_from(rig: &mut Rig, blocks: CaptureConsumer, channels: usize) -> Result<()> {
    begin(
        rig,
        Capture::Manual {
            blocks,
            planes: vec![Vec::new(); channels],
        },
        channels,
    )
}

/// What every start has in common once the capture exists.
fn begin(rig: &mut Rig, capture: Capture, channels: usize) -> Result<()> {
    if rig.recording.is_some() {
        return Err(SuperbError::Message(
            "a take is already being recorded".into(),
        ));
    }
    let tracks = armed_tracks(rig);
    if tracks.is_empty() {
        return Err(SuperbError::Message(
            "arm an audio track before recording".into(),
        ));
    }
    if rig.needs_rebuild {
        play::rebuild(rig)?;
    }
    rig.engine.send(EngineCommand::Record)?;
    rig.recording = Some(Recording {
        capture,
        punch_in: play::position(rig),
        tracks,
        channels,
        #[cfg(feature = "devices")]
        stream: None,
    });
    Ok(())
}

/// Moves waiting blocks into the take when the capture is manual (see
/// [`start_from`]); with a collector thread there is nothing to do.
/// Returns the frames gathered in total, or zero when no take is running.
///
/// Made of: [`steps::collect`].
pub fn collect(rig: &mut Rig) -> usize {
    match rig.recording.as_mut() {
        Some(recording) => {
            steps::collect(&mut recording.capture);
            recording.frames()
        }
        None => 0,
    }
}

/// Stops the take and the transport, stores the captured audio, and places
/// one clip per armed track starting at the punch-in point. Returns the new
/// clips (none if nothing was captured). The whole placement is one undo
/// step called "Record take".
///
/// Made of: `EngineHandle::send(Stop)` → drop the input stream →
/// [`steps::collect`] or `Collector::finish` → [`steps::finish_take`] →
/// [`steps::take_name`] → [`steps::store_take`] → [`steps::place_clip`] per
/// track inside an undo group; marks the rig for a rebuild so the take plays
/// back on its strip.
pub fn stop(rig: &mut Rig) -> Result<Vec<ClipId>> {
    let recording = rig
        .recording
        .take()
        .ok_or_else(|| SuperbError::Message("nothing is being recorded".into()))?;
    rig.engine.send(EngineCommand::Stop)?;
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
    let audio = steps::finish_take(planes);
    if audio.frames() == 0 {
        return Ok(Vec::new());
    }
    let length = TimeDuration::from_samples(audio.frames() as i64);
    let name = steps::take_name(rig);
    steps::store_take(&mut rig.cache, &name, audio);
    rig.history.begin_group("Record take");
    let mut clips = Vec::with_capacity(tracks.len());
    for track in &tracks {
        match steps::place_clip(rig, *track, &name, punch_in, length) {
            Ok(clip) => clips.push(clip),
            Err(e) => {
                rig.history.cancel_group(&mut rig.document)?;
                return Err(e);
            }
        }
    }
    rig.history.end_group();
    rig.needs_rebuild = true;
    Ok(clips)
}

/// The steps `start`, [`start_from`], [`collect`] and [`stop`] are made of.
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
            .name("superb take collector".into())
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
            .map_err(|e| SuperbError::Message(format!("cannot start the take collector: {e}")))?;
        Ok(Collector {
            frames,
            dropped,
            stop,
            handle: Some(handle),
        })
    }

    /// Moves every waiting block into the take when the capture is manual;
    /// a threaded capture needs nothing. Returns the frames appended.
    pub fn collect(capture: &mut Capture) -> usize {
        match capture {
            Capture::Manual { blocks, planes } => append_blocks(blocks, planes),
            Capture::Threaded(_) => 0,
        }
    }

    /// Appends every waiting block to `planes` and hands the blocks back to
    /// the input side. Blocks with fewer channels than the take are padded
    /// with silence. Returns the frames appended. Allocates; never call it
    /// on the audio thread.
    pub fn append_blocks(blocks: &mut CaptureConsumer, planes: &mut [Vec<f32>]) -> usize {
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
    pub fn finish_take(planes: Vec<Vec<f32>>) -> PlanarBuffer {
        let channels = planes.len();
        let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        PlanarBuffer::from_planes(&refs).unwrap_or_else(|_| PlanarBuffer::new(channels, 0))
    }

    /// A cache key for a new take that no earlier take uses: `take 1`,
    /// `take 2`, ...
    pub fn take_name(rig: &Rig) -> String {
        let mut n = rig.cache.len() + 1;
        loop {
            let name = format!("take {n}");
            if rig.cache.get(std::path::Path::new(&name)).is_none() {
                return name;
            }
            n += 1;
        }
    }

    /// Puts the captured audio into the cache under `name`.
    ///
    /// Made of: `AudioCache::insert`.
    pub fn store_take(
        cache: &mut crate::primitives::io::cache::AudioCache,
        name: &str,
        audio: PlanarBuffer,
    ) -> Arc<PlanarBuffer> {
        cache.insert(name, audio)
    }

    /// Places a clip of the cached take on `track`.
    ///
    /// Made of: [`session::add_audio_clip`](crate::session::add_audio_clip).
    pub fn place_clip(
        rig: &mut Rig,
        track: TrackId,
        take: &str,
        start: TimePosition,
        length: TimeDuration,
    ) -> Result<ClipId> {
        crate::session::add_audio_clip(rig, track, take, start, length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::io::capture::capture_queue;
    use crate::primitives::SampleRate;
    use crate::session;

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
    fn a_take_becomes_a_clip_on_every_armed_track() {
        let mut rig = session::create("rec", R, 64).unwrap();
        let vox = session::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let gtr = session::add_track(&mut rig, "gtr", TrackKind::Audio).unwrap();
        let _idle = session::add_track(&mut rig, "idle", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        arm(&mut rig, gtr, true).unwrap();
        assert_eq!(armed_tracks(&rig), [vox, gtr]);
        assert!(!is_recording(&rig));

        // Roll to 640 frames, then punch in there.
        play::start(&mut rig).unwrap();
        play::drive(&mut rig, 10).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        start_from(&mut rig, blocks, 1).unwrap();
        assert!(is_recording(&rig));
        assert_eq!(
            rig.recording.as_ref().unwrap().punch_in,
            TimePosition::from_samples(640)
        );
        play::drive(&mut rig, 5).unwrap();
        assert_eq!(
            play::state(&rig),
            crate::primitives::transport::TransportState::Recording
        );
        feed(&mut mic, 5, 0.25); // 320 frames of "singing"

        let clips = stop(&mut rig).unwrap();
        assert_eq!(clips.len(), 2);
        assert!(!is_recording(&rig));
        for clip in &clips {
            let c = rig.document.clip(*clip).unwrap();
            assert_eq!(c.start(), TimePosition::from_samples(640));
            assert_eq!(c.length(), TimeDuration::from_samples(320));
            assert_eq!(c.name, "take 1");
        }
        assert_eq!(rig.document.clip_track(clips[0]).unwrap(), vox);
        assert_eq!(rig.document.clip_track(clips[1]).unwrap(), gtr);
        assert_eq!(rig.cache.len(), 1);
        assert_eq!(rig.history.undo_label(), Some("Record take"));

        // Stopping stopped the transport; the take plays back from its place.
        play::drive(&mut rig, 1).unwrap();
        assert_eq!(
            play::state(&rig),
            crate::primitives::transport::TransportState::Stopped
        );
        play::seek(&mut rig, TimePosition::from_samples(640)).unwrap();
        play::start(&mut rig).unwrap();
        let heard = play::drive(&mut rig, 4).unwrap();
        assert!(heard.channel(0)[200].abs() > 0.2, "two tracks of the take");
        assert_eq!(rig.live.strips.len(), 3, "one handle per track");

        // One undo removes both clips.
        assert!(session::undo(&mut rig).unwrap());
        assert!(rig.document.clip(clips[0]).is_err() && rig.document.clip(clips[1]).is_err());
        assert!(session::redo(&mut rig).unwrap());
        assert!(rig.document.clip(clips[0]).is_ok());
    }

    #[test]
    fn takes_get_distinct_names_and_empty_takes_leave_no_clip() {
        let mut rig = session::create("rec", R, 64).unwrap();
        let vox = session::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        for i in 1..=2 {
            let (mut mic, blocks) = capture_queue(2, 64, 4).unwrap();
            start_from(&mut rig, blocks, 2).unwrap();
            feed_stereo(&mut mic, 2);
            let clips = stop(&mut rig).unwrap();
            assert_eq!(
                rig.document.clip(clips[0]).unwrap().name,
                format!("take {i}")
            );
        }
        assert_eq!(rig.cache.len(), 2);
        let (_mic, blocks) = capture_queue(2, 64, 4).unwrap();
        start_from(&mut rig, blocks, 2).unwrap();
        assert!(
            stop(&mut rig).unwrap().is_empty(),
            "nothing captured, nothing placed"
        );
        assert_eq!(rig.document.clip_count(), 2);
    }

    fn feed_stereo(producer: &mut crate::primitives::io::capture::CaptureProducer, blocks: usize) {
        let block = PlanarBuffer::new(2, 64);
        for i in 0..blocks {
            assert!(producer.push(&block, i as u64));
        }
    }

    #[test]
    fn errors_are_explicit() {
        let mut rig = session::create("rec", R, 64).unwrap();
        let keys = session::add_track(&mut rig, "keys", TrackKind::Midi).unwrap();
        assert!(
            arm(&mut rig, keys, true).is_err(),
            "MIDI tracks monitor, they do not record audio"
        );
        assert!(
            matches!(stop(&mut rig), Err(SuperbError::Message(_))),
            "nothing recording"
        );
        let (_mic, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(start_from(&mut rig, blocks, 1).is_err(), "no armed track");
        let vox = session::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        let (_a, blocks) = capture_queue(1, 64, 2).unwrap();
        start_from(&mut rig, blocks, 1).unwrap();
        let (_b, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(
            start_from(&mut rig, blocks, 1).is_err(),
            "already recording"
        );
        let _ = format!("{:?}", rig.recording.as_ref().unwrap());
    }

    #[test]
    fn a_collector_thread_gathers_the_take_by_itself() {
        let mut rig = session::create("rec", R, 64).unwrap();
        let vox = session::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        // A queue of 64 blocks fed with 200, paced like a device, and no
        // `collect` calls: the thread keeps up on its own. (A failed push
        // counts as a drop, so the accounting below must balance exactly.)
        let (mut mic, blocks) = capture_queue(1, 64, 64).unwrap();
        let collector = steps::spawn_collector(blocks, 1).unwrap();
        begin(&mut rig, Capture::Threaded(collector), 1).unwrap();
        let mut block = PlanarBuffer::new(1, 64);
        block.fill(0.5);
        for i in 0..200u64 {
            mic.push(&block, i);
            std::thread::sleep(core::time::Duration::from_micros(300));
        }
        // `collect` has nothing to do for a threaded capture; it reports.
        let _ = collect(&mut rig);
        let _ = format!("{:?}", rig.recording.as_ref().unwrap());
        let clips = stop(&mut rig).unwrap();
        let dropped = mic.dropped() as usize;
        let clip = rig.document.clip(clips[0]).unwrap();
        assert_eq!(
            clip.length(),
            TimeDuration::from_samples(((200 - dropped) * 64) as i64),
            "every block pushed was gathered or counted as dropped"
        );
        assert!(
            dropped < 100,
            "the collector fell far behind: {dropped} dropped"
        );
        let take = rig.cache.get("take 1".as_ref()).unwrap();
        assert!(take.channel(0).iter().all(|&s| s == 0.5));

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
        let audio = steps::finish_take(planes);
        assert_eq!(audio.channels(), 2);
        assert_eq!(audio.channel(0), &[1.0; 8]);
        assert_eq!(audio.channel(1), &[0.0; 8]);
        assert_eq!(steps::finish_take(vec![Vec::new(); 2]).frames(), 0);
    }

    #[test]
    fn collecting_as_you_go_outlasts_the_queue() {
        let mut rig = session::create("rec", R, 64).unwrap();
        let vox = session::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        // A queue of only two blocks; the take is ten blocks long.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        start_from(&mut rig, blocks, 1).unwrap();
        for i in 0..10 {
            feed(&mut mic, 1, 0.5);
            // Advancing the engine collects for us (a UI timer would call
            // `collect` when a device drives the engine).
            play::drive(&mut rig, 1).unwrap();
            assert_eq!(rig.recording.as_ref().unwrap().frames(), (i + 1) * 64);
        }
        assert_eq!(rig.recording.as_ref().unwrap().dropped(), 0);
        assert_eq!(collect(&mut rig), 640);
        let clips = stop(&mut rig).unwrap();
        assert_eq!(
            rig.document.clip(clips[0]).unwrap().length(),
            TimeDuration::from_samples(640)
        );
        assert_eq!(collect(&mut rig), 0, "nothing recording");

        // Without collecting, the input side reports what it had to drop.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        start_from(&mut rig, blocks, 1).unwrap();
        feed_lossy(&mut mic, 5, 0.5);
        assert_eq!(rig.recording.as_ref().unwrap().dropped(), 3);
        stop(&mut rig).unwrap();
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
