//! A running engine split in two: a control handle and an audio thread.
//!
//! [`Engine::render`] has to be driven from exactly one thread: a device
//! callback, or an offline loop. Play, stop, seek and "swap in this new
//! graph" come from somewhere else, usually a UI. [`Engine::live`] makes
//! the two sides:
//!
//! * [`AudioThread`] is the audio side. It owns the [`Engine`] (wrapping a
//!   [`LiveGraph`]) and exposes one method, [`AudioThread::tick`], which
//!   applies pending commands, renders a block and publishes the transport
//!   position. Hand it to a device stream with `AudioThread::run_on_device`
//!   (feature `devices`) or call `tick` from a loop of your own.
//! * [`EngineHandle`] is the control side. It sends [`EngineCommand`]s over a
//!   wait-free queue, publishes prepared graphs through a [`GraphEditor`],
//!   queues software MIDI, and reads the position and state the audio
//!   thread publishes through atomics.
//!
//! Nothing here blocks or allocates on the audio side: commands are `Copy`
//! or move a queue handle, graph swaps are pointer exchanges, and retired
//! graphs travel back to the control side to be dropped there.
//!
//! ```
//! use superb::primitives::graph::nodes::ConstantNode;
//! use superb::primitives::graph::AudioGraph;
//! use superb::primitives::io::{Engine, EngineCommand};
//! use superb::primitives::buffers::PlanarBuffer;
//! use superb::primitives::{SampleRate, TimePosition};
//!
//! let (mut control, mut audio) = Engine::live(SampleRate::HZ_48K, 64, 120.0).unwrap();
//!
//! // Control thread: build a graph and publish it.
//! let mut graph = AudioGraph::new();
//! let c = graph.add_node(Box::new(ConstantNode::new(0.5, 2)));
//! graph.set_output(c).unwrap();
//! control.publish(graph).unwrap();
//! control.send(EngineCommand::Play).unwrap();
//!
//! // Audio thread: one block.
//! let mut out = PlanarBuffer::new(2, 64);
//! audio.tick(&mut out).unwrap();
//! assert_eq!(out.channel(0)[0], 0.5);
//! assert_eq!(control.position(), TimePosition::from_samples(64));
//! ```

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::{live_pair_with_capacity, AudioGraph, GraphEditor, LiveGraph};
use crate::primitives::midi::{MidiEvent, TimestampedMessage};
use crate::primitives::realtime::spsc::{self, Consumer, Producer};
use crate::primitives::realtime::LoadMeter;
use crate::primitives::transport::{LoopRange, Transport, TransportState};
use crate::primitives::{Result, SampleRate, SuperbError, TimePosition};

use super::{Engine, LiveInput, MidiTiming};

/// How many [`EngineCommand`]s may wait between two blocks.
pub const COMMAND_CAPACITY: usize = 64;
/// How many software MIDI messages may wait between two blocks.
pub const MIDI_CAPACITY: usize = 256;
/// How many extra MIDI sources ([`EngineCommand::AddMidiInput`]) an audio thread accepts.
pub const MAX_EXTRA_MIDI_INPUTS: usize = 4;
/// How many published graphs may wait to be swapped in.
pub const SWAP_CAPACITY: usize = 2;
/// How many replaced live inputs may wait to be reclaimed.
pub const RETIRED_INPUT_CAPACITY: usize = 4;

/// A request from the control thread to the audio thread.
///
/// Applied at the start of the next [`AudioThread::tick`], in the order sent.
pub enum EngineCommand {
    /// Start the transport (recording stays recording).
    Play,
    /// Switch the transport to recording (also rolls).
    Record,
    /// Stop the transport where it is.
    Stop,
    /// Move the playhead.
    Seek(TimePosition),
    /// Set or clear the audio thread range (does not enable looping).
    SetLoop(Option<LoopRange>),
    /// Enable or disable looping.
    SetLoopEnabled(bool),
    /// Choose how queued MIDI is placed in a block.
    SetMidiTiming(MidiTiming),
    /// Adds a MIDI source (for example a hardware input bridge). Its
    /// messages land at frame 0 of the block they are drained in; the
    /// built-in software queue keeps sample-accurate placement. At most
    /// [`MAX_EXTRA_MIDI_INPUTS`] sources; extra ones are dropped.
    AddMidiInput(Consumer<TimestampedMessage>),
    /// Attaches captured audio as the engine's live input (what
    /// `ProcessContext::input` carries), or detaches it with `None`. An
    /// input attached before travels back to the control side and is freed
    /// by [`EngineHandle::reclaim`].
    LiveInput(Option<LiveInput>),
}

impl core::fmt::Debug for EngineCommand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            EngineCommand::Play => f.write_str("Play"),
            EngineCommand::Record => f.write_str("Record"),
            EngineCommand::Stop => f.write_str("Stop"),
            EngineCommand::Seek(p) => write!(f, "Seek({p})"),
            EngineCommand::SetLoop(r) => write!(f, "SetLoop({r:?})"),
            EngineCommand::SetLoopEnabled(b) => write!(f, "SetLoopEnabled({b})"),
            EngineCommand::SetMidiTiming(t) => write!(f, "SetMidiTiming({t:?})"),
            EngineCommand::AddMidiInput(_) => f.write_str("AddMidiInput(..)"),
            EngineCommand::LiveInput(input) => write!(f, "LiveInput({input:?})"),
        }
    }
}

/// What the audio thread publishes after every block.
struct Shared {
    position: AtomicI64,
    /// 0 stopped, 1 playing, 2 recording.
    state: AtomicU8,
    blocks: AtomicU64,
}

fn state_to_u8(state: TransportState) -> u8 {
    match state {
        TransportState::Stopped => 0,
        TransportState::Playing => 1,
        TransportState::Recording => 2,
    }
}

fn state_from_u8(v: u8) -> TransportState {
    match v {
        1 => TransportState::Playing,
        2 => TransportState::Recording,
        _ => TransportState::Stopped,
    }
}

/// The control side of a live engine: what a UI holds to drive an
/// [`AudioThread`].
///
/// Every method is safe to call from a UI thread. Nothing here waits for
/// the audio thread; commands and graphs are queued and applied at the next
/// block.
pub struct EngineHandle {
    commands: Producer<EngineCommand>,
    midi: Producer<TimestampedMessage>,
    retired_inputs: Consumer<LiveInput>,
    editor: GraphEditor,
    shared: Arc<Shared>,
    load: Arc<LoadMeter>,
    sample_rate: SampleRate,
    max_frames: usize,
}

impl EngineHandle {
    /// Queues a command. Fails with [`SuperbError::CapacityExceeded`] if
    /// [`COMMAND_CAPACITY`] commands are already waiting.
    pub fn send(&mut self, command: EngineCommand) -> Result<()> {
        self.commands
            .push(command)
            .map_err(|_| SuperbError::CapacityExceeded {
                what: "engine command queue",
                capacity: COMMAND_CAPACITY,
            })
    }

    /// Prepares `graph` for this engine's sample rate and block size and
    /// hands it to the audio thread, which swaps it in at its next block. Retired
    /// graphs that have come back are dropped here first.
    ///
    /// Fails if `graph` cannot be prepared (for example a cycle) or if
    /// [`SWAP_CAPACITY`] graphs are still waiting to be applied.
    pub fn publish(&mut self, graph: AudioGraph) -> Result<()> {
        let _ = self.editor.replace_draft(graph);
        self.editor.publish()
    }

    /// The editor behind [`publish`](Self::publish), for callers that want
    /// to build the next graph in its draft instead of passing one in.
    pub fn editor(&mut self) -> &mut GraphEditor {
        &mut self.editor
    }

    /// Drops retired graphs and replaced live inputs the audio thread has
    /// handed back. Returns how many.
    pub fn reclaim(&mut self) -> usize {
        let mut n = self.editor.try_reclaim();
        while self.retired_inputs.pop().is_some() {
            n += 1;
        }
        n
    }

    /// Queues a MIDI message for the engine's built-in software input.
    /// Stamp `timestamp_us` on the engine's stream clock for sample-accurate
    /// placement, or use `0` for "the start of the next block".
    pub fn push_midi(&mut self, message: TimestampedMessage) -> Result<()> {
        self.midi
            .push(message)
            .map_err(|_| SuperbError::CapacityExceeded {
                what: "engine midi queue",
                capacity: MIDI_CAPACITY,
            })
    }

    /// Playhead position after the last block the audio thread rendered.
    pub fn position(&self) -> TimePosition {
        TimePosition::from_samples(self.shared.position.load(Ordering::Acquire))
    }

    /// Transport state after the last block the audio thread rendered.
    pub fn state(&self) -> TransportState {
        state_from_u8(self.shared.state.load(Ordering::Acquire))
    }

    /// `true` if the transport was playing or recording at the last block.
    pub fn is_rolling(&self) -> bool {
        self.state().is_rolling()
    }

    /// Blocks the audio thread has rendered so far.
    pub fn blocks_rendered(&self) -> u64 {
        self.shared.blocks.load(Ordering::Acquire)
    }

    /// DSP load meter shared with the audio thread.
    pub fn load(&self) -> Arc<LoadMeter> {
        Arc::clone(&self.load)
    }

    /// Sample rate every published graph is prepared for.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// Largest block the audio thread will render, and the size graphs are prepared for.
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// `true` if the audio thread has been dropped.
    pub fn is_detached(&self) -> bool {
        self.commands.is_abandoned()
    }
}

impl core::fmt::Debug for EngineHandle {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EngineHandle")
            .field("position", &self.position())
            .field("state", &self.state())
            .field("blocks", &self.blocks_rendered())
            .field("sample_rate", &self.sample_rate)
            .field("max_frames", &self.max_frames)
            .finish()
    }
}

/// The audio side of a live engine: owns the [`Engine`] and renders blocks
/// on demand, from a device callback or a loop of your own.
pub struct AudioThread {
    engine: Engine<LiveGraph>,
    commands: Consumer<EngineCommand>,
    extra_midi: [Option<Consumer<TimestampedMessage>>; MAX_EXTRA_MIDI_INPUTS],
    retired_inputs: Producer<LiveInput>,
    /// A replaced input the retire queue had no room for; retried each tick.
    retired_overflow: Option<LiveInput>,
    shared: Arc<Shared>,
}

impl AudioThread {
    /// Applies every waiting command to the transport and engine. Called by
    /// [`tick`](Self::tick); call it directly only if you drive
    /// [`Engine::render`] yourself. Allocation-free.
    pub fn apply_commands(&mut self) {
        if let Some(old) = self.retired_overflow.take() {
            if let Err(old) = self.retired_inputs.push(old) {
                self.retired_overflow = Some(old);
            }
        }
        while let Some(command) = self.commands.pop() {
            match command {
                EngineCommand::Play => self.engine.transport_mut().play(),
                EngineCommand::Record => self.engine.transport_mut().record(),
                EngineCommand::Stop => self.engine.transport_mut().stop(),
                EngineCommand::Seek(position) => self.engine.transport_mut().seek(position),
                EngineCommand::SetLoop(range) => self.engine.transport_mut().set_loop(range),
                EngineCommand::SetLoopEnabled(on) => {
                    self.engine.transport_mut().set_loop_enabled(on)
                }
                EngineCommand::SetMidiTiming(timing) => self.engine.set_midi_timing(timing),
                EngineCommand::AddMidiInput(consumer) => {
                    if let Some(slot) = self.extra_midi.iter_mut().find(|s| s.is_none()) {
                        *slot = Some(consumer);
                    }
                    // A dropped consumer is freed here, on this thread; the
                    // producer side notices through `is_abandoned`.
                }
                EngineCommand::LiveInput(input) => {
                    if let Some(old) = self.engine.set_live_input(input) {
                        // Never freed here: it goes back to the control thread.
                        if let Err(old) = self.retired_inputs.push(old) {
                            self.retired_overflow = Some(old);
                        }
                    }
                }
            }
        }
    }

    /// Applies pending commands, drains extra MIDI sources, renders one
    /// block into `out` and publishes the new position. Allocation-free.
    ///
    /// `out` may have any channel count; the graph's output is adapted.
    pub fn tick(&mut self, out: &mut PlanarBuffer) -> Result<()> {
        self.apply_commands();
        for source in self.extra_midi.iter_mut().flatten() {
            while let Some(msg) = source.pop() {
                let _ = self.engine.midi_mut().push(MidiEvent {
                    frame: 0,
                    message: msg.message,
                });
            }
        }
        let result = self.engine.render(out);
        let transport = self.engine.transport();
        self.shared
            .position
            .store(transport.position().samples(), Ordering::Release);
        self.shared
            .state
            .store(state_to_u8(transport.state()), Ordering::Release);
        self.shared.blocks.fetch_add(1, Ordering::Release);
        result
    }

    /// The engine, for inspection (stats, transport) between ticks.
    pub fn engine(&self) -> &Engine<LiveGraph> {
        &self.engine
    }

    /// Mutable engine access, for wiring that has to happen on this side.
    pub fn engine_mut(&mut self) -> &mut Engine<LiveGraph> {
        &mut self.engine
    }

    /// `true` if the control handle has been dropped.
    pub fn is_detached(&self) -> bool {
        self.commands.is_abandoned()
    }

    /// Moves the audio thread into a device output stream and starts it.
    ///
    /// `config.sample_rate` and `config.max_block_frames` must equal the
    /// values the audio thread was created with, because every published graph is
    /// prepared for them; a mismatch is rejected rather than silently
    /// re-prepared. The callback sets the MIDI block start from the device
    /// clock, then calls [`tick`](Self::tick).
    #[cfg(feature = "devices")]
    pub fn run_on_device(
        mut self,
        config: &crate::primitives::io::device::OutputConfig,
    ) -> Result<crate::primitives::io::device::OutputStream> {
        let (rate, frames) = self
            .engine
            .prepared_config()
            .ok_or(SuperbError::NotPrepared)?;
        if config.sample_rate != rate || config.max_block_frames != frames {
            return Err(SuperbError::InvalidArgument {
                what: "output config must match the live engine's sample rate and block size",
            });
        }
        let load = self.engine.load();
        crate::primitives::io::device::OutputStream::open_with_meter(
            config,
            move |block: &mut PlanarBuffer, block_start_us: u64| {
                self.engine.set_midi_block_start_us(Some(block_start_us));
                if self.tick(block).is_err() {
                    block.clear();
                }
            },
            load,
        )
    }
}

impl core::fmt::Debug for AudioThread {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AudioThread")
            .field("transport", &self.engine.transport().state())
            .field("position", &self.engine.transport().position())
            .field("extra_midi", &self.extra_midi.iter().flatten().count())
            .field("live_input", &self.engine.live_input().is_some())
            .finish()
    }
}

impl Engine<LiveGraph> {
    /// Creates a live engine: a connected control handle and audio thread.
    ///
    /// The audio thread starts with an empty, prepared graph at `sample_rate`
    /// and `max_frames`, a stopped transport at `bpm`, and the built-in
    /// software MIDI queue attached. Publish a graph with
    /// [`EngineHandle::publish`], then drive [`AudioThread::tick`].
    pub fn live(
        sample_rate: SampleRate,
        max_frames: usize,
        bpm: f64,
    ) -> Result<(EngineHandle, AudioThread)> {
        pair(sample_rate, max_frames, bpm)
    }
}

fn pair(
    sample_rate: SampleRate,
    max_frames: usize,
    bpm: f64,
) -> Result<(EngineHandle, AudioThread)> {
    let (mut editor, live) = live_pair_with_capacity(SWAP_CAPACITY);
    editor.configure(sample_rate, max_frames);
    let mut engine = Engine::new(live, Transport::new(sample_rate, bpm));
    engine.prepare(sample_rate, max_frames)?;
    let (midi_tx, midi_rx) = spsc::channel::<TimestampedMessage>(MIDI_CAPACITY);
    engine.set_midi_input(Some(midi_rx));
    let (cmd_tx, cmd_rx) = spsc::channel::<EngineCommand>(COMMAND_CAPACITY);
    let (retired_tx, retired_rx) = spsc::channel::<LiveInput>(RETIRED_INPUT_CAPACITY);
    let shared = Arc::new(Shared {
        position: AtomicI64::new(0),
        state: AtomicU8::new(0),
        blocks: AtomicU64::new(0),
    });
    let load = engine.load();
    Ok((
        EngineHandle {
            commands: cmd_tx,
            midi: midi_tx,
            retired_inputs: retired_rx,
            editor,
            shared: Arc::clone(&shared),
            load,
            sample_rate,
            max_frames,
        },
        AudioThread {
            engine,
            commands: cmd_rx,
            extra_midi: [None, None, None, None],
            retired_inputs: retired_tx,
            retired_overflow: None,
            shared,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::nodes::{ConstantNode, MonoSynthNode};
    use crate::primitives::midi::{MidiChannel, MidiMessage};

    const R: SampleRate = SampleRate::HZ_48K;

    fn constant_graph(value: f32) -> AudioGraph {
        let mut graph = AudioGraph::new();
        let c = graph.add_node(Box::new(ConstantNode::new(value, 2)));
        graph.set_output(c).unwrap();
        graph
    }

    fn note_on(us: u64) -> TimestampedMessage {
        TimestampedMessage {
            timestamp_us: us,
            message: MidiMessage::NoteOn {
                channel: MidiChannel::new(0).unwrap(),
                note: 69,
                velocity: 127,
            },
        }
    }

    #[test]
    fn empty_loop_renders_silence_and_does_not_move_while_stopped() {
        let (control, mut audio) = Engine::live(R, 32, 120.0).unwrap();
        let mut out = PlanarBuffer::new(2, 32);
        out.fill(1.0);
        audio.tick(&mut out).unwrap();
        assert!(out.is_silent(), "empty graph renders silence");
        assert_eq!(control.position(), TimePosition::ZERO);
        assert_eq!(control.state(), TransportState::Stopped);
        assert_eq!(control.blocks_rendered(), 1);
        assert_eq!(control.sample_rate(), R);
        assert_eq!(control.max_frames(), 32);
        assert!(!control.is_detached() && !audio.is_detached());
        let _ = format!("{control:?} {audio:?}");
    }

    #[test]
    fn commands_drive_the_transport_in_order() {
        let (mut control, mut audio) = Engine::live(R, 64, 120.0).unwrap();
        control.publish(constant_graph(0.25)).unwrap();
        control
            .send(EngineCommand::Seek(TimePosition::from_samples(1_000)))
            .unwrap();
        control.send(EngineCommand::Play).unwrap();
        let mut out = PlanarBuffer::new(2, 64);
        audio.tick(&mut out).unwrap();
        assert_eq!(out.channel(1)[63], 0.25);
        assert_eq!(control.position(), TimePosition::from_samples(1_064));
        assert!(control.is_rolling());

        control.send(EngineCommand::Record).unwrap();
        audio.tick(&mut out).unwrap();
        assert_eq!(control.state(), TransportState::Recording);

        control.send(EngineCommand::Stop).unwrap();
        audio.tick(&mut out).unwrap();
        assert_eq!(control.state(), TransportState::Stopped);
        assert_eq!(control.position(), TimePosition::from_samples(1_128));
        audio.tick(&mut out).unwrap();
        assert_eq!(
            control.position(),
            TimePosition::from_samples(1_128),
            "stopped: does not move"
        );
    }

    #[test]
    fn loop_range_wraps_the_playhead() {
        let (mut control, mut audio) = Engine::live(R, 100, 120.0).unwrap();
        control.publish(constant_graph(0.1)).unwrap();
        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(150)).unwrap();
        control.send(EngineCommand::SetLoop(Some(range))).unwrap();
        control.send(EngineCommand::SetLoopEnabled(true)).unwrap();
        control.send(EngineCommand::Play).unwrap();
        let mut out = PlanarBuffer::new(2, 100);
        audio.tick(&mut out).unwrap();
        audio.tick(&mut out).unwrap();
        assert_eq!(control.position(), TimePosition::from_samples(50));
        assert_eq!(audio.engine().stats().segments, 3);
    }

    #[test]
    fn published_graphs_swap_between_blocks_and_retire_to_the_control_side() {
        let (mut control, mut audio) = Engine::live(R, 16, 120.0).unwrap();
        control.publish(constant_graph(0.2)).unwrap();
        let mut out = PlanarBuffer::new(1, 16);
        audio.tick(&mut out).unwrap();
        assert!((out.channel(0)[0] - 0.2).abs() < 1e-6);
        control.publish(constant_graph(0.7)).unwrap();
        audio.tick(&mut out).unwrap();
        assert!((out.channel(0)[0] - 0.7).abs() < 1e-6);
        // The empty initial graph was reclaimed by the second publish; the
        // 0.2 graph is waiting now.
        assert_eq!(control.reclaim(), 1);
        // The editor's draft is the alternative way to build the next graph.
        let c = control
            .editor()
            .graph_mut()
            .add_node(Box::new(ConstantNode::new(0.9, 2)));
        control.editor().graph_mut().set_output(c).unwrap();
        control.editor().publish().unwrap();
        audio.tick(&mut out).unwrap();
        assert!((out.channel(0)[0] - 0.9).abs() < 1e-6);
        assert_eq!(audio.engine().processor().swap_count(), 3);
    }

    #[test]
    fn software_midi_reaches_the_graph_sample_accurately() {
        let (mut control, mut audio) = Engine::live(R, 480, 120.0).unwrap();
        let mut graph = AudioGraph::new();
        let synth = graph.add_node(Box::new(MonoSynthNode::new(1)));
        graph.set_output(synth).unwrap();
        control.publish(graph).unwrap();
        control.send(EngineCommand::Play).unwrap();
        // Block 0 spans 0..10_000 µs on the stream clock; 5_000 µs is frame 240.
        control.push_midi(note_on(5_000)).unwrap();
        let mut out = PlanarBuffer::new(1, 480);
        audio.tick(&mut out).unwrap();
        assert!(out.channel(0)[..240].iter().all(|&s| s == 0.0));
        assert!(out.channel(0)[240..].iter().any(|&s| s != 0.0));
    }

    #[test]
    fn extra_midi_inputs_land_at_the_block_start() {
        let (mut control, mut audio) = Engine::live(R, 64, 120.0).unwrap();
        let mut graph = AudioGraph::new();
        let synth = graph.add_node(Box::new(MonoSynthNode::new(1)));
        graph.set_output(synth).unwrap();
        control.publish(graph).unwrap();
        let (mut hardware, rx) = spsc::channel::<TimestampedMessage>(8);
        control.send(EngineCommand::AddMidiInput(rx)).unwrap();
        hardware.push(note_on(123_456_789)).unwrap(); // some unrelated clock
        let mut out = PlanarBuffer::new(1, 64);
        audio.tick(&mut out).unwrap();
        assert!(
            out.channel(0)[..2].iter().any(|&s| s != 0.0),
            "sounds from frame 0"
        );
        // Only four extra inputs are kept; the fifth is dropped without error.
        for _ in 0..5 {
            let (_tx, rx) = spsc::channel::<TimestampedMessage>(1);
            control.send(EngineCommand::AddMidiInput(rx)).unwrap();
        }
        audio.tick(&mut out).unwrap();
        assert_eq!(
            audio.extra_midi.iter().flatten().count(),
            MAX_EXTRA_MIDI_INPUTS
        );
    }

    #[test]
    fn full_queues_report_capacity_errors() {
        let (mut control, _audio) = Engine::live(R, 64, 120.0).unwrap();
        for _ in 0..COMMAND_CAPACITY {
            control.send(EngineCommand::Play).unwrap();
        }
        assert!(matches!(
            control.send(EngineCommand::Stop),
            Err(SuperbError::CapacityExceeded { .. })
        ));
        for _ in 0..MIDI_CAPACITY {
            control.push_midi(note_on(0)).unwrap();
        }
        assert!(matches!(
            control.push_midi(note_on(0)),
            Err(SuperbError::CapacityExceeded { .. })
        ));
        // Two publishes may wait; a third is refused until the audio thread applies them.
        control.publish(constant_graph(0.1)).unwrap();
        control.publish(constant_graph(0.2)).unwrap();
        assert!(matches!(
            control.publish(constant_graph(0.3)),
            Err(SuperbError::CapacityExceeded { .. })
        ));
    }

    #[test]
    fn dropping_one_side_is_visible_to_the_other() {
        let (control, audio) = Engine::live(R, 64, 120.0).unwrap();
        assert!(!audio.is_detached());
        drop(control);
        assert!(audio.is_detached());
        let (control, audio) = Engine::live(R, 64, 120.0).unwrap();
        drop(audio);
        assert!(control.is_detached());
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        assert!(Engine::live(R, 0, 120.0).is_err());
    }

    #[test]
    fn a_live_input_command_attaches_and_a_replaced_one_comes_back() {
        use crate::primitives::graph::nodes::LiveInputNode;
        use crate::primitives::io::capture::capture_queue;
        let (mut control, mut audio) = Engine::live(R, 64, 120.0).unwrap();
        let mut graph = AudioGraph::new();
        let tap = graph.add_node(Box::new(LiveInputNode::new([0, 0])));
        graph.set_output(tap).unwrap();
        control.publish(graph).unwrap();
        let (mut mic, blocks) = capture_queue(1, 64, 4).unwrap();
        control
            .send(EngineCommand::LiveInput(Some(
                LiveInput::new(blocks, 1, 64).unwrap(),
            )))
            .unwrap();
        let mut block = PlanarBuffer::new(1, 64);
        block.fill(0.5);
        assert!(mic.push(&block, 0));
        let mut out = PlanarBuffer::new(2, 64);
        audio.tick(&mut out).unwrap();
        assert!(
            out.channel(1).iter().all(|&s| s == 0.5),
            "a mono input on both sides"
        );
        assert!(format!("{audio:?}").contains("live_input: true"));
        let _ = control.reclaim(); // the first graph's predecessor

        // A second input replaces the first, which travels back to be freed here.
        let (_mic2, blocks2) = capture_queue(1, 64, 4).unwrap();
        control
            .send(EngineCommand::LiveInput(Some(
                LiveInput::new(blocks2, 1, 64).unwrap(),
            )))
            .unwrap();
        audio.tick(&mut out).unwrap();
        assert!(out.is_silent(), "the new input has nothing yet");
        assert_eq!(control.reclaim(), 1, "the replaced input");

        // Detaching hands the input back the same way.
        control.send(EngineCommand::LiveInput(None)).unwrap();
        audio.tick(&mut out).unwrap();
        assert!(format!("{audio:?}").contains("live_input: false"));
        assert_eq!(control.reclaim(), 1, "the detached input");
    }
}
