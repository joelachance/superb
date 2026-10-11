//! The graph container and scheduler.

use std::collections::HashMap;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::midi::MidiEventBuffer;
use crate::primitives::realtime::thread::debug_assert_not_rt;
use crate::primitives::{AuraError, IdGenerator, NodeId, Result, SampleRate};

use crate::primitives::graph::delay::DelayLine;
use crate::primitives::graph::{BlockTime, Node, ProcessContext, ProcessStatus};

/// A directed connection from one node's output bus to another's input bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Edge {
    /// Producer.
    pub from: NodeId,
    /// Consumer.
    pub to: NodeId,
}

/// A source of MIDI events that can be routed to a node.
///
/// Only [`MidiPort::External`] — the buffer passed to
/// [`AudioGraph::process`] — exists today. The enum is `non_exhaustive` so a
/// node-to-node port (`Node(NodeId)`, for MIDI effects / arpeggiators) can be
/// added without breaking callers.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MidiPort {
    /// The `MidiEventBuffer` handed to `process` by the engine (hardware
    /// input, sequencer, tests).
    External,
}

/// A MIDI connection from a port to a node's `ProcessContext::midi`.
///
/// Separate from the audio [`Edge`]: MIDI edges never take part in the audio
/// schedule or cycle detection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MidiEdge {
    /// Source of events.
    pub from: MidiPort,
    /// Node that receives them.
    pub to: NodeId,
}

/// Counters exposed after each `process` call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GraphStats {
    /// Nodes whose `process` was invoked.
    pub nodes_processed: usize,
    /// Nodes that reported [`ProcessStatus::Silent`].
    pub nodes_silent: usize,
    /// Edge mixes performed (silent producers are skipped).
    pub mixes: usize,
    /// Of those mixes, how many went through a latency-compensation delay.
    pub delayed_mixes: usize,
}

/// One incoming edge of a slot, compiled by `prepare`.
#[derive(Clone, Copy)]
struct Producer {
    /// Slot index of the producer.
    slot: usize,
    /// Index into `AudioGraph::delays` when this edge is delayed for latency
    /// compensation.
    delay: Option<usize>,
}

/// A compensation delay on one edge plus the scratch block it renders into.
struct EdgeDelay {
    line: DelayLine,
    out: PlanarBuffer,
}

struct Slot {
    id: NodeId,
    node: Box<dyn Node>,
    inputs: usize,
    outputs: usize,
    /// Incoming edges, filled in by `compile` / `compile_latency`.
    producers: Vec<Producer>,
}

/// An audio process graph.
///
/// Editing methods (`add_node`, `connect`, ...) mark the graph *dirty*;
/// [`prepare`](AudioGraph::prepare) recompiles the schedule and allocates all
/// buffers. [`process`](AudioGraph::process) fails with
/// [`AuraError::NotPrepared`] on a dirty graph rather than allocating on
/// the audio thread.
///
/// # Latency compensation
///
/// Every node reports [`Node::latency_frames`]. When compensation is on (the
/// default), `prepare` walks the schedule and computes each node's *arrival
/// latency* — the largest latency accumulated along any path into it — and
/// delays every incoming edge whose path is shorter by the difference, so
/// parallel paths stay time-aligned. The whole graph then has a single
/// latency at its output, [`latency_frames`](AudioGraph::latency_frames),
/// which the application can report or pre-roll against. Delay lines are
/// allocated in `prepare` and processed without heap activity.
pub struct AudioGraph {
    ids: IdGenerator,
    slots: Vec<Slot>,
    index: HashMap<NodeId, usize>,
    edges: Vec<Edge>,
    midi_edges: Vec<MidiEdge>,
    output: Option<NodeId>,
    /// Topological order of slot indices.
    order: Vec<usize>,
    input_buffers: Vec<PlanarBuffer>,
    output_buffers: Vec<PlanarBuffer>,
    silent: Vec<bool>,
    /// Per slot, compiled by `compile`: does this node see the external
    /// MIDI buffer (`true`) or `empty_midi` (`false`)?
    receives_external_midi: Vec<bool>,
    /// Per slot: latency of the signal arriving at the node's input.
    input_latency: Vec<usize>,
    /// Per slot: latency of the signal leaving the node (`input + own`).
    output_latency: Vec<usize>,
    /// Compensation delay lines referenced by `Producer::delay`.
    delays: Vec<EdgeDelay>,
    compensate_latency: bool,
    prepared: Option<(SampleRate, usize)>,
    dirty: bool,
    stats: GraphStats,
    empty_output: PlanarBuffer,
    /// Zero-capacity buffer handed to nodes that are not MIDI targets.
    empty_midi: MidiEventBuffer,
}

impl Default for AudioGraph {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioGraph {
    /// Creates an empty graph.
    pub fn new() -> Self {
        Self {
            ids: IdGenerator::new(),
            slots: Vec::new(),
            index: HashMap::new(),
            edges: Vec::new(),
            midi_edges: Vec::new(),
            output: None,
            order: Vec::new(),
            input_buffers: Vec::new(),
            output_buffers: Vec::new(),
            silent: Vec::new(),
            receives_external_midi: Vec::new(),
            input_latency: Vec::new(),
            output_latency: Vec::new(),
            delays: Vec::new(),
            compensate_latency: true,
            prepared: None,
            dirty: true,
            stats: GraphStats::default(),
            empty_output: PlanarBuffer::new(0, 0),
            empty_midi: MidiEventBuffer::with_capacity(0),
        }
    }

    // ------------------------------------------------------------------
    // Editing (off the audio thread)
    // ------------------------------------------------------------------

    /// Adds a node and returns its ID.
    pub fn add_node(&mut self, node: Box<dyn Node>) -> NodeId {
        debug_assert_not_rt("AudioGraph::add_node");
        let id: NodeId = self.ids.next();
        let ch = node.channels();
        self.index.insert(id, self.slots.len());
        self.slots.push(Slot {
            id,
            node,
            inputs: ch.inputs,
            outputs: ch.outputs,
            producers: Vec::new(),
        });
        self.dirty = true;
        id
    }

    /// Removes a node and all edges touching it. Returns the node.
    pub fn remove_node(&mut self, id: NodeId) -> Result<Box<dyn Node>> {
        debug_assert_not_rt("AudioGraph::remove_node");
        let idx = self.slot_index(id)?;
        let slot = self.slots.swap_remove(idx);
        self.index.remove(&id);
        if idx < self.slots.len() {
            let moved = self.slots[idx].id;
            self.index.insert(moved, idx);
        }
        self.edges.retain(|e| e.from != id && e.to != id);
        self.midi_edges.retain(|e| e.to != id);
        if self.output == Some(id) {
            self.output = None;
        }
        self.dirty = true;
        Ok(slot.node)
    }

    /// Connects `from`'s output to `to`'s input.
    ///
    /// Fails if either node is unknown, the edge already exists, the
    /// producer has no outputs, the consumer has no inputs, or the edge
    /// would create a cycle.
    pub fn connect(&mut self, from: NodeId, to: NodeId) -> Result<()> {
        debug_assert_not_rt("AudioGraph::connect");
        let from_idx = self.slot_index(from)?;
        let to_idx = self.slot_index(to)?;
        if self.slots[from_idx].outputs == 0 {
            return Err(AuraError::ChannelMismatch {
                expected: 1,
                actual: 0,
            });
        }
        if self.slots[to_idx].inputs == 0 {
            return Err(AuraError::ChannelMismatch {
                expected: 1,
                actual: 0,
            });
        }
        if from == to || self.reaches(to, from) {
            return Err(AuraError::Cycle);
        }
        let edge = Edge { from, to };
        if self.edges.contains(&edge) {
            return Err(AuraError::InvalidArgument {
                what: "edge already exists",
            });
        }
        self.edges.push(edge);
        self.dirty = true;
        Ok(())
    }

    /// Removes an edge.
    pub fn disconnect(&mut self, from: NodeId, to: NodeId) -> Result<()> {
        debug_assert_not_rt("AudioGraph::disconnect");
        let before = self.edges.len();
        self.edges.retain(|e| !(e.from == from && e.to == to));
        if self.edges.len() == before {
            return Err(AuraError::NotFound {
                kind: "edge",
                id: from.raw(),
            });
        }
        self.dirty = true;
        Ok(())
    }

    /// Routes MIDI from `from` to node `to`.
    ///
    /// Once *any* MIDI edge exists, only nodes that are the target of a
    /// [`MidiPort::External`] edge receive the buffer passed to
    /// [`process`](AudioGraph::process); every other node sees an empty
    /// buffer. With no MIDI edges at all the buffer is broadcast to every
    /// node (see [`process`](AudioGraph::process)).
    ///
    /// Fails if `to` is unknown or the edge already exists. MIDI edges are
    /// independent of the audio schedule, so they can never form an audio
    /// cycle.
    ///
    /// ```
    /// use aura::primitives::SampleRate;
    /// use aura::primitives::graph::nodes::{MonoSynthNode, PassThroughNode};
    /// use aura::primitives::graph::{AudioGraph, BlockTime, MidiEdge, MidiPort};
    /// use aura::primitives::midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage};
    ///
    /// let mut graph = AudioGraph::new();
    /// let lead = graph.add_node(Box::new(MonoSynthNode::new(1)));
    /// let pad = graph.add_node(Box::new(MonoSynthNode::new(1)));
    /// let sink = graph.add_node(Box::new(PassThroughNode::new(1)));
    /// graph.connect(lead, sink).unwrap();
    /// graph.connect(pad, sink).unwrap();
    /// graph.set_output(sink).unwrap();
    ///
    /// // Only `lead` hears external MIDI.
    /// graph.connect_midi(MidiPort::External, lead).unwrap();
    /// assert_eq!(graph.midi_edges(), &[MidiEdge { from: MidiPort::External, to: lead }]);
    /// graph.prepare(SampleRate::HZ_48K, 64).unwrap();
    ///
    /// let ch = MidiChannel::new(0).unwrap();
    /// let mut midi = MidiEventBuffer::with_capacity(4);
    /// midi.push(MidiEvent { frame: 0, message: MidiMessage::NoteOn { channel: ch, note: 69, velocity: 127 } }).unwrap();
    /// graph.process(BlockTime::free_running(), 64, &midi, None).unwrap();
    /// assert!(graph.node_output(lead).unwrap().peak() > 0.9);
    /// assert!(graph.node_output(pad).unwrap().is_silent());
    /// ```
    pub fn connect_midi(&mut self, from: MidiPort, to: NodeId) -> Result<()> {
        debug_assert_not_rt("AudioGraph::connect_midi");
        self.slot_index(to)?;
        let edge = MidiEdge { from, to };
        if self.midi_edges.contains(&edge) {
            return Err(AuraError::InvalidArgument {
                what: "midi edge already exists",
            });
        }
        self.midi_edges.push(edge);
        self.dirty = true;
        Ok(())
    }

    /// Removes a MIDI edge. Fails with `NotFound` if it does not exist.
    pub fn disconnect_midi(&mut self, from: MidiPort, to: NodeId) -> Result<()> {
        debug_assert_not_rt("AudioGraph::disconnect_midi");
        let before = self.midi_edges.len();
        self.midi_edges.retain(|e| !(e.from == from && e.to == to));
        if self.midi_edges.len() == before {
            return Err(AuraError::NotFound {
                kind: "midi edge",
                id: to.raw(),
            });
        }
        self.dirty = true;
        Ok(())
    }

    /// All MIDI edges, in insertion order.
    pub fn midi_edges(&self) -> &[MidiEdge] {
        &self.midi_edges
    }

    /// Chooses the node whose output buffer [`output`](AudioGraph::output) returns.
    pub fn set_output(&mut self, id: NodeId) -> Result<()> {
        let idx = self.slot_index(id)?;
        if self.slots[idx].outputs == 0 {
            return Err(AuraError::ChannelMismatch {
                expected: 1,
                actual: 0,
            });
        }
        self.output = Some(id);
        self.dirty = true;
        Ok(())
    }

    /// The current output node.
    pub fn output_node(&self) -> Option<NodeId> {
        self.output
    }

    /// Number of nodes.
    pub fn node_count(&self) -> usize {
        self.slots.len()
    }

    /// All edges.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// IDs of all nodes (unordered).
    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.slots.iter().map(|s| s.id)
    }

    /// `true` if `id` is a node in this graph.
    pub fn contains(&self, id: NodeId) -> bool {
        self.index.contains_key(&id)
    }

    /// Name of a node.
    pub fn node_name(&self, id: NodeId) -> Result<&str> {
        Ok(self.slots[self.slot_index(id)?].node.name())
    }

    /// Mutable access to a node (e.g. to call a fixture's setters in tests).
    pub fn node_mut(&mut self, id: NodeId) -> Result<&mut dyn Node> {
        let idx = self.slot_index(id)?;
        Ok(self.slots[idx].node.as_mut())
    }

    /// The compiled processing order (valid after `prepare`).
    pub fn schedule(&self) -> Vec<NodeId> {
        self.order.iter().map(|&i| self.slots[i].id).collect()
    }

    /// `true` if edits were made since the last `prepare`.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Sample rate and maximum block size from the last `prepare`.
    pub fn prepared_config(&self) -> Option<(SampleRate, usize)> {
        if self.dirty {
            None
        } else {
            self.prepared
        }
    }

    // ------------------------------------------------------------------
    // Latency
    // ------------------------------------------------------------------

    /// Turns latency compensation on or off (on by default). Takes effect
    /// at the next `prepare`; the graph is marked dirty.
    pub fn set_latency_compensation(&mut self, enabled: bool) {
        debug_assert_not_rt("AudioGraph::set_latency_compensation");
        if self.compensate_latency != enabled {
            self.compensate_latency = enabled;
            self.dirty = true;
        }
    }

    /// `true` if edges are delayed to align parallel paths.
    pub fn latency_compensation(&self) -> bool {
        self.compensate_latency
    }

    /// Total latency in frames from the graph's inputs to its output node,
    /// as compiled by the last `prepare`. `0` if the graph is dirty or has
    /// no output node.
    ///
    /// This is the longest path latency ending at the output; with
    /// compensation on, every path to the output has exactly this latency.
    pub fn latency_frames(&self) -> usize {
        match self.output.and_then(|id| self.index.get(&id)) {
            Some(&i) if !self.dirty => self.output_latency.get(i).copied().unwrap_or(0),
            _ => 0,
        }
    }

    /// Latency of the signal arriving at `id`'s input (largest path latency
    /// among its producers). Fails if the graph is dirty.
    pub fn node_input_latency(&self, id: NodeId) -> Result<usize> {
        if self.dirty {
            return Err(AuraError::NotPrepared);
        }
        Ok(self.input_latency[self.slot_index(id)?])
    }

    /// Latency of the signal leaving `id` (input latency plus the node's own
    /// [`Node::latency_frames`]). Fails if the graph is dirty.
    pub fn node_output_latency(&self, id: NodeId) -> Result<usize> {
        if self.dirty {
            return Err(AuraError::NotPrepared);
        }
        Ok(self.output_latency[self.slot_index(id)?])
    }

    /// Compensation delay in frames inserted on the edge `from → to`
    /// (`0` when the edge is not delayed). Fails if the graph is dirty or
    /// the edge does not exist.
    pub fn edge_delay(&self, from: NodeId, to: NodeId) -> Result<usize> {
        if self.dirty {
            return Err(AuraError::NotPrepared);
        }
        let from_idx = self.slot_index(from)?;
        let to_idx = self.slot_index(to)?;
        let producer = self.slots[to_idx]
            .producers
            .iter()
            .find(|p| p.slot == from_idx)
            .ok_or(AuraError::NotFound {
                kind: "edge",
                id: from.raw(),
            })?;
        Ok(producer.delay.map_or(0, |d| self.delays[d].line.delay()))
    }

    // ------------------------------------------------------------------
    // Compilation
    // ------------------------------------------------------------------

    /// Compiles the schedule, allocates buffers and prepares every node.
    ///
    /// Must be called off the audio thread after any edit. Fails with
    /// [`AuraError::Cycle`] if the edge set is cyclic (cannot normally
    /// happen because `connect` rejects cycles).
    pub fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        debug_assert_not_rt("AudioGraph::prepare");
        if max_frames == 0 {
            return Err(AuraError::InvalidArgument { what: "max_frames" });
        }
        self.compile()?;
        let n = self.slots.len();
        self.input_buffers = self
            .slots
            .iter()
            .map(|s| PlanarBuffer::new(s.inputs, max_frames))
            .collect();
        self.output_buffers = self
            .slots
            .iter()
            .map(|s| PlanarBuffer::new(s.outputs, max_frames))
            .collect();
        self.silent = vec![true; n];
        for slot in &mut self.slots {
            slot.node.prepare(sample_rate, max_frames);
        }
        // Nodes may only know their latency once prepared, so the latency
        // pass runs after `Node::prepare`.
        self.compile_latency(max_frames);
        self.prepared = Some((sample_rate, max_frames));
        self.dirty = false;
        Ok(())
    }

    /// Resets every node's internal state (and the compensation delay
    /// lines) without reallocating.
    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.node.reset();
        }
        for d in &mut self.delays {
            d.line.reset();
        }
    }

    /// Computes per-node arrival/departure latencies along the schedule and
    /// allocates a delay line for every edge that needs one.
    fn compile_latency(&mut self, max_frames: usize) {
        let n = self.slots.len();
        self.input_latency = vec![0; n];
        self.output_latency = vec![0; n];
        self.delays.clear();
        for &i in &self.order {
            let arrival = self.slots[i]
                .producers
                .iter()
                .map(|p| self.output_latency[p.slot])
                .max()
                .unwrap_or(0);
            self.input_latency[i] = arrival;
            self.output_latency[i] = arrival + self.slots[i].node.latency_frames();
        }
        if !self.compensate_latency {
            return;
        }
        for i in 0..n {
            let arrival = self.input_latency[i];
            for k in 0..self.slots[i].producers.len() {
                let p = self.slots[i].producers[k].slot;
                let delay = arrival - self.output_latency[p];
                if delay > 0 {
                    let channels = self.slots[p].outputs;
                    self.delays.push(EdgeDelay {
                        line: DelayLine::new(channels, delay, max_frames),
                        out: PlanarBuffer::new(channels, max_frames),
                    });
                    self.slots[i].producers[k].delay = Some(self.delays.len() - 1);
                }
            }
        }
    }

    fn compile(&mut self) -> Result<()> {
        let n = self.slots.len();
        let mut indegree = vec![0usize; n];
        let mut consumers: Vec<Vec<usize>> = vec![Vec::new(); n];
        for slot in &mut self.slots {
            slot.producers.clear();
        }
        for e in &self.edges {
            let from = self.index[&e.from];
            let to = self.index[&e.to];
            consumers[from].push(to);
            indegree[to] += 1;
            self.slots[to].producers.push(Producer {
                slot: from,
                delay: None,
            });
        }
        // Kahn's algorithm; ties broken by insertion order for determinism.
        let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
        ready.reverse();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = ready.pop() {
            order.push(i);
            for &c in &consumers[i] {
                indegree[c] -= 1;
                if indegree[c] == 0 {
                    ready.push(c);
                }
            }
        }
        if order.len() != n {
            return Err(AuraError::Cycle);
        }
        self.order = order;

        // MIDI routing: broadcast when there are no MIDI edges, otherwise only
        // the targets of External edges hear the incoming buffer.
        let broadcast = self.midi_edges.is_empty();
        self.receives_external_midi.clear();
        self.receives_external_midi.resize(n, broadcast);
        for e in &self.midi_edges {
            match e.from {
                MidiPort::External => self.receives_external_midi[self.index[&e.to]] = true,
            }
        }
        Ok(())
    }

    /// `true` if `to` is reachable from `from` following edges.
    fn reaches(&self, from: NodeId, to: NodeId) -> bool {
        let mut stack = vec![from];
        let mut seen: Vec<NodeId> = Vec::new();
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if seen.contains(&n) {
                continue;
            }
            seen.push(n);
            for e in &self.edges {
                if e.from == n {
                    stack.push(e.to);
                }
            }
        }
        false
    }

    fn slot_index(&self, id: NodeId) -> Result<usize> {
        self.index.get(&id).copied().ok_or(AuraError::NotFound {
            kind: "node",
            id: id.raw(),
        })
    }

    // ------------------------------------------------------------------
    // Processing (audio thread)
    // ------------------------------------------------------------------

    /// Processes one block of `frames` frames. Allocation-free.
    ///
    /// MIDI delivery follows the compiled [`midi_edges`](AudioGraph::midi_edges):
    ///
    /// * **no MIDI edges** — `midi` is broadcast: every node's
    ///   `ProcessContext::midi` is this buffer;
    /// * **one or more MIDI edges** — only nodes targeted by a
    ///   [`MidiPort::External`] edge see `midi`; all others see a
    ///   pre-allocated empty buffer.
    ///
    /// The choice is a pointer select per node from a table built in
    /// `prepare`; the buffer is never copied or filtered by channel. Fails if
    /// the graph is dirty/unprepared or `frames` exceeds the prepared
    /// maximum.
    pub fn process(
        &mut self,
        time: BlockTime,
        frames: usize,
        midi: &MidiEventBuffer,
        input: Option<&PlanarBuffer>,
    ) -> Result<()> {
        let (sample_rate, max_frames) = match (self.dirty, self.prepared) {
            (false, Some(cfg)) => cfg,
            _ => return Err(AuraError::NotPrepared),
        };
        if frames > max_frames {
            return Err(AuraError::CapacityExceeded {
                what: "block frames",
                capacity: max_frames,
            });
        }
        self.stats = GraphStats::default();

        for &i in &self.order {
            let slot = &mut self.slots[i];
            // Gather inputs.
            let mut input_is_silent = true;
            if slot.inputs > 0 {
                let input = &mut self.input_buffers[i];
                input.set_frames(frames)?;
                input.clear();
                for producer in &slot.producers {
                    let p = producer.slot;
                    match producer.delay {
                        None => {
                            if self.silent[p] {
                                continue;
                            }
                            mix_into(input, &self.output_buffers[p]);
                        }
                        Some(d) => {
                            // The delay line must always advance, even when
                            // the producer is silent, so its history stays
                            // aligned with the timeline.
                            let delay = &mut self.delays[d];
                            delay.out.set_frames(frames)?;
                            let src = if self.silent[p] {
                                None
                            } else {
                                Some(&self.output_buffers[p])
                            };
                            if delay.line.process(src, frames, &mut delay.out) {
                                continue;
                            }
                            mix_into(input, &delay.out);
                            self.stats.delayed_mixes += 1;
                        }
                    }
                    input_is_silent = false;
                    self.stats.mixes += 1;
                }
            }
            let output = &mut self.output_buffers[i];
            output.set_frames(frames)?;
            let node_midi = if self.receives_external_midi[i] {
                midi
            } else {
                &self.empty_midi
            };
            let ctx = ProcessContext {
                sample_rate,
                frames,
                time,
                midi: node_midi,
                input_is_silent,
                input,
            };
            let status = slot.node.process(&ctx, &self.input_buffers[i], output);
            self.stats.nodes_processed += 1;
            let is_silent = status == ProcessStatus::Silent || slot.outputs == 0;
            if status == ProcessStatus::Silent {
                self.stats.nodes_silent += 1;
            }
            self.silent[i] = is_silent;
        }

        // Guarantee the output buffer is meaningful even when the node was silent.
        if let Some(out) = self.output {
            let i = self.index[&out];
            if self.silent[i] {
                self.output_buffers[i].clear();
            }
        }
        Ok(())
    }

    /// The output node's buffer from the last `process` call.
    ///
    /// Returns an empty (0-channel) buffer if no output node is set.
    pub fn output(&self) -> &PlanarBuffer {
        match self.output.and_then(|id| self.index.get(&id)) {
            Some(&i) if !self.dirty => &self.output_buffers[i],
            _ => &self.empty_output,
        }
    }

    /// The output buffer of any node from the last `process` call.
    pub fn node_output(&self, id: NodeId) -> Result<&PlanarBuffer> {
        if self.dirty {
            return Err(AuraError::NotPrepared);
        }
        Ok(&self.output_buffers[self.slot_index(id)?])
    }

    /// Whether a node reported silence in the last `process` call. A silent
    /// node's buffer ([`node_output`](Self::node_output)) need not have been
    /// cleared, so read this first and take silence for zeros.
    pub fn node_is_silent(&self, id: NodeId) -> Result<bool> {
        if self.dirty {
            return Err(AuraError::NotPrepared);
        }
        Ok(self.silent[self.slot_index(id)?])
    }

    /// Statistics from the last `process` call.
    pub fn stats(&self) -> GraphStats {
        self.stats
    }
}

impl crate::primitives::graph::BlockProcessor for AudioGraph {
    fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        AudioGraph::prepare(self, sample_rate, max_frames)
    }

    fn process(
        &mut self,
        time: BlockTime,
        frames: usize,
        midi: &MidiEventBuffer,
        input: Option<&PlanarBuffer>,
    ) -> Result<()> {
        AudioGraph::process(self, time, frames, midi, input)
    }

    fn output(&self) -> &PlanarBuffer {
        AudioGraph::output(self)
    }

    fn output_channels(&self) -> usize {
        self.output
            .and_then(|id| self.index.get(&id))
            .map_or(0, |&i| self.slots[i].outputs)
    }

    fn reset(&mut self) {
        AudioGraph::reset(self)
    }

    fn latency_frames(&self) -> usize {
        AudioGraph::latency_frames(self)
    }
}

/// Mixes `src` into `dst` following the channel-count rule:
/// equal counts → 1:1; mono source → copied to every destination channel;
/// mono destination → average of source channels; otherwise the first
/// `min(src, dst)` channels are mapped 1:1.
fn mix_into(dst: &mut PlanarBuffer, src: &PlanarBuffer) {
    let (sc, dc) = (src.channels(), dst.channels());
    if sc == 0 || dc == 0 {
        return;
    }
    if sc == dc {
        for ch in 0..dc {
            let _ = dst.add_channel_from(ch, src, ch, 1.0);
        }
    } else if sc == 1 {
        for ch in 0..dc {
            let _ = dst.add_channel_from(ch, src, 0, 1.0);
        }
    } else if dc == 1 {
        let g = 1.0 / sc as f32;
        for ch in 0..sc {
            let _ = dst.add_channel_from(0, src, ch, g);
        }
    } else {
        for ch in 0..sc.min(dc) {
            let _ = dst.add_channel_from(ch, src, ch, 1.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::nodes::{
        ConstantNode, GainNode, PassThroughNode, SilenceNode, SineNode,
    };

    const R: SampleRate = SampleRate::HZ_48K;

    fn no_midi() -> MidiEventBuffer {
        MidiEventBuffer::with_capacity(0)
    }

    fn run(graph: &mut AudioGraph, frames: usize) {
        graph
            .process(BlockTime::free_running(), frames, &no_midi(), None)
            .unwrap();
    }

    #[test]
    fn empty_graph_processes_and_outputs_nothing() {
        let mut g = AudioGraph::new();
        g.prepare(R, 64).unwrap();
        run(&mut g, 64);
        assert_eq!(g.output().channels(), 0);
        assert_eq!(g.stats().nodes_processed, 0);
    }

    #[test]
    fn process_before_prepare_or_after_edit_fails() {
        let mut g = AudioGraph::new();
        let c = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        g.set_output(c).unwrap();
        assert_eq!(
            g.process(BlockTime::free_running(), 16, &no_midi(), None),
            Err(AuraError::NotPrepared)
        );
        g.prepare(R, 16).unwrap();
        run(&mut g, 16);
        assert!(g.add_node(Box::new(SilenceNode::new(1))).is_valid());
        assert!(g.is_dirty());
        assert_eq!(
            g.process(BlockTime::free_running(), 16, &no_midi(), None),
            Err(AuraError::NotPrepared)
        );
        assert_eq!(g.output().channels(), 0);
        assert!(g.prepared_config().is_none());
        g.prepare(R, 16).unwrap();
        assert_eq!(g.prepared_config(), Some((R, 16)));
        assert!(matches!(
            g.process(BlockTime::free_running(), 17, &no_midi(), None),
            Err(AuraError::CapacityExceeded { .. })
        ));
        assert!(g.prepare(R, 0).is_err());
    }

    #[test]
    fn linear_chain_sums_and_scales() {
        let mut g = AudioGraph::new();
        let a = g.add_node(Box::new(ConstantNode::new(0.25, 2)));
        let b = g.add_node(Box::new(ConstantNode::new(0.5, 2)));
        let gain = g.add_node(Box::new(GainNode::new(2.0, 2)));
        g.connect(a, gain).unwrap();
        g.connect(b, gain).unwrap();
        g.set_output(gain).unwrap();
        g.prepare(R, 32).unwrap();
        run(&mut g, 32);
        let out = g.output();
        assert_eq!(out.frames(), 32);
        for ch in 0..2 {
            assert!(out.channel(ch).iter().all(|&s| (s - 1.5).abs() < 1e-6));
        }
        assert_eq!(g.stats().mixes, 2);
        assert_eq!(g.stats().nodes_processed, 3);
    }

    #[test]
    fn smaller_blocks_than_max_are_fine() {
        let mut g = AudioGraph::new();
        let c = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        g.set_output(c).unwrap();
        g.prepare(R, 512).unwrap();
        run(&mut g, 7);
        assert_eq!(g.output().frames(), 7);
        run(&mut g, 512);
        assert_eq!(g.output().frames(), 512);
    }

    #[test]
    fn schedule_is_topological_and_deterministic() {
        let mut g = AudioGraph::new();
        let sink = g.add_node(Box::new(PassThroughNode::new(1)));
        let mid = g.add_node(Box::new(PassThroughNode::new(1)));
        let src = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        g.connect(src, mid).unwrap();
        g.connect(mid, sink).unwrap();
        g.set_output(sink).unwrap();
        g.prepare(R, 8).unwrap();
        assert_eq!(g.schedule(), vec![src, mid, sink]);
        run(&mut g, 8);
        assert_eq!(g.output().channel(0), &[1.0; 8]);
        // adding an unconnected node keeps a valid order
        let lonely = g.add_node(Box::new(SilenceNode::new(1)));
        g.prepare(R, 8).unwrap();
        let order = g.schedule();
        assert!(order.iter().position(|&n| n == src) < order.iter().position(|&n| n == mid));
        assert!(order.iter().position(|&n| n == mid) < order.iter().position(|&n| n == sink));
        assert!(order.contains(&lonely));
    }

    #[test]
    fn cycles_and_bad_edges_are_rejected() {
        let mut g = AudioGraph::new();
        let a = g.add_node(Box::new(PassThroughNode::new(1)));
        let b = g.add_node(Box::new(PassThroughNode::new(1)));
        let c = g.add_node(Box::new(PassThroughNode::new(1)));
        g.connect(a, b).unwrap();
        g.connect(b, c).unwrap();
        assert_eq!(g.connect(c, a), Err(AuraError::Cycle));
        assert_eq!(g.connect(a, a), Err(AuraError::Cycle));
        assert!(g.connect(a, b).is_err()); // duplicate
        let ghost = NodeId::from_raw(999);
        assert!(matches!(
            g.connect(a, ghost),
            Err(AuraError::NotFound { .. })
        ));
        assert!(matches!(
            g.set_output(ghost),
            Err(AuraError::NotFound { .. })
        ));
        let gen = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        assert!(g.connect(a, gen).is_err()); // generator has no inputs
        assert!(g.disconnect(c, a).is_err());
        g.disconnect(a, b).unwrap();
        g.connect(c, a).unwrap(); // no longer a cycle
    }

    #[test]
    fn remove_node_drops_edges_and_output() {
        let mut g = AudioGraph::new();
        let a = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        let b = g.add_node(Box::new(PassThroughNode::new(1)));
        let c = g.add_node(Box::new(PassThroughNode::new(1)));
        g.connect(a, b).unwrap();
        g.connect(b, c).unwrap();
        g.set_output(b).unwrap();
        let removed = g.remove_node(b).unwrap();
        assert_eq!(removed.name(), "passthrough");
        assert_eq!(g.node_count(), 2);
        assert!(g.edges().is_empty());
        assert_eq!(g.output_node(), None);
        assert!(!g.contains(b));
        assert!(g.contains(c));
        assert_eq!(g.node_name(c).unwrap(), "passthrough");
        assert!(g.remove_node(b).is_err());
        // graph still works after swap-remove reindexing
        g.connect(a, c).unwrap();
        g.set_output(c).unwrap();
        g.prepare(R, 4).unwrap();
        run(&mut g, 4);
        assert_eq!(g.output().channel(0), &[1.0; 4]);
        assert_eq!(g.node_output(a).unwrap().channel(0), &[1.0; 4]);
    }

    #[test]
    fn silence_propagates_and_is_not_mixed() {
        let mut g = AudioGraph::new();
        let s = g.add_node(Box::new(SilenceNode::new(1)));
        let c = g.add_node(Box::new(ConstantNode::new(0.5, 1)));
        let gain = g.add_node(Box::new(GainNode::new(1.0, 1)));
        g.connect(s, gain).unwrap();
        g.connect(c, gain).unwrap();
        g.set_output(gain).unwrap();
        g.prepare(R, 8).unwrap();
        run(&mut g, 8);
        assert_eq!(g.stats().mixes, 1);
        assert_eq!(g.stats().nodes_silent, 1);
        assert_eq!(g.output().channel(0), &[0.5; 8]);

        // A silent output node yields a cleared buffer, not stale data.
        let mut g = AudioGraph::new();
        let s = g.add_node(Box::new(SilenceNode::new(2)));
        let p = g.add_node(Box::new(PassThroughNode::new(2)));
        g.connect(s, p).unwrap();
        g.set_output(p).unwrap();
        g.prepare(R, 8).unwrap();
        run(&mut g, 8);
        assert!(g.output().is_silent());
        assert_eq!(g.stats().nodes_silent, 2);
        assert_eq!(g.stats().mixes, 0);
    }

    #[test]
    fn channel_adaptation_rules() {
        // mono -> stereo fans out
        let mut g = AudioGraph::new();
        let m = g.add_node(Box::new(ConstantNode::new(0.5, 1)));
        let st = g.add_node(Box::new(PassThroughNode::new(2)));
        g.connect(m, st).unwrap();
        g.set_output(st).unwrap();
        g.prepare(R, 4).unwrap();
        run(&mut g, 4);
        assert_eq!(g.output().channel(0), &[0.5; 4]);
        assert_eq!(g.output().channel(1), &[0.5; 4]);

        // stereo -> mono averages
        let mut g = AudioGraph::new();
        let st = g.add_node(Box::new(ConstantNode::new(0.5, 2)));
        let m = g.add_node(Box::new(PassThroughNode::new(1)));
        g.connect(st, m).unwrap();
        g.set_output(m).unwrap();
        g.prepare(R, 4).unwrap();
        run(&mut g, 4);
        assert!(g
            .output()
            .channel(0)
            .iter()
            .all(|&s| (s - 0.5).abs() < 1e-6));

        // 4 -> 2 maps the first two channels
        let mut g = AudioGraph::new();
        let quad = g.add_node(Box::new(ConstantNode::new(0.25, 4)));
        let st = g.add_node(Box::new(PassThroughNode::new(2)));
        g.connect(quad, st).unwrap();
        g.set_output(st).unwrap();
        g.prepare(R, 4).unwrap();
        run(&mut g, 4);
        assert_eq!(g.output().channels(), 2);
        assert_eq!(g.output().channel(1), &[0.25; 4]);
    }

    #[test]
    fn sine_chain_produces_expected_amplitude_and_continuity() {
        let mut g = AudioGraph::new();
        let s = g.add_node(Box::new(SineNode::new(1_000.0, 1.0, 1)));
        let gain = g.add_node(Box::new(GainNode::new(0.5, 1)));
        g.connect(s, gain).unwrap();
        g.set_output(gain).unwrap();
        g.prepare(R, 480).unwrap();
        let mut all = Vec::new();
        for _ in 0..10 {
            run(&mut g, 480);
            all.extend_from_slice(g.output().channel(0));
        }
        let peak = all.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 0.5 + 1e-5 && peak > 0.49, "peak {peak}");
        // no discontinuity at block boundaries: max step bounded by 2*pi*f/sr*amp
        let max_step = 2.0 * core::f32::consts::PI * 1_000.0 / 48_000.0 * 0.5 * 1.01;
        for w in all.windows(2) {
            assert!((w[1] - w[0]).abs() <= max_step);
        }
        g.reset();
        run(&mut g, 480);
        assert_eq!(g.output().channel(0)[0], 0.0); // phase reset
    }

    fn note_on_buffer() -> MidiEventBuffer {
        use crate::primitives::midi::{MidiChannel, MidiEvent, MidiMessage};
        let ch = MidiChannel::new(0).unwrap();
        let mut midi = MidiEventBuffer::with_capacity(4);
        midi.push(MidiEvent {
            frame: 0,
            message: MidiMessage::NoteOn {
                channel: ch,
                note: 69,
                velocity: 127,
            },
        })
        .unwrap();
        midi
    }

    /// Two synths into one sink; returns `(graph, a, b, sink)`.
    fn two_synths() -> (AudioGraph, NodeId, NodeId, NodeId) {
        use crate::primitives::graph::nodes::MonoSynthNode;
        let mut g = AudioGraph::new();
        let a = g.add_node(Box::new(MonoSynthNode::new(1)));
        let b = g.add_node(Box::new(MonoSynthNode::new(1)));
        let sink = g.add_node(Box::new(PassThroughNode::new(1)));
        g.connect(a, sink).unwrap();
        g.connect(b, sink).unwrap();
        g.set_output(sink).unwrap();
        (g, a, b, sink)
    }

    #[test]
    fn midi_edge_routes_external_midi_to_targets_only() {
        let (mut g, a, b, sink) = two_synths();
        g.connect_midi(MidiPort::External, a).unwrap();
        assert_eq!(
            g.midi_edges(),
            &[MidiEdge {
                from: MidiPort::External,
                to: a
            }]
        );
        g.prepare(R, 64).unwrap();
        let midi = note_on_buffer();
        g.process(BlockTime::free_running(), 64, &midi, None)
            .unwrap();
        assert!(
            g.node_output(a).unwrap().peak() > 0.9,
            "routed synth sounds"
        );
        assert!(
            g.node_output(b).unwrap().is_silent(),
            "unrouted synth is silent"
        );
        assert_eq!(g.stats().nodes_silent, 1);
        assert_eq!(g.stats().mixes, 1, "only A is mixed into the sink");
        assert!(g.node_output(sink).unwrap().peak() > 0.9);

        // Re-routing to B flips which synth hears the note (A keeps its note
        // from the previous block, so reset first).
        g.disconnect_midi(MidiPort::External, a).unwrap();
        g.connect_midi(MidiPort::External, b).unwrap();
        g.prepare(R, 64).unwrap();
        g.reset();
        g.process(BlockTime::free_running(), 64, &midi, None)
            .unwrap();
        assert!(g.node_output(b).unwrap().peak() > 0.9);
        assert_eq!(g.stats().nodes_silent, 1);
        assert_eq!(g.stats().mixes, 1);
    }

    #[test]
    fn without_midi_edges_midi_is_broadcast() {
        let (mut g, a, b, _sink) = two_synths();
        g.prepare(R, 64).unwrap();
        g.process(BlockTime::free_running(), 64, &note_on_buffer(), None)
            .unwrap();
        assert!(g.node_output(a).unwrap().peak() > 0.9);
        assert!(g.node_output(b).unwrap().peak() > 0.9);
        assert_eq!(g.stats().nodes_silent, 0);
        assert_eq!(g.stats().mixes, 2);
        // Removing the last MIDI edge restores broadcast.
        g.connect_midi(MidiPort::External, a).unwrap();
        g.disconnect_midi(MidiPort::External, a).unwrap();
        assert!(g.midi_edges().is_empty());
        g.prepare(R, 64).unwrap();
        g.reset();
        g.process(BlockTime::free_running(), 64, &note_on_buffer(), None)
            .unwrap();
        assert_eq!(g.stats().mixes, 2);
    }

    #[test]
    fn midi_edge_errors_and_removal() {
        let (mut g, a, b, _sink) = two_synths();
        g.connect_midi(MidiPort::External, a).unwrap();
        assert!(matches!(
            g.connect_midi(MidiPort::External, a),
            Err(AuraError::InvalidArgument {
                what: "midi edge already exists"
            })
        ));
        let ghost = NodeId::from_raw(999);
        assert!(matches!(
            g.connect_midi(MidiPort::External, ghost),
            Err(AuraError::NotFound { .. })
        ));
        assert!(matches!(
            g.disconnect_midi(MidiPort::External, b),
            Err(AuraError::NotFound { .. })
        ));
        // Editing MIDI edges dirties the graph like audio edits.
        g.prepare(R, 8).unwrap();
        g.connect_midi(MidiPort::External, b).unwrap();
        assert!(g.is_dirty());
        assert_eq!(
            g.process(BlockTime::free_running(), 8, &no_midi(), None),
            Err(AuraError::NotPrepared)
        );
        // remove_node drops the MIDI edges targeting it.
        g.remove_node(a).unwrap();
        assert_eq!(
            g.midi_edges(),
            &[MidiEdge {
                from: MidiPort::External,
                to: b
            }]
        );
        g.remove_node(b).unwrap();
        assert!(g.midi_edges().is_empty());
        g.prepare(R, 8).unwrap();
        run(&mut g, 8);
    }

    /// A one-frame impulse generator: `1.0` at the first frame ever rendered.
    struct ImpulseNode {
        fired: bool,
    }
    impl Node for ImpulseNode {
        fn channels(&self) -> NodeChannels {
            NodeChannels::generator(1)
        }
        fn reset(&mut self) {
            self.fired = false;
        }
        fn process(
            &mut self,
            _ctx: &ProcessContext<'_>,
            _i: &PlanarBuffer,
            output: &mut PlanarBuffer,
        ) -> ProcessStatus {
            output.clear();
            if self.fired {
                return ProcessStatus::Silent;
            }
            output.channel_mut(0)[0] = 1.0;
            self.fired = true;
            ProcessStatus::Normal
        }
    }

    use crate::primitives::graph::nodes::DelayNode;
    use crate::primitives::graph::NodeChannels;

    /// Impulse → (direct, delayed by `d`) → sum. Returns `(graph, sum, direct edge source)`.
    fn parallel_paths(d: usize) -> (AudioGraph, NodeId, NodeId, NodeId) {
        let mut g = AudioGraph::new();
        let src = g.add_node(Box::new(ImpulseNode { fired: false }));
        let slow = g.add_node(Box::new(DelayNode::new(d, 1)));
        let sum = g.add_node(Box::new(PassThroughNode::new(1)));
        g.connect(src, slow).unwrap();
        g.connect(slow, sum).unwrap();
        g.connect(src, sum).unwrap();
        g.set_output(sum).unwrap();
        (g, src, slow, sum)
    }

    fn collect(g: &mut AudioGraph, block: usize, blocks: usize) -> Vec<f32> {
        let mut all = Vec::new();
        for _ in 0..blocks {
            run(g, block);
            all.extend_from_slice(g.output().channel(0));
        }
        all
    }

    #[test]
    fn parallel_paths_are_aligned_by_compensation() {
        let (mut g, src, slow, sum) = parallel_paths(10);
        g.prepare(R, 4).unwrap();
        assert!(g.latency_compensation());
        assert_eq!(g.latency_frames(), 10);
        assert_eq!(g.node_input_latency(src).unwrap(), 0);
        assert_eq!(g.node_output_latency(slow).unwrap(), 10);
        assert_eq!(g.node_input_latency(sum).unwrap(), 10);
        assert_eq!(
            g.edge_delay(src, sum).unwrap(),
            10,
            "direct path is delayed to match"
        );
        assert_eq!(g.edge_delay(slow, sum).unwrap(), 0);
        let out = collect(&mut g, 4, 6);
        // Both copies of the impulse land at frame 10 and sum to 2.0.
        let mut expected = vec![0.0; 24];
        expected[10] = 2.0;
        assert_eq!(out, expected);
        assert_eq!(
            g.stats().delayed_mixes,
            0,
            "delay line drained: nothing mixed in the last block"
        );

        // Without compensation the direct copy arrives at frame 0.
        g.set_latency_compensation(false);
        assert!(g.is_dirty());
        g.prepare(R, 4).unwrap();
        g.reset();
        assert_eq!(
            g.latency_frames(),
            10,
            "reported latency is still the longest path"
        );
        assert_eq!(g.edge_delay(src, sum).unwrap(), 0);
        let out = collect(&mut g, 4, 6);
        let mut expected = vec![0.0; 24];
        expected[0] = 1.0;
        expected[10] = 1.0;
        assert_eq!(out, expected);
    }

    #[test]
    fn compensation_is_allocation_free_after_prepare_and_resets() {
        let (mut g, _src, _slow, _sum) = parallel_paths(3);
        g.prepare(R, 8).unwrap();
        let out = collect(&mut g, 8, 1);
        assert_eq!(out[3], 2.0);
        // Delayed mixes are counted while the line carries signal (the
        // silence flag is conservative: it tracks pushed silence, not content).
        g.reset();
        run(&mut g, 2);
        assert_eq!(
            g.stats().delayed_mixes,
            1,
            "impulse entered the line this block"
        );
        assert_eq!(g.output().channel(0), &[0.0, 0.0]);
        run(&mut g, 2);
        assert_eq!(g.stats().delayed_mixes, 1);
        assert_eq!(g.stats().mixes, 2);
        assert_eq!(g.output().channel(0), &[0.0, 2.0]);
        // Once only silence has been pushed for delay + frames, the edge is skipped.
        run(&mut g, 2);
        run(&mut g, 2);
        assert_eq!(g.stats().delayed_mixes, 0);
        assert_eq!(g.stats().mixes, 0);
        // reset clears the delay line too: no stale impulse after a reset.
        g.reset();
        let out = collect(&mut g, 8, 1);
        assert_eq!(out[3], 2.0);
        assert!(out[..3].iter().all(|&s| s == 0.0));
    }

    #[test]
    fn nested_latencies_accumulate_along_the_longest_path() {
        // src -> a(2) -> b(5) -> sum ; src -> c(4) -> sum ; src -> sum
        let mut g = AudioGraph::new();
        let src = g.add_node(Box::new(ImpulseNode { fired: false }));
        let a = g.add_node(Box::new(DelayNode::new(2, 1)));
        let b = g.add_node(Box::new(DelayNode::new(5, 1)));
        let c = g.add_node(Box::new(DelayNode::new(4, 1)));
        let sum = g.add_node(Box::new(PassThroughNode::new(1)));
        g.connect(src, a).unwrap();
        g.connect(a, b).unwrap();
        g.connect(b, sum).unwrap();
        g.connect(src, c).unwrap();
        g.connect(c, sum).unwrap();
        g.connect(src, sum).unwrap();
        g.set_output(sum).unwrap();
        g.prepare(R, 16).unwrap();
        assert_eq!(g.latency_frames(), 7);
        assert_eq!(g.node_output_latency(b).unwrap(), 7);
        assert_eq!(g.edge_delay(c, sum).unwrap(), 3);
        assert_eq!(g.edge_delay(src, sum).unwrap(), 7);
        assert_eq!(g.edge_delay(b, sum).unwrap(), 0);
        assert_eq!(g.edge_delay(src, a).unwrap(), 0);
        let out = collect(&mut g, 16, 1);
        let mut expected = vec![0.0; 16];
        expected[7] = 3.0;
        assert_eq!(out, expected);
        assert!(g.edge_delay(sum, src).is_err(), "no such edge");
        assert!(g.node_input_latency(NodeId::from_raw(999)).is_err());
        g.add_node(Box::new(SilenceNode::new(1)));
        assert!(g.node_input_latency(src).is_err(), "dirty");
        assert!(g.edge_delay(src, sum).is_err());
        assert_eq!(g.latency_frames(), 0);
    }

    #[test]
    fn channel_adaptation_still_applies_on_delayed_edges() {
        // mono impulse delayed onto a stereo sink, next to a stereo delayed path.
        let mut g = AudioGraph::new();
        let src = g.add_node(Box::new(ImpulseNode { fired: false }));
        let slow = g.add_node(Box::new(DelayNode::new(2, 2)));
        let sum = g.add_node(Box::new(PassThroughNode::new(2)));
        g.connect(src, slow).unwrap();
        g.connect(slow, sum).unwrap();
        g.connect(src, sum).unwrap();
        g.set_output(sum).unwrap();
        g.prepare(R, 4).unwrap();
        run(&mut g, 4);
        for ch in 0..2 {
            assert_eq!(g.output().channel(ch), &[0.0, 0.0, 2.0, 0.0]);
        }
        assert_eq!(g.node_mut(slow).unwrap().latency_frames(), 2);
    }

    #[test]
    fn node_mut_gives_access_to_fixtures() {
        let mut g = AudioGraph::new();
        let c = g.add_node(Box::new(ConstantNode::new(1.0, 1)));
        assert_eq!(g.node_mut(c).unwrap().name(), "constant");
        assert!(g.node_mut(NodeId::from_raw(42)).is_err());
        assert!(g.node_output(c).is_err()); // dirty
    }
}
