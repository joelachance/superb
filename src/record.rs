//! Recording: arm tracks, capture input, turn the take into a clip.
//!
//! A take flows through a [`CaptureConsumer`]: the input side fills it with
//! blocks (a real device behind the `devices` feature, or anything else
//! that produces planar audio), [`collect`] moves them into the growing take
//! on the control thread, and [`stop`] puts the take in the audio cache and
//! places a clip on every armed track, as one undo step. The steps are
//! public so a program can record from any source it likes.
//!
//! The queue holds only a fraction of a second, so while recording from a
//! device call [`collect`] regularly (a UI timer is fine).
//! [`play::advance`] and [`stop`] call it for you.
//!
//! Made of `superb::primitives::io::capture` (the queue),
//! `superb::primitives::io::device` (hardware input, feature `devices`),
//! the audio cache, and the session's clip commands.

use std::sync::Arc;

use crate::play;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::io::live::EngineCommand;
use crate::primitives::session::commands;
use crate::primitives::session::TrackKind;
use crate::primitives::{ClipId, Result, SuperbError, TimeDuration, TimePosition, TrackId};
use crate::rig::Rig;

/// A take in progress.
pub struct Recording {
    /// Captured blocks arriving from the input.
    pub blocks: CaptureConsumer,
    /// Audio collected so far, one plane per channel. Grows on the control
    /// thread as [`collect`] runs; becomes the cached take at [`stop`].
    pub take: Vec<Vec<f32>>,
    /// Where on the timeline the take started.
    pub punch_in: TimePosition,
    /// Tracks that were armed when recording started; each gets a clip.
    pub tracks: Vec<TrackId>,
    /// Channels in the captured blocks.
    pub channels: usize,
    /// The input stream keeping the capture alive, when recording from hardware.
    #[cfg(feature = "devices")]
    pub stream: Option<crate::primitives::io::device::InputStream>,
}

impl Recording {
    /// Frames collected into the take so far.
    pub fn frames(&self) -> usize {
        self.take.first().map_or(0, Vec::len)
    }

    /// Blocks the input side could not queue because [`collect`] was not
    /// called in time. Anything above zero means a gap in the take.
    pub fn dropped(&self) -> u64 {
        self.blocks.dropped()
    }
}

impl core::fmt::Debug for Recording {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Recording")
            .field("punch_in", &self.punch_in)
            .field("tracks", &self.tracks)
            .field("channels", &self.channels)
            .field("frames", &self.frames())
            .field("blocks_waiting", &self.blocks.len())
            .field("dropped", &self.dropped())
            .finish()
    }
}

/// Arms or disarms an audio track. Armed tracks receive a clip when a take
/// is stopped.
///
/// Made of: push `set_track_armed` (its own undo step).
pub fn arm(rig: &mut Rig, track: TrackId, on: bool) -> Result<()> {
    if rig.session.track(track)?.kind != TrackKind::Audio {
        return Err(SuperbError::InvalidArgument {
            what: "only audio tracks are armed for recording",
        });
    }
    crate::project::switch(rig, commands::set_track_armed(track, on))
}

/// The audio tracks currently armed, in track order.
pub fn armed_tracks(rig: &Rig) -> Vec<TrackId> {
    rig.session
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
/// current playhead position. The transport starts rolling. Keep calling
/// [`collect`] while it does.
///
/// Made of: [`steps::open_input`] with 64 queued blocks → [`steps::begin`].
#[cfg(feature = "devices")]
pub fn start(rig: &mut Rig, input: &crate::primitives::io::device::InputConfig) -> Result<()> {
    let (stream, blocks) = steps::open_input(input, 64)?;
    let channels = stream.channels();
    steps::begin(rig, blocks, channels)?;
    if let Some(recording) = rig.recording.as_mut() {
        recording.stream = Some(stream);
    }
    Ok(())
}

/// Moves the blocks captured so far into the take. Returns the frames
/// collected in total; does nothing when no take is running.
///
/// Made of: [`steps::collect`].
pub fn collect(rig: &mut Rig) -> usize {
    match rig.recording.as_mut() {
        Some(recording) => {
            steps::collect(recording);
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
/// Made of: `LiveEngine::send(Stop)` → [`steps::collect`] →
/// [`steps::finish_take`] → [`steps::take_name`] → [`steps::store_take`] →
/// [`steps::place_clip`] per track inside an undo group; marks the rig for
/// a sync so the take plays back on its strip.
pub fn stop(rig: &mut Rig) -> Result<Vec<ClipId>> {
    let mut recording = rig
        .recording
        .take()
        .ok_or_else(|| SuperbError::Message("nothing is being recorded".into()))?;
    rig.engine.send(EngineCommand::Stop)?;
    #[cfg(feature = "devices")]
    drop(recording.stream.take());
    steps::collect(&mut recording);
    let audio = steps::finish_take(recording.take);
    if audio.frames() == 0 {
        return Ok(Vec::new());
    }
    let length = TimeDuration::from_samples(audio.frames() as i64);
    let name = steps::take_name(rig);
    steps::store_take(&mut rig.cache, &name, audio);
    rig.history.begin_group("Record take");
    let mut clips = Vec::with_capacity(recording.tracks.len());
    for track in &recording.tracks {
        match steps::place_clip(rig, *track, &name, recording.punch_in, length) {
            Ok(clip) => clips.push(clip),
            Err(e) => {
                rig.history.cancel_group(&mut rig.session)?;
                return Err(e);
            }
        }
    }
    rig.history.end_group();
    rig.needs_sync = true;
    Ok(clips)
}

/// The steps `start`, [`collect`] and [`stop`] are made of, for recording
/// from a source of your own.
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

    /// Begins a take from any capture source at the current playhead: the
    /// console is synced if needed, the transport switches to recording,
    /// and the take is remembered in [`Rig::recording`]. Fails if no audio
    /// track is armed or a take is already running.
    ///
    /// Made of: [`play::sync`] when needed → `LiveEngine::send(Record)`.
    pub fn begin(rig: &mut Rig, blocks: CaptureConsumer, channels: usize) -> Result<()> {
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
        if rig.needs_sync {
            play::sync(rig)?;
        }
        rig.engine.send(EngineCommand::Record)?;
        rig.recording = Some(Recording {
            blocks,
            take: vec![Vec::new(); channels],
            punch_in: play::position(rig),
            tracks,
            channels,
            #[cfg(feature = "devices")]
            stream: None,
        });
        Ok(())
    }

    /// Appends every waiting block to the take and hands the blocks back
    /// to the input side. Blocks with fewer channels than the take are
    /// padded with silence. Returns the frames appended. Allocates; control
    /// thread only.
    pub fn collect(recording: &mut Recording) -> usize {
        let mut appended = 0;
        while let Some(block) = recording.blocks.pop() {
            let frames = block.frames();
            for (ch, plane) in recording.take.iter_mut().enumerate() {
                if ch < block.channels() {
                    plane.extend_from_slice(block.buffer().channel(ch));
                } else {
                    plane.extend(std::iter::repeat_n(0.0, frames));
                }
            }
            appended += frames;
            recording.blocks.recycle(block);
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
        cache: &mut crate::play::AudioCache,
        name: &str,
        audio: PlanarBuffer,
    ) -> Arc<PlanarBuffer> {
        cache.insert(name, audio)
    }

    /// Places a clip of the cached take on `track`.
    ///
    /// Made of: [`project::add_audio_clip`](crate::project::add_audio_clip).
    pub fn place_clip(
        rig: &mut Rig,
        track: TrackId,
        take: &str,
        start: TimePosition,
        length: TimeDuration,
    ) -> Result<ClipId> {
        crate::project::add_audio_clip(rig, track, take, start, length)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::io::capture::capture_queue;
    use crate::primitives::SampleRate;
    use crate::project;

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
        let mut rig = project::create("rec", R, 64).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let gtr = project::add_track(&mut rig, "gtr", TrackKind::Audio).unwrap();
        let _idle = project::add_track(&mut rig, "idle", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        arm(&mut rig, gtr, true).unwrap();
        assert_eq!(armed_tracks(&rig), [vox, gtr]);
        assert!(!is_recording(&rig));

        // Roll to 640 frames, then punch in there.
        play::start(&mut rig).unwrap();
        play::advance(&mut rig, 10).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 16).unwrap();
        steps::begin(&mut rig, blocks, 1).unwrap();
        assert!(is_recording(&rig));
        assert_eq!(
            rig.recording.as_ref().unwrap().punch_in,
            TimePosition::from_samples(640)
        );
        play::advance(&mut rig, 5).unwrap();
        assert_eq!(
            play::state(&rig),
            crate::primitives::transport::TransportState::Recording
        );
        feed(&mut mic, 5, 0.25); // 320 frames of "singing"

        let clips = stop(&mut rig).unwrap();
        assert_eq!(clips.len(), 2);
        assert!(!is_recording(&rig));
        for clip in &clips {
            let c = rig.session.clip(*clip).unwrap();
            assert_eq!(c.start(), TimePosition::from_samples(640));
            assert_eq!(c.length(), TimeDuration::from_samples(320));
            assert_eq!(c.name, "take 1");
        }
        assert_eq!(rig.session.clip_track(clips[0]).unwrap(), vox);
        assert_eq!(rig.session.clip_track(clips[1]).unwrap(), gtr);
        assert_eq!(rig.cache.len(), 1);
        assert_eq!(rig.history.undo_label(), Some("Record take"));

        // Stopping stopped the transport; the take plays back from its place.
        play::advance(&mut rig, 1).unwrap();
        assert_eq!(
            play::state(&rig),
            crate::primitives::transport::TransportState::Stopped
        );
        play::seek(&mut rig, TimePosition::from_samples(640)).unwrap();
        play::start(&mut rig).unwrap();
        let heard = play::advance(&mut rig, 4).unwrap();
        assert!(heard.channel(0)[200].abs() > 0.2, "two tracks of the take");
        assert_eq!(rig.live.strips.len(), 3, "one handle per track");

        // One undo removes both clips.
        assert!(project::undo(&mut rig).unwrap());
        assert!(rig.session.clip(clips[0]).is_err() && rig.session.clip(clips[1]).is_err());
        assert!(project::redo(&mut rig).unwrap());
        assert!(rig.session.clip(clips[0]).is_ok());
    }

    #[test]
    fn takes_get_distinct_names_and_empty_takes_leave_no_clip() {
        let mut rig = project::create("rec", R, 64).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        for i in 1..=2 {
            let (mut mic, blocks) = capture_queue(2, 64, 4).unwrap();
            steps::begin(&mut rig, blocks, 2).unwrap();
            feed_stereo(&mut mic, 2);
            let clips = stop(&mut rig).unwrap();
            assert_eq!(
                rig.session.clip(clips[0]).unwrap().name,
                format!("take {i}")
            );
        }
        assert_eq!(rig.cache.len(), 2);
        let (_mic, blocks) = capture_queue(2, 64, 4).unwrap();
        steps::begin(&mut rig, blocks, 2).unwrap();
        assert!(
            stop(&mut rig).unwrap().is_empty(),
            "nothing captured, nothing placed"
        );
        assert_eq!(rig.session.clip_count(), 2);
    }

    fn feed_stereo(producer: &mut crate::primitives::io::capture::CaptureProducer, blocks: usize) {
        let block = PlanarBuffer::new(2, 64);
        for i in 0..blocks {
            assert!(producer.push(&block, i as u64));
        }
    }

    #[test]
    fn errors_are_explicit() {
        let mut rig = project::create("rec", R, 64).unwrap();
        let keys = project::add_track(&mut rig, "keys", TrackKind::Midi).unwrap();
        assert!(
            arm(&mut rig, keys, true).is_err(),
            "MIDI tracks monitor, they do not record audio"
        );
        assert!(
            matches!(stop(&mut rig), Err(SuperbError::Message(_))),
            "nothing recording"
        );
        let (_mic, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(steps::begin(&mut rig, blocks, 1).is_err(), "no armed track");
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        let (_a, blocks) = capture_queue(1, 64, 2).unwrap();
        steps::begin(&mut rig, blocks, 1).unwrap();
        let (_b, blocks) = capture_queue(1, 64, 2).unwrap();
        assert!(
            steps::begin(&mut rig, blocks, 1).is_err(),
            "already recording"
        );
        let _ = format!("{:?}", rig.recording.as_ref().unwrap());
    }

    #[test]
    fn collect_pads_missing_channels_with_silence() {
        let (mut mic, blocks) = capture_queue(1, 4, 4).unwrap();
        let mut block = PlanarBuffer::new(1, 4);
        block.fill(1.0);
        mic.push(&block, 0);
        mic.push(&block, 1);
        let mut recording = Recording {
            blocks,
            take: vec![Vec::new(); 2],
            punch_in: TimePosition::ZERO,
            tracks: Vec::new(),
            channels: 2,
            #[cfg(feature = "devices")]
            stream: None,
        };
        assert_eq!(steps::collect(&mut recording), 8);
        assert_eq!(recording.frames(), 8);
        assert!(recording.blocks.is_empty());
        assert_eq!(steps::collect(&mut recording), 0);
        let audio = steps::finish_take(recording.take);
        assert_eq!(audio.channels(), 2);
        assert_eq!(audio.channel(0), &[1.0; 8]);
        assert_eq!(audio.channel(1), &[0.0; 8]);
        assert_eq!(steps::finish_take(vec![Vec::new(); 2]).frames(), 0);
    }

    #[test]
    fn collecting_as_you_go_outlasts_the_queue() {
        let mut rig = project::create("rec", R, 64).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        arm(&mut rig, vox, true).unwrap();
        // A queue of only two blocks; the take is ten blocks long.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        steps::begin(&mut rig, blocks, 1).unwrap();
        for i in 0..10 {
            feed(&mut mic, 1, 0.5);
            // Advancing the engine collects for us (a UI timer would call
            // `collect` when a device drives the engine).
            play::advance(&mut rig, 1).unwrap();
            assert_eq!(rig.recording.as_ref().unwrap().frames(), (i + 1) * 64);
        }
        assert_eq!(rig.recording.as_ref().unwrap().dropped(), 0);
        assert_eq!(collect(&mut rig), 640);
        let clips = stop(&mut rig).unwrap();
        assert_eq!(
            rig.session.clip(clips[0]).unwrap().length(),
            TimeDuration::from_samples(640)
        );
        assert_eq!(collect(&mut rig), 0, "nothing recording");

        // Without collecting, the input side reports what it had to drop.
        let (mut mic, blocks) = capture_queue(1, 64, 2).unwrap();
        steps::begin(&mut rig, blocks, 1).unwrap();
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
