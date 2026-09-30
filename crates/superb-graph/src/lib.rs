//! # superb-graph
//!
//! The audio process graph of the Superb DAW SDK.
//!
//! * [`Node`] — the trait every processor implements: `prepare` once (may
//!   allocate), `process` per block (must not allocate).
//! * [`AudioGraph`] — owns nodes and edges, compiles a topological schedule,
//!   allocates all buffers up front in [`AudioGraph::prepare`], then runs
//!   [`AudioGraph::process`] with **no heap activity** (verified by the
//!   `no_alloc` integration test using `superb-rt`'s counting allocator).
//! * [`ProcessContext`] / [`BlockTime`] — what a node sees each block:
//!   sample rate, frame count, timeline position from the transport, tempo,
//!   and the block's MIDI events.
//! * [`MidiEdge`] / [`MidiPort`] — per-node MIDI routing: with no MIDI edges
//!   the block's buffer is broadcast to every node; once any edge exists only
//!   the targets of `MidiPort::External` edges receive it.
//! * [`nodes`] — fixture nodes: sine, gain, pass-through, constant, silence,
//!   a tiny monophonic MIDI synth, plus timeline-aware `BufferPlayerNode` and
//!   `ScheduledSynthNode` for session clip playback.
//! * [`live`] — lock-free live editing: [`GraphEditor`] owns a draft graph on
//!   the control thread, [`LiveGraph`] runs the active graph on the audio
//!   thread, and [`live_pair`] joins them with SPSC queues so a prepared graph
//!   can be swapped in between blocks without stopping the stream.
//!
//! Every node has one input bus and one output bus. Multiple edges into a
//! node are summed. Channel-count mismatches follow a fixed rule (mono fans
//! out, multi-channel folds down to mono by averaging, otherwise the common
//! channels are mapped 1:1). Silence is propagated: a node that reports
//! [`ProcessStatus::Silent`] is not mixed into its consumers.
//!
//! ```
//! use superb_core::SampleRate;
//! use superb_graph::{AudioGraph, BlockTime};
//! use superb_graph::nodes::{GainNode, SineNode};
//! use superb_midi::MidiEventBuffer;
//!
//! let mut graph = AudioGraph::new();
//! let sine = graph.add_node(Box::new(SineNode::new(440.0, 0.5, 1)));
//! let gain = graph.add_node(Box::new(GainNode::new(0.5, 1)));
//! graph.connect(sine, gain).unwrap();
//! graph.set_output(gain).unwrap();
//! graph.prepare(SampleRate::HZ_48K, 256).unwrap();
//!
//! let midi = MidiEventBuffer::with_capacity(0);
//! graph.process(BlockTime::free_running(), 256, &midi).unwrap();
//! let out = graph.output();
//! assert_eq!(out.channels(), 1);
//! assert!(out.peak() <= 0.25 + 1e-6);
//! assert!(out.peak() > 0.2);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod context;
mod delay;
mod graph;
pub mod live;
mod node;
pub mod nodes;

pub use context::{BlockTime, ProcessContext};
pub use graph::{AudioGraph, Edge, GraphStats, MidiEdge, MidiPort};
pub use live::{live_pair, live_pair_with_capacity, GraphCommand, GraphEditor, LiveGraph};
pub use node::{BlockProcessor, Node, NodeChannels, ProcessStatus};
