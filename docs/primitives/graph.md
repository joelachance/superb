# primitives::graph

The audio process graph of the Superb DAW SDK.

## Purpose

`primitives::graph` is the middle of the engine: it depends on the
`primitives` root, `primitives::buffers`, `primitives::realtime` and
`primitives::midi`, and pulls `primitives::transport` in for
`BlockTime::from_transport`. It defines the `Node` trait every processor
implements and `AudioGraph`, which owns nodes and edges, compiles a
topological schedule, allocates all buffers up front in `prepare`, and then
runs `process` with no heap activity. `primitives::mixer` builds its console
on this layer and the block loop in `primitives::io` drives it.

## Key types

* `Node` — the processor trait: `channels()` (fixed for the node's
  lifetime), `prepare` (off-thread, may allocate), `reset`, `process`
  (audio thread, must not allocate), `latency_frames` (read after `prepare`
  for latency compensation, see below).
* `NodeChannels` — a node's single input/output bus channel counts
  (`symmetric` or `generator`).
* `ProcessStatus` — `Normal` or `Silent`, returned by `process` so the graph
  can skip mixing a silent producer into its consumers.
* `AudioGraph` — the container: `add_node`/`remove_node`/`connect`/
  `disconnect`/`set_output` (editing, off the audio thread), `prepare`
  (compiles + allocates), `process` (audio thread).
* `Edge` / `GraphStats` — a producer→consumer connection, and per-`process`
  counters (`nodes_processed`, `nodes_silent`, `mixes`, `delayed_mixes`).
* Latency: `set_latency_compensation`/`latency_compensation`,
  `latency_frames` (graph total), `node_input_latency`/`node_output_latency`,
  `edge_delay` (see below).
* `MidiPort` / `MidiEdge` — a MIDI source (`MidiPort::External`, the buffer
  passed to `process`) routed to one node; edited with
  `connect_midi`/`disconnect_midi`/`midi_edges` (see below).
* `ProcessContext` / `BlockTime` — what a node sees each block: sample rate,
  frame count, timeline position/tempo/rolling/wrapped state, the block's
  `MidiEventBuffer`, and `input_is_silent`.
* `nodes` module — fixtures: `SineNode`, `GainNode` (click-free ramped gain),
  `PassThroughNode`, `ConstantNode`, `SilenceNode`, `MonoSynthNode`,
  `DelayNode` (fixed delay that reports itself as latency).

## Usage

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::{AudioGraph, BlockTime};
use superb::primitives::graph::nodes::{GainNode, SineNode};
use superb::primitives::midi::MidiEventBuffer;

let mut graph = AudioGraph::new();
let sine = graph.add_node(Box::new(SineNode::new(440.0, 0.5, 1)));
let gain = graph.add_node(Box::new(GainNode::new(0.5, 1)));
graph.connect(sine, gain).unwrap();
graph.set_output(gain).unwrap();
graph.prepare(SampleRate::HZ_48K, 256).unwrap();

let midi = MidiEventBuffer::with_capacity(0);
graph.process(BlockTime::free_running(), 256, &midi).unwrap();
let out = graph.output();
assert_eq!(out.channels(), 1);
assert!(out.peak() <= 0.25 + 1e-6);
```

Editing after `prepare` (`add_node`, `connect`, ...) sets `graph.is_dirty()`;
`process` then returns `Err(SuperbError::NotPrepared)` until `prepare` runs
again.

## MIDI edges: per-node MIDI routing

Audio edges say where samples flow; **MIDI edges** say which nodes see the
`MidiEventBuffer` handed to `process`. They are a separate edge set
(`MidiEdge { from: MidiPort, to: NodeId }`) that never participates in the
audio schedule or cycle detection.

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::{MonoSynthNode, PassThroughNode};
use superb::primitives::graph::{AudioGraph, BlockTime, MidiEdge, MidiPort};
use superb::primitives::midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage};

let mut graph = AudioGraph::new();
let lead = graph.add_node(Box::new(MonoSynthNode::new(1)));
let pad = graph.add_node(Box::new(MonoSynthNode::new(1)));
let sink = graph.add_node(Box::new(PassThroughNode::new(1)));
graph.connect(lead, sink).unwrap();
graph.connect(pad, sink).unwrap();
graph.set_output(sink).unwrap();

// Only `lead` hears external MIDI.
graph.connect_midi(MidiPort::External, lead).unwrap();
assert_eq!(graph.midi_edges(), &[MidiEdge { from: MidiPort::External, to: lead }]);
graph.prepare(SampleRate::HZ_48K, 64).unwrap();

let ch = MidiChannel::new(0).unwrap();
let mut midi = MidiEventBuffer::with_capacity(4);
midi.push(MidiEvent { frame: 0, message: MidiMessage::NoteOn { channel: ch, note: 69, velocity: 127 } }).unwrap();
graph.process(BlockTime::free_running(), 64, &midi).unwrap();
assert!(graph.node_output(lead).unwrap().peak() > 0.9);
assert!(graph.node_output(pad).unwrap().is_silent());
```

Rules:

* **Broadcast when empty.** With *no* MIDI edges every node receives the
  incoming buffer, so a graph that never routes MIDI needs no edges at all.
  Adding the first MIDI edge switches the graph to routed mode;
  removing the last one switches back.
* **Routed mode.** Nodes targeted by a `MidiPort::External` edge get the
  incoming buffer; every other node gets a pre-allocated **empty** buffer
  (`AudioGraph::empty_midi`, capacity 0, created in `new`). There is no
  per-node copy or channel filtering in v1: the choice is a pointer select.
* **Compiled, not scanned.** `prepare` builds a per-slot
  `receives_external_midi: Vec<bool>` table so `process` does O(1) work per
  node and never touches the edge list on the audio thread. Like audio
  edits, `connect_midi`/`disconnect_midi` mark the graph dirty and require
  `prepare` (or, with `LiveGraph`, a fresh `publish`).
* **Errors.** `connect_midi` fails with `NotFound` for an unknown node and
  `InvalidArgument` for a duplicate edge; `disconnect_midi` fails with
  `NotFound` if the edge does not exist. `remove_node` drops the MIDI edges
  that target the removed node.
* `MidiPort` is `#[non_exhaustive]`: a node-to-node port (MIDI effects,
  arpeggiators feeding synths) can be added later without breaking callers.
  `Mixer` exposes the graph through `graph_mut()`, so
  `mixer.graph_mut().connect_midi(...)` is the mixer-level API for now.

See `examples/midi_route` for MIDI edges plus sample-accurate placement of a
queued note through `superb::primitives::io::Engine`.

## Live editing: `live_pair`, `GraphEditor`, `LiveGraph`

`AudioGraph` alone forces a stop/`prepare`/start cycle for every structural
edit because `process` refuses to run a dirty graph. The `live` module lifts
that restriction without locks: the control thread edits a *draft* graph and
publishes whole prepared graphs; the audio thread swaps them in between
blocks.

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::{GainNode, SineNode};
use superb::primitives::graph::{live_pair, BlockTime};
use superb::primitives::midi::MidiEventBuffer;

let (mut editor, mut live) = live_pair();      // SPSC-connected pair
editor.configure(SampleRate::HZ_48K, 256);      // config every publish is prepared with

let s = editor.graph_mut().add_node(Box::new(SineNode::new(440.0, 1.0, 1)));
let g = editor.graph_mut().add_node(Box::new(GainNode::new(0.5, 1)));
editor.graph_mut().connect(s, g).unwrap();
editor.graph_mut().set_output(g).unwrap();
editor.publish().unwrap();                      // prepare + enqueue, draft becomes empty

let midi = MidiEventBuffer::with_capacity(0);
live.process(BlockTime::free_running(), 256, &midi).unwrap(); // applies the swap, then renders
assert_eq!(live.swap_count(), 1);

// Keep processing while building the next generation on the draft ...
let c = editor.graph_mut().add_node(Box::new(superb::primitives::graph::nodes::ConstantNode::new(0.25, 1)));
editor.graph_mut().set_output(c).unwrap();
editor.publish().unwrap();
live.process(BlockTime::free_running(), 256, &midi).unwrap();
assert_eq!(live.output().channel(0)[0], 0.25);
```

Protocol and ownership:

* Exactly one active `Box<AudioGraph>` lives inside `LiveGraph`; the editor
  owns the draft. Two `superb::primitives::realtime::spsc` queues connect them: a **swap queue**
  (`GraphCommand::Swap(Box<AudioGraph>)`, editor → live, capacity 1 by
  default, `live_pair_with_capacity(n)` for more) and a **recycle queue**
  (live → editor, capacity `n + 1`).
* **Dirty vs. published.** Edits on the draft (`editor.graph_mut()`) mark it
  dirty as usual. `publish()` reclaims retired graphs, calls
  `AudioGraph::prepare` on the draft with the configured sample rate / block
  size, moves the prepared graph into the swap queue and resets the draft to
  `AudioGraph::new()`. The *active* graph is never dirty: only prepared graphs
  cross the boundary. Editing `live.graph_mut()` structurally would dirty it
  and make `process` fail, so don't.
* **Errors.** `publish` fails with `InvalidArgument` if `configure` was not
  called, `CapacityExceeded` if the previous publish has not been applied yet
  (`editor.is_busy()`), or whatever `prepare` returns (e.g. `Cycle`). In every
  case the draft is left intact for a retry.
* **Audio thread.** `LiveGraph::process` first calls `apply_pending`: pop a
  `Swap`, `mem::swap` it with the active graph, push the retired graph to the
  recycle queue. Pointer moves only, no allocation, proven by
  `tests/live_edit.rs` both with and without a pending swap. If several swaps
  are queued the newest ends up active and the intermediates are recycled.
* **Nothing is dropped on the audio thread.** Retired graphs go back to the
  editor and are dropped by `publish()` or `try_reclaim()`; `take_retired()`
  hands one back instead so nodes can be pulled out with `remove_node` and
  reused. If the recycle queue is ever full, the retired graph is parked in
  an `orphan` slot and no further swaps are applied until it has been handed
  back.
* **Configuration must match.** A published graph prepared with a different
  `(SampleRate, max_frames)` than the live side is *rejected*: it is recycled
  without being activated and counted in `rejected_count()` on both halves.
  `LiveGraph::prepare(sr, frames)` (also `BlockProcessor::prepare`, so an
  `Engine` can own a `LiveGraph`) applies whatever is pending first, prepares
  the active graph with the new configuration, and fixes that configuration
  for future swaps; the first accepted swap into an unprepared `LiveGraph`
  adopts the incoming configuration.
* Shared `AtomicU64` counters let the editor observe progress without a
  channel: `published_count()`, `applied_count()`, `rejected_count()`,
  `pending_count()`.

See `examples/live_graph_edit` for three generations swapped into a running
block loop with printed peaks.

## Latency compensation

Every `Node` reports `latency_frames()` (a look-ahead limiter, a linear-phase
EQ, a plugin). With compensation on — the default, toggled by
`set_latency_compensation` — `prepare` runs a latency pass *after* preparing
the nodes (so a node may only know its latency once prepared):

1. Walking the schedule, each node's **input latency** is the largest output
   latency among its producers, and its **output latency** is that plus its
   own `latency_frames()`.
2. Every edge whose producer's output latency is smaller than the consumer's
   input latency gets a delay line of the difference, allocated right there
   (`delay + max_frames` frames per producer channel plus a scratch block).
3. `latency_frames()` is the output node's output latency — the longest path
   — and, with compensation on, *every* path to the output has exactly that
   latency, so parallel paths stay phase-aligned.

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::{DelayNode, PassThroughNode, SineNode};
use superb::primitives::graph::AudioGraph;

let mut g = AudioGraph::new();
let src = g.add_node(Box::new(SineNode::new(440.0, 0.5, 1)));
let limiter = g.add_node(Box::new(DelayNode::new(64, 1))); // 64-frame look-ahead
let sum = g.add_node(Box::new(PassThroughNode::new(1)));
g.connect(src, limiter).unwrap();
g.connect(limiter, sum).unwrap();
g.connect(src, sum).unwrap(); // dry path in parallel
g.set_output(sum).unwrap();
g.prepare(SampleRate::HZ_48K, 256).unwrap();
assert_eq!(g.latency_frames(), 64);
assert_eq!(g.edge_delay(src, sum).unwrap(), 64); // dry path delayed to match
assert_eq!(g.edge_delay(limiter, sum).unwrap(), 0);
```

In `process` a delayed edge always advances its delay line (even while the
producer is silent, so history stays aligned with the timeline), reads the
delayed block into the edge's scratch buffer and mixes that with the usual
channel rule. Silence propagation still works: an edge is skipped once only
silence has been pushed for `delay + frames` frames (`GraphStats::delayed_mixes`
counts the ones that were mixed). `reset()` clears the delay lines too.
`BlockProcessor::latency_frames` exposes the total through `LiveGraph` and
`Mixer` so an engine can report or pre-roll it.

## Real-time notes

`AudioGraph::add_node`/`remove_node`/`connect`/`disconnect`/`prepare` all
call `debug_assert_not_rt` and allocate (`Vec<PlanarBuffer>` per node,
`HashMap`/`Vec` bookkeeping) — they are the off-thread editing/compile path.
`AudioGraph::process` performs **no heap activity**: this is proven directly
by `tests/graph_no_alloc.rs::process_is_allocation_free`, which
runs a `SineNode`+`MonoSynthNode`+`GainNode`+`PassThroughNode` graph for 64
varying-size blocks inside `superb::primitives::realtime::assert_no_alloc` with
`CountingAllocator` installed. Per-node `process` implementations reuse the
pre-allocated input/output `PlanarBuffer`s (`input.set_frames`/`clear`,
`output.set_frames`, `mix_into` via `add_channel_from`) rather than
allocating new ones. MIDI is delivered as a `&MidiEventBuffer` reference, not
copied; the same test includes a MIDI edge so the routed path (pointer
select between the incoming buffer and `empty_midi`) is covered by the
no-alloc proof too, and a `DelayNode` in a parallel path so a
latency-compensation delay line (ring write + delayed read + mix, and
`reset`) is exercised under the proof as well.

## Session playback nodes

* `BufferPlayerNode` — timeline-aware playback of an `Arc<PlanarBuffer>`
  (region, offset, gain, fades); silent when the transport is stopped.
* `ScheduledSynthNode` / `ScheduledNoteEvent` — monophonic sine synth
  driven by absolute-sample note on/off events (built from session MIDI
  clips via `superb::session::schedule_midi_notes`).

## Limitations / next

* **Single input bus and single output bus per node** — `NodeChannels` has
  exactly one `inputs`/`outputs` count; there is no multi-bus (e.g. separate
  sidechain input) support.
* **MIDI routing is on/off per node, external source only.** `MidiPort` has
  a single variant (`External`), there is no node-to-node MIDI (no MIDI
  effects), and a routed node sees the whole block buffer — no channel or
  note-range filtering and no per-node copy. Without any MIDI edge the
  buffer is still broadcast to every node.
* **Latency compensation is per edge and static.** Delays are computed at
  `prepare`; a node whose latency changes at run time (plugin oversampling
  toggled) needs a re-`prepare`/re-publish. There is no negative-latency
  ("pre-roll the sources") mode: the graph's total latency is reported for
  the application to handle. The delayed-edge silence flag is conservative
  (based on pushed silence, not buffer content).
* **Channel adaptation is a fixed rule, not configurable**: equal counts map
  1:1, mono producers fan out, mono consumers average, otherwise only the
  first `min(src, dst)` channels map (`mix_into`,
  `src/primitives/graph/graph.rs`, tested by `channel_adaptation_rules`).
* Cycles are rejected at `connect` time (`SuperbError::Cycle`), so there is
  no feedback-delay-compensated loop support.
* **Live editing swaps whole graphs.** After `publish()` the draft is empty,
  so the next generation must be rebuilt (or assembled from nodes recovered
  via `take_retired()` + `remove_node`). Node state (oscillator phase, gain
  ramps) does not carry over across a swap, and swaps land on block
  boundaries only, so a click-free crossfade between generations is the
  caller's responsibility for now.
