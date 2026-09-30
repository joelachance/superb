//! The `Node` trait.

use superb_buffers::PlanarBuffer;
use superb_core::SampleRate;

use crate::ProcessContext;

/// Channel counts of a node's single input and output bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeChannels {
    /// Channels the node reads. `0` for pure generators.
    pub inputs: usize,
    /// Channels the node writes. `0` for pure sinks (e.g. analysers).
    pub outputs: usize,
}

impl NodeChannels {
    /// Same channel count in and out.
    pub const fn symmetric(channels: usize) -> Self {
        Self {
            inputs: channels,
            outputs: channels,
        }
    }

    /// A generator with no input.
    pub const fn generator(outputs: usize) -> Self {
        Self { inputs: 0, outputs }
    }
}

/// What a node did with its output buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessStatus {
    /// The output buffer holds audio.
    Normal,
    /// The output is silent; the graph will not read the buffer and will not
    /// mix it into consumers. The node need not have cleared it.
    Silent,
}

/// Anything that renders whole blocks: an [`AudioGraph`](crate::AudioGraph),
/// a mixer wrapping one, or a custom engine.
///
/// The device and offline layers in `superb-io` are generic over this trait.
pub trait BlockProcessor: Send {
    /// Compiles/allocates for the given configuration. Off the audio thread.
    fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> superb_core::Result<()>;

    /// Renders one block. Must be allocation-free.
    fn process(
        &mut self,
        time: crate::BlockTime,
        frames: usize,
        midi: &superb_midi::MidiEventBuffer,
    ) -> superb_core::Result<()>;

    /// The rendered output of the last `process` call.
    fn output(&self) -> &PlanarBuffer;

    /// Number of output channels once prepared.
    fn output_channels(&self) -> usize {
        self.output().channels()
    }

    /// Clears internal state without reallocating.
    fn reset(&mut self) {}

    /// Frames of delay between input and output once prepared (for example
    /// a graph's compensated path latency). Applications use it to report
    /// or pre-roll. Defaults to `0`.
    fn latency_frames(&self) -> usize {
        0
    }
}

/// A processor in the audio graph.
///
/// Contract:
/// * [`prepare`](Node::prepare) is called off the audio thread before the
///   first `process` and again if the sample rate or block size changes. It
///   may allocate.
/// * [`process`](Node::process) is called on the audio thread. It must not
///   allocate, block, or lock. `input` contains the sum of all producers
///   (silence if there are none); `output` has `ctx.frames` frames and must
///   be fully written unless [`ProcessStatus::Silent`] is returned.
/// * `input` and `output` are distinct buffers, so in-place style processing
///   is `output.copy_from(input)` followed by mutation.
pub trait Node: Send {
    /// Human-readable name for diagnostics.
    fn name(&self) -> &str {
        "node"
    }

    /// Channel configuration. Must be constant for the node's lifetime.
    fn channels(&self) -> NodeChannels;

    /// Called before processing begins. May allocate.
    fn prepare(&mut self, _sample_rate: SampleRate, _max_frames: usize) {}

    /// Clears internal state (delay lines, oscillator phase) without reallocating.
    fn reset(&mut self) {}

    /// Processes one block. Must be real-time safe.
    fn process(
        &mut self,
        ctx: &ProcessContext<'_>,
        input: &PlanarBuffer,
        output: &mut PlanarBuffer,
    ) -> ProcessStatus;

    /// Processing latency in frames. Read by [`AudioGraph`](crate::AudioGraph)
    /// after `prepare` to align parallel paths (latency compensation) and to
    /// report the graph's total latency. Must be constant between `prepare`
    /// calls.
    fn latency_frames(&self) -> usize {
        0
    }
}
