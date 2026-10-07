//! The engine block loop, and the live engine split into a control handle
//! and an audio thread ([`Engine::live`]).

mod input;
mod live;

pub use input::LiveInput;
pub use live::{
    AudioThread, BlockStart, EngineCommand, EngineHandle, RollStart, COMMAND_CAPACITY,
    MAX_EXTRA_MIDI_INPUTS, MIDI_CAPACITY, MIDI_CAPTURE_CAPACITY, RETIRED_INPUT_CAPACITY,
    RETIRED_MAP_CAPACITY, SWAP_CAPACITY,
};

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::{BlockProcessor, BlockTime};
use crate::primitives::midi::timing::FrameMapper;
use crate::primitives::midi::{MidiEvent, MidiEventBuffer, MidiMessage, TimestampedMessage};
use crate::primitives::realtime::spsc::{Consumer, Producer};
use crate::primitives::realtime::{LoadMeter, ProcessBudget};
use crate::primitives::transport::{Transport, TransportState};
use crate::primitives::{Result, SampleRate, SuperbError, TimePosition};

/// Counters maintained by [`Engine::render`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EngineStats {
    /// Blocks rendered.
    pub blocks: usize,
    /// Transport segments processed (more than `blocks` when looping).
    pub segments: usize,
    /// MIDI messages dropped because the block buffer was full.
    pub midi_dropped: usize,
    /// Incoming MIDI a recording missed because the capture queue was full.
    pub capture_dropped: usize,
}

/// One incoming MIDI message a recording captured, stamped with the
/// transport position it arrived at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapturedMidi {
    /// Where the transport was when the message arrived.
    pub position: TimePosition,
    /// The message.
    pub message: MidiMessage,
}

/// How [`Engine::render`] turns a queued [`TimestampedMessage`] into a
/// block-relative [`MidiEvent::frame`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MidiTiming {
    /// Every queued message lands at frame 0 of the block it is drained in.
    /// Timestamps are ignored, which is the right choice when the producer's
    /// clock is unrelated to the engine's.
    BlockStart,
    /// Timestamps are mapped through a [`FrameMapper`] whose block start is
    /// the engine's **stream clock**: `frames_rendered * 1_000_000 / hz`
    /// microseconds, or the value given to
    /// [`Engine::set_midi_block_start_us`] for this block. Messages stamped
    /// on that clock are placed sample-accurately; earlier ones clamp to
    /// frame 0, later ones to the last frame.
    #[default]
    Stream,
}

/// Transport + processor + MIDI delivery for one audio callback.
///
/// The engine owns everything the audio thread touches. Build it off the
/// audio thread, call [`Engine::prepare`], then move it into the callback
/// (see `superb::primitives::io::device::run_engine`) or drive it with
/// [`offline::render`](crate::primitives::io::offline::render).
pub struct Engine<P: BlockProcessor> {
    processor: P,
    transport: Transport,
    midi: MidiEventBuffer,
    midi_in: Option<Consumer<TimestampedMessage>>,
    prepared: Option<(SampleRate, usize)>,
    load: Arc<LoadMeter>,
    stats: EngineStats,
    midi_dropped: Arc<AtomicUsize>,
    frame_mapper: FrameMapper,
    /// Frames handed to `render` so far since `prepare` — the stream clock.
    frames_rendered: u64,
    midi_timing: MidiTiming,
    /// One-shot block start (µs) for the next `render`, if set.
    midi_block_start_us: Option<u64>,
    /// Captured audio on its way to the nodes, if an input is attached.
    live_input: Option<LiveInput>,
    /// Where incoming MIDI goes while a recording captures it.
    midi_capture: Option<Producer<CapturedMidi>>,
    capturing_midi: bool,
}

impl<P: BlockProcessor> Engine<P> {
    /// Default capacity of the per-block MIDI buffer.
    pub const DEFAULT_MIDI_CAPACITY: usize = 1024;

    /// Creates an engine. Nothing is prepared yet.
    pub fn new(processor: P, transport: Transport) -> Self {
        let frame_mapper = FrameMapper::new(transport.sample_rate());
        Self {
            processor,
            transport,
            midi: MidiEventBuffer::with_capacity(Self::DEFAULT_MIDI_CAPACITY),
            midi_in: None,
            prepared: None,
            load: Arc::new(LoadMeter::new()),
            stats: EngineStats::default(),
            midi_dropped: Arc::new(AtomicUsize::new(0)),
            frame_mapper,
            frames_rendered: 0,
            midi_timing: MidiTiming::default(),
            midi_block_start_us: None,
            live_input: None,
            midi_capture: None,
            capturing_midi: false,
        }
    }

    /// The queue incoming MIDI goes into while a recording captures it
    /// ([`CapturedMidi`]); the control side drains it. The producer stays
    /// here for the engine's life, [`capture_midi`](Self::capture_midi)
    /// switches capture on and off.
    pub fn set_midi_capture(&mut self, producer: Option<Producer<CapturedMidi>>) {
        self.midi_capture = producer;
    }

    /// Captures incoming MIDI into the capture queue while the transport is
    /// recording, stamped with the position it arrived at; or stops.
    pub fn capture_midi(&mut self, on: bool) {
        self.capturing_midi = on;
    }

    /// Whether incoming MIDI is being captured.
    pub fn is_capturing_midi(&self) -> bool {
        self.capturing_midi
    }

    /// Prepares the processor and aligns the transport's sample rate.
    ///
    /// Off the audio thread; allocates.
    pub fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        if max_frames == 0 {
            return Err(SuperbError::InvalidArgument { what: "max_frames" });
        }
        self.transport.tempo_map_mut().set_sample_rate(sample_rate);
        self.processor.prepare(sample_rate, max_frames)?;
        self.prepared = Some((sample_rate, max_frames));
        // The stream clock restarts with the stream.
        self.frame_mapper = FrameMapper::new(sample_rate);
        self.frames_rendered = 0;
        self.midi_block_start_us = None;
        Ok(())
    }

    /// `(sample_rate, max_frames)` from the last `prepare`.
    pub fn prepared_config(&self) -> Option<(SampleRate, usize)> {
        self.prepared
    }

    /// The transport.
    pub fn transport(&self) -> &Transport {
        &self.transport
    }

    /// Mutable transport access (play/stop/seek/loop).
    pub fn transport_mut(&mut self) -> &mut Transport {
        &mut self.transport
    }

    /// The processor.
    pub fn processor(&self) -> &P {
        &self.processor
    }

    /// Mutable processor access. Editing a graph requires `prepare` again.
    pub fn processor_mut(&mut self) -> &mut P {
        &mut self.processor
    }

    /// Consumes the engine, returning its parts.
    pub fn into_parts(self) -> (P, Transport) {
        (self.processor, self.transport)
    }

    /// MIDI events for the *next* block. Frame offsets are relative to the
    /// block start; the buffer is cleared after each `render`.
    pub fn midi_mut(&mut self) -> &mut MidiEventBuffer {
        &mut self.midi
    }

    /// Attaches a queue of timestamped messages (e.g. from
    /// `superb::primitives::midi::input::MidiInputBridge`).
    ///
    /// Messages are drained at the start of every `render` and placed in
    /// that block according to [`midi_timing`](Engine::midi_timing): with
    /// the default [`MidiTiming::Stream`] each `timestamp_us` is mapped
    /// through a [`FrameMapper`] against the engine's stream clock (see
    /// [`stream_clock_us`](Engine::stream_clock_us)), so producers that
    /// stamp messages on that clock get sample-accurate placement.
    /// Timestamps from a different clock (e.g. midir's connection clock
    /// against cpal's stream clock) are clamped into the block rather than
    /// rejected; use [`MidiTiming::BlockStart`] if you only want frame 0.
    ///
    /// ```
    /// use superb::primitives::buffers::PlanarBuffer;
    /// use superb::primitives::SampleRate;
    /// use superb::primitives::graph::nodes::MonoSynthNode;
    /// use superb::primitives::graph::AudioGraph;
    /// use superb::primitives::io::{Engine, MidiTiming};
    /// use superb::primitives::midi::{MidiChannel, MidiMessage, TimestampedMessage};
    /// use superb::primitives::transport::Transport;
    ///
    /// let mut graph = AudioGraph::new();
    /// let synth = graph.add_node(Box::new(MonoSynthNode::new(1)));
    /// graph.set_output(synth).unwrap();
    /// let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
    /// engine.prepare(SampleRate::HZ_48K, 480).unwrap(); // 480 frames = 10 ms
    /// assert_eq!(engine.midi_timing(), MidiTiming::Stream);
    ///
    /// let (mut tx, rx) = superb::primitives::realtime::spsc::channel::<TimestampedMessage>(16);
    /// engine.set_midi_input(Some(rx));
    ///
    /// // Stamp on the engine's stream clock: block 0 spans 0..10_000 µs.
    /// let ch = MidiChannel::new(0).unwrap();
    /// let at = engine.stream_clock_us() + 5_000; // frame 240
    /// tx.push(TimestampedMessage { timestamp_us: at, message: MidiMessage::NoteOn { channel: ch, note: 69, velocity: 127 } }).unwrap();
    ///
    /// let mut out = PlanarBuffer::new(1, 480);
    /// engine.render(&mut out).unwrap();
    /// assert!(out.channel(0)[..240].iter().all(|&s| s == 0.0));
    /// assert!(out.channel(0)[240..].iter().any(|&s| s != 0.0));
    /// assert_eq!(engine.stream_clock_us(), 10_000); // next block's start
    /// ```
    pub fn set_midi_input(&mut self, consumer: Option<Consumer<TimestampedMessage>>) {
        self.midi_in = consumer;
    }

    /// Attaches captured audio as the live input every node sees as
    /// `ProcessContext::input`, or detaches it with `None`. Returns the
    /// input that was attached before; on the audio thread, hand that one
    /// back to the control thread rather than dropping it here.
    pub fn set_live_input(&mut self, input: Option<LiveInput>) -> Option<LiveInput> {
        core::mem::replace(&mut self.live_input, input)
    }

    /// The live input, if one is attached.
    pub fn live_input(&self) -> Option<&LiveInput> {
        self.live_input.as_ref()
    }

    /// Chooses how queued messages are placed within a block.
    pub fn set_midi_timing(&mut self, timing: MidiTiming) {
        self.midi_timing = timing;
    }

    /// Current MIDI placement mode (default [`MidiTiming::Stream`]).
    pub fn midi_timing(&self) -> MidiTiming {
        self.midi_timing
    }

    /// Overrides the block-start timestamp used by the **next** `render`
    /// only (one-shot; it is consumed by that render and cleared by
    /// `prepare`). A device callback that knows its own clock — e.g. cpal's
    /// `OutputCallbackInfo` — sets this right before `render` so queued
    /// messages stamped on that clock are placed exactly. `None` cancels a
    /// pending override and the auto stream clock is used.
    pub fn set_midi_block_start_us(&mut self, block_start_us: Option<u64>) {
        self.midi_block_start_us = block_start_us;
    }

    /// Frames rendered since the last `prepare` (advanced by every `render`
    /// that reached the processor, whether or not it succeeded).
    pub fn frames_rendered(&self) -> u64 {
        self.frames_rendered
    }

    /// The auto stream clock: the timestamp (µs) of the next block's first
    /// frame, `frames_rendered * 1_000_000 / sample_rate`. Test and offline
    /// producers stamp `TimestampedMessage`s on this clock for
    /// sample-accurate placement.
    pub fn stream_clock_us(&self) -> u64 {
        let hz = self
            .prepared
            .map_or_else(|| self.transport.sample_rate(), |(sr, _)| sr)
            .hz();
        frames_to_us(self.frames_rendered, hz)
    }

    /// DSP load meter shared with UI threads.
    pub fn load(&self) -> Arc<LoadMeter> {
        Arc::clone(&self.load)
    }

    /// Counters from all renders so far.
    pub fn stats(&self) -> EngineStats {
        EngineStats {
            midi_dropped: self.midi_dropped.load(Ordering::Relaxed),
            ..self.stats
        }
    }

    /// Output channel count of the processor.
    pub fn output_channels(&self) -> usize {
        self.processor.output_channels()
    }

    /// Renders one block into `out` (its active frame count is the block size).
    ///
    /// Allocation-free. Fails if unprepared or the block is larger than the
    /// prepared maximum. `out` may have any channel count: the processor
    /// output is adapted with [`PlanarBuffer::copy_block_from`].
    pub fn render(&mut self, out: &mut PlanarBuffer) -> Result<()> {
        let (sample_rate, max_frames) = self.prepared.ok_or(SuperbError::NotPrepared)?;
        let frames = out.frames();
        if frames > max_frames {
            return Err(SuperbError::CapacityExceeded {
                what: "block frames",
                capacity: max_frames,
            });
        }
        let timer = ProcessBudget::for_block(frames, sample_rate).start();

        // Drain queued external MIDI into this block.
        let block_start_us = self
            .midi_block_start_us
            .take()
            .unwrap_or_else(|| frames_to_us(self.frames_rendered, sample_rate.hz()));
        self.frame_mapper
            .set_block(block_start_us, frames.min(u32::MAX as usize) as u32);
        if let Some(rx) = self.midi_in.as_mut() {
            while let Some(msg) = rx.pop() {
                let frame = match self.midi_timing {
                    MidiTiming::BlockStart => 0,
                    MidiTiming::Stream => self.frame_mapper.frame_for(msg.timestamp_us),
                };
                if self
                    .midi
                    .push(MidiEvent {
                        frame,
                        message: msg.message,
                    })
                    .is_err()
                {
                    self.midi_dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        self.midi.sort_by_frame();
        // A recording takes a copy of what arrived, stamped where it arrived.
        if self.capturing_midi && self.transport.state() == TransportState::Recording {
            if let Some(tx) = self.midi_capture.as_mut() {
                let start = self.transport.position();
                for event in self.midi.iter() {
                    let captured = CapturedMidi {
                        position: start.offset(i64::from(event.frame)),
                        message: event.message,
                    };
                    if tx.push(captured).is_err() {
                        self.stats.capture_dropped += 1;
                    }
                }
            }
        }

        // Captured audio for this block, if an input is attached.
        if let Some(live) = self.live_input.as_mut() {
            live.pull();
            live.read_block(frames);
        }

        let mut result = Ok(());
        for seg in self.transport.advance(frames) {
            let time = BlockTime::from_transport(&self.transport, &seg);
            let input = self
                .live_input
                .as_mut()
                .map(|live| live.segment(seg.block_offset, seg.frames));
            if let Err(e) = self.processor.process(time, seg.frames, &self.midi, input) {
                result = Err(e);
                break;
            }
            if let Err(e) =
                out.copy_block_from(seg.block_offset, self.processor.output(), seg.frames)
            {
                result = Err(e);
                break;
            }
            self.stats.segments += 1;
            // Events belong to the first segment only.
            self.midi.clear();
        }
        self.midi.clear();
        self.stats.blocks += 1;
        self.frames_rendered += frames as u64;
        self.load.record(&timer.finish());
        result
    }
}

/// Timestamp (µs) of sample `frames` at `hz`, rounded **up** so that
/// `superb::primitives::midi::timing::us_to_frames(frames_to_us(f), hz) == f` for every
/// `f` (floor/floor would land one sample early for non-integer periods).
fn frames_to_us(frames: u64, hz: u32) -> u64 {
    let hz = hz.max(1) as u128;
    ((frames as u128 * 1_000_000).div_ceil(hz)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::nodes::{ConstantNode, MonoSynthNode, SineNode};
    use crate::primitives::graph::AudioGraph;
    use crate::primitives::midi::{MidiChannel, MidiMessage};
    use crate::primitives::transport::LoopRange;
    use crate::primitives::TimePosition;

    const R: SampleRate = SampleRate::HZ_48K;

    fn constant_engine(value: f32, channels: usize) -> Engine<AudioGraph> {
        let mut g = AudioGraph::new();
        let c = g.add_node(Box::new(ConstantNode::new(value, channels)));
        g.set_output(c).unwrap();
        Engine::new(g, Transport::new(R, 120.0))
    }

    #[test]
    fn render_requires_prepare_and_respects_max_frames() {
        let mut e = constant_engine(1.0, 2);
        let mut out = PlanarBuffer::new(2, 64);
        assert_eq!(e.render(&mut out), Err(SuperbError::NotPrepared));
        assert!(e.prepare(R, 0).is_err());
        e.prepare(R, 32).unwrap();
        assert!(matches!(
            e.render(&mut out),
            Err(SuperbError::CapacityExceeded { .. })
        ));
        out.set_frames(32).unwrap();
        e.render(&mut out).unwrap();
        assert_eq!(out.channel(1), &[1.0; 32]);
        assert_eq!(e.prepared_config(), Some((R, 32)));
        assert_eq!(e.output_channels(), 2);
        assert_eq!(e.stats().blocks, 1);
        assert_eq!(e.stats().segments, 1);
        assert_eq!(e.load().callbacks(), 1);
    }

    #[test]
    fn stopped_transport_still_renders_but_does_not_move() {
        let mut e = constant_engine(0.5, 1);
        e.prepare(R, 16).unwrap();
        let mut out = PlanarBuffer::new(1, 16);
        e.render(&mut out).unwrap();
        assert_eq!(e.transport().position(), TimePosition::ZERO);
        e.transport_mut().play();
        e.render(&mut out).unwrap();
        assert_eq!(e.transport().position().samples(), 16);
    }

    #[test]
    fn loop_wrap_splits_block_into_segments_and_output_is_contiguous() {
        let mut g = AudioGraph::new();
        let s = g.add_node(Box::new(SineNode::new(1_000.0, 1.0, 1)));
        g.set_output(s).unwrap();
        let mut e = Engine::new(g, Transport::new(R, 120.0));
        e.prepare(R, 100).unwrap();
        let t = e.transport_mut();
        t.set_loop(Some(
            LoopRange::new(
                TimePosition::from_samples(0),
                TimePosition::from_samples(150),
            )
            .unwrap(),
        ));
        t.set_loop_enabled(true);
        t.play();
        let mut out = PlanarBuffer::new(1, 100);
        e.render(&mut out).unwrap(); // 0..100
        e.render(&mut out).unwrap(); // 100..150 + 0..50
        assert_eq!(e.stats().segments, 3);
        assert_eq!(e.transport().position().samples(), 50);
        // whole block written (no zeros left from clear) — sine at 1 kHz never
        // has 50 consecutive exact zeros
        assert!(out.channel(0)[50..].iter().any(|&x| x != 0.0));
    }

    #[test]
    fn output_channel_adaptation() {
        let mut e = constant_engine(0.5, 1);
        e.prepare(R, 8).unwrap();
        let mut stereo = PlanarBuffer::new(2, 8);
        e.render(&mut stereo).unwrap();
        assert_eq!(stereo.channel(0), &[0.5; 8]);
        assert_eq!(stereo.channel(1), &[0.5; 8]);
    }

    #[test]
    fn midi_from_queue_and_block_buffer_reaches_nodes() {
        let mut g = AudioGraph::new();
        let synth = g.add_node(Box::new(MonoSynthNode::new(1)));
        g.set_output(synth).unwrap();
        let mut e = Engine::new(g, Transport::new(R, 120.0));
        e.prepare(R, 64).unwrap();
        let ch = MidiChannel::new(0).unwrap();

        let (mut tx, rx) = crate::primitives::realtime::spsc::channel::<TimestampedMessage>(8);
        e.set_midi_input(Some(rx));
        tx.push(TimestampedMessage {
            timestamp_us: 0,
            message: MidiMessage::NoteOn {
                channel: ch,
                note: 69,
                velocity: 127,
            },
        })
        .unwrap();

        let mut out = PlanarBuffer::new(1, 64);
        e.render(&mut out).unwrap();
        assert!(out.peak() > 0.9, "note from queue sounds in the same block");
        assert!(e.midi_mut().is_empty(), "block buffer cleared after render");

        // Block buffer events at a frame offset
        e.midi_mut()
            .push(MidiEvent {
                frame: 32,
                message: MidiMessage::NoteOff {
                    channel: ch,
                    note: 69,
                    velocity: 0,
                },
            })
            .unwrap();
        e.render(&mut out).unwrap();
        assert!(out.channel(0)[..32].iter().any(|&x| x != 0.0));
        assert!(out.channel(0)[32..].iter().all(|&x| x == 0.0));

        // Overflow of the block buffer is counted, not fatal.
        let (mut tx2, rx2) = crate::primitives::realtime::spsc::channel::<TimestampedMessage>(4096);
        e.set_midi_input(Some(rx2));
        for _ in 0..(Engine::<AudioGraph>::DEFAULT_MIDI_CAPACITY + 5) {
            tx2.push(TimestampedMessage {
                timestamp_us: 0,
                message: MidiMessage::TimingClock,
            })
            .unwrap();
        }
        e.render(&mut out).unwrap();
        assert_eq!(e.stats().midi_dropped, 5);
        drop(tx);
    }

    fn synth_engine(
        max_frames: usize,
    ) -> (
        Engine<AudioGraph>,
        crate::primitives::realtime::spsc::Producer<TimestampedMessage>,
    ) {
        let mut g = AudioGraph::new();
        let synth = g.add_node(Box::new(MonoSynthNode::new(1)));
        g.set_output(synth).unwrap();
        let mut e = Engine::new(g, Transport::new(R, 120.0));
        e.prepare(R, max_frames).unwrap();
        let (tx, rx) = crate::primitives::realtime::spsc::channel::<TimestampedMessage>(16);
        e.set_midi_input(Some(rx));
        (e, tx)
    }

    fn note_on(us: u64) -> TimestampedMessage {
        let ch = MidiChannel::new(0).unwrap();
        TimestampedMessage {
            timestamp_us: us,
            message: MidiMessage::NoteOn {
                channel: ch,
                note: 69,
                velocity: 127,
            },
        }
    }

    fn note_off(us: u64) -> TimestampedMessage {
        let ch = MidiChannel::new(0).unwrap();
        TimestampedMessage {
            timestamp_us: us,
            message: MidiMessage::NoteOff {
                channel: ch,
                note: 69,
                velocity: 0,
            },
        }
    }

    /// Asserts the note started at `frame`: every sample before it is zero
    /// and the sound is audible within the next two samples (the synth's
    /// sine may be at phase 0 exactly on the note-on frame).
    fn assert_onset(out: &PlanarBuffer, frame: usize) {
        let s = out.channel(0);
        assert!(
            s[..frame].iter().all(|&x| x == 0.0),
            "expected silence before frame {frame}"
        );
        assert!(
            s[frame..frame + 2].iter().any(|&x| x != 0.0),
            "expected sound at frame {frame}"
        );
    }

    #[test]
    fn stream_timing_places_queued_midi_at_its_frame() {
        // 480 frames at 48 kHz = exactly 10 ms per block.
        let (mut e, mut tx) = synth_engine(480);
        assert_eq!(e.midi_timing(), MidiTiming::Stream);
        assert_eq!(e.stream_clock_us(), 0);
        let mut out = PlanarBuffer::new(1, 480);

        // 5 ms into the first block -> frame 240.
        tx.push(note_on(5_000)).unwrap();
        e.render(&mut out).unwrap();
        assert!(
            out.channel(0)[..240].iter().all(|&x| x == 0.0),
            "silent before the note-on frame"
        );
        assert!(
            out.channel(0)[240..].iter().any(|&x| x.abs() > 0.5),
            "sounding from frame 240"
        );
        assert_eq!(e.frames_rendered(), 480);
        assert_eq!(e.stream_clock_us(), 10_000);

        // Second block starts at 10 ms on the stream clock; a note-off 2.5 ms
        // in lands at frame 120.
        tx.push(note_off(12_500)).unwrap();
        e.render(&mut out).unwrap();
        assert!(out.channel(0)[..120].iter().any(|&x| x != 0.0));
        assert!(out.channel(0)[120..].iter().all(|&x| x == 0.0));

        // Timestamps outside the block clamp: before -> frame 0, after -> last frame.
        tx.push(note_on(0)).unwrap();
        e.render(&mut out).unwrap();
        assert_onset(&out, 0);
        tx.push(note_off(u64::MAX)).unwrap();
        e.render(&mut out).unwrap();
        assert!(out.channel(0)[..479].iter().all(|&x| x != 0.0));
        assert_eq!(out.channel(0)[479], 0.0);

        // prepare restarts the stream clock.
        e.prepare(R, 480).unwrap();
        assert_eq!(e.frames_rendered(), 0);
        assert_eq!(e.stream_clock_us(), 0);
    }

    #[test]
    fn block_start_timing_forces_frame_zero() {
        let (mut e, mut tx) = synth_engine(480);
        e.set_midi_timing(MidiTiming::BlockStart);
        assert_eq!(e.midi_timing(), MidiTiming::BlockStart);
        let mut out = PlanarBuffer::new(1, 480);
        tx.push(note_on(5_000)).unwrap();
        e.render(&mut out).unwrap();
        assert_onset(&out, 0); // mid-block timestamp still lands at frame 0
                               // Stream clock still advances so switching modes later is consistent.
        assert_eq!(e.frames_rendered(), 480);
    }

    #[test]
    fn block_start_override_is_one_shot() {
        let (mut e, mut tx) = synth_engine(480);
        let mut out = PlanarBuffer::new(1, 480);

        // A device clock says this block starts at 1 s; a message 5 ms later
        // must land at frame 240 even though the stream clock is at 0.
        e.set_midi_block_start_us(Some(1_000_000));
        tx.push(note_on(1_005_000)).unwrap();
        e.render(&mut out).unwrap();
        assert_onset(&out, 240);

        // The override was consumed: the next block is back on the stream
        // clock (10 ms), so the same device timestamp clamps to the end.
        tx.push(note_off(1_005_000)).unwrap();
        e.render(&mut out).unwrap();
        assert!(out.channel(0)[..479].iter().any(|&x| x != 0.0));
        assert_eq!(out.channel(0)[479], 0.0);

        // Setting then cancelling leaves the stream clock in charge.
        e.set_midi_block_start_us(Some(5_000_000));
        e.set_midi_block_start_us(None);
        tx.push(note_on(20_000 + 2_500)).unwrap(); // block 3 starts at 20 ms
        e.render(&mut out).unwrap();
        assert_onset(&out, 120);
    }

    #[test]
    fn frames_to_us_inverts_us_to_frames_on_sample_boundaries() {
        for &frames in &[0u64, 1, 479, 480, 48_000, 1 << 40] {
            let us = frames_to_us(frames, 48_000);
            assert_eq!(
                crate::primitives::midi::timing::us_to_frames(us, R),
                frames,
                "frames {frames}"
            );
        }
        assert_eq!(frames_to_us(480, 48_000), 10_000);
        assert_eq!(frames_to_us(44_100, 44_100), 1_000_000);
    }

    #[test]
    fn into_parts_returns_components() {
        let e = constant_engine(1.0, 1);
        let (g, t) = e.into_parts();
        assert_eq!(g.node_count(), 1);
        assert_eq!(t.sample_rate(), R);
    }

    #[test]
    fn live_input_reaches_the_nodes_and_stays_whole_across_a_loop_wrap() {
        use crate::primitives::graph::nodes::LiveInputNode;
        use crate::primitives::io::capture::capture_queue;
        let mut g = AudioGraph::new();
        let tap = g.add_node(Box::new(LiveInputNode::new([0, 1])));
        g.set_output(tap).unwrap();
        let mut engine = Engine::new(g, Transport::new(R, 120.0));
        engine.prepare(R, 64).unwrap();
        let (mut mic, blocks) = capture_queue(2, 64, 8).unwrap();
        assert!(engine
            .set_live_input(Some(LiveInput::new(blocks, 2, 64).unwrap()))
            .is_none());
        let mut out = PlanarBuffer::new(2, 64);
        engine.render(&mut out).unwrap();
        assert!(out.is_silent(), "nothing captured yet");

        let mut block = PlanarBuffer::new(2, 64);
        block.channel_mut(0).fill(0.25);
        block.channel_mut(1).fill(0.5);
        for i in 0..2 {
            assert!(mic.push(&block, i));
        }
        engine.render(&mut out).unwrap();
        assert!(out.channel(0).iter().all(|&s| s == 0.25));
        assert!(out.channel(1).iter().all(|&s| s == 0.5));

        // A block that wraps the loop is rendered in two segments; the
        // input is fed to each in turn and stays whole.
        let t = engine.transport_mut();
        t.set_loop(Some(
            LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(100)).unwrap(),
        ));
        t.set_loop_enabled(true);
        t.seek(TimePosition::from_samples(80));
        t.play();
        let segments = engine.stats().segments;
        engine.render(&mut out).unwrap();
        assert_eq!(engine.stats().segments, segments + 2);
        assert!(out.channel(0).iter().all(|&s| s == 0.25));
        assert!(out.channel(1).iter().all(|&s| s == 0.5));

        // The ring is empty now: silence, counted as an underrun.
        engine.render(&mut out).unwrap();
        assert!(out.is_silent());
        assert_eq!(engine.live_input().unwrap().underruns(), 64);
        assert!(engine.set_live_input(None).is_some());
        assert!(engine.live_input().is_none());
    }
}
