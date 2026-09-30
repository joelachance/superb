//! Lock-free live editing: edit a draft graph off-thread, publish it, and let
//! the audio thread swap it in between blocks without stopping the stream.
//!
//! # Protocol
//!
//! [`live_pair`] returns a [`GraphEditor`] (control thread) and a
//! [`LiveGraph`] (audio thread) joined by two wait-free SPSC queues from
//! `superb-rt`:
//!
//! * **swap queue** (editor → live): carries [`GraphCommand::Swap`] with a
//!   fully prepared `Box<AudioGraph>`.
//! * **recycle queue** (live → editor): carries the graph that was just
//!   replaced so it can be dropped (or mined for nodes) on the control
//!   thread. **Nothing is ever dropped on the audio thread.**
//!
//! The editor owns a *draft* [`AudioGraph`]. Structural edits (`add_node`,
//! `connect`, `remove_node`, ...) go to the draft through
//! [`GraphEditor::graph_mut`]; they mark the draft dirty exactly as they would
//! any graph. [`GraphEditor::publish`] then compiles the draft with
//! [`AudioGraph::prepare`] using the configuration given to
//! [`GraphEditor::configure`], moves it into the swap queue and leaves a fresh
//! empty draft behind. At the start of its next `process`, the [`LiveGraph`]
//! pops the command, exchanges its active graph for the incoming one with a
//! pointer swap, and pushes the retired graph back through the recycle queue.
//!
//! Dirty vs. published: the *draft* may be dirty for as long as you like; the
//! *active* graph is never dirty because only prepared graphs cross the
//! boundary. A published graph must have been prepared with the same sample
//! rate and block size the [`LiveGraph`] runs at; a mismatched graph is
//! rejected (recycled, never activated) and counted in
//! [`LiveGraph::rejected_count`] so the stream cannot start failing mid-flight.
//!
//! Queue sizing: the recycle queue always has one more slot than the swap
//! queue, and the editor drains it before every publish, so the audio-side
//! push can only fail if the control side stops calling `publish` or
//! [`GraphEditor::try_reclaim`]. Even then no graph is dropped: the retired
//! graph is parked in an `orphan` slot and no further swaps are applied until
//! it has been handed back.
//!
//! ```
//! use superb::primitives::SampleRate;
//! use superb::primitives::graph::nodes::ConstantNode;
//! use superb::primitives::graph::{live_pair, BlockTime};
//! use superb::primitives::midi::MidiEventBuffer;
//!
//! let (mut editor, mut live) = live_pair();
//! editor.configure(SampleRate::HZ_48K, 64);
//! let midi = MidiEventBuffer::with_capacity(0);
//!
//! // Build and publish the first graph.
//! let c = editor.graph_mut().add_node(Box::new(ConstantNode::new(0.25, 1)));
//! editor.graph_mut().set_output(c).unwrap();
//! editor.publish().unwrap();
//!
//! live.process(BlockTime::free_running(), 64, &midi).unwrap();
//! assert_eq!(live.output().channel(0)[0], 0.25);
//!
//! // Edit the (fresh) draft while the stream keeps running, then publish again.
//! let c = editor.graph_mut().add_node(Box::new(ConstantNode::new(0.5, 1)));
//! editor.graph_mut().set_output(c).unwrap();
//! editor.publish().unwrap();
//!
//! live.process(BlockTime::free_running(), 64, &midi).unwrap();
//! assert_eq!(live.output().channel(0)[0], 0.5);
//! assert_eq!(live.swap_count(), 2);
//! ```

use core::mem;
use core::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::midi::MidiEventBuffer;
use crate::primitives::realtime::spsc::{channel, Consumer, Producer};
use crate::primitives::realtime::thread::debug_assert_not_rt;
use crate::primitives::{Result, SampleRate, SuperbError};

use crate::primitives::graph::{AudioGraph, BlockProcessor, BlockTime};

/// Default swap-queue capacity used by [`live_pair`].
pub const DEFAULT_SWAP_CAPACITY: usize = 1;

/// Commands that cross the control → audio boundary.
///
/// Only [`Swap`](GraphCommand::Swap) exists today: granular edits are applied
/// to the editor's draft off-thread, and the audio thread only ever receives
/// whole, prepared graphs. Each variant must be applicable with pointer moves
/// only (no allocation, no drop of heap data) on the audio thread.
pub enum GraphCommand {
    /// Replace the active graph with this prepared graph at the next `process`.
    Swap(Box<AudioGraph>),
}

impl core::fmt::Debug for GraphCommand {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            GraphCommand::Swap(g) => f.debug_tuple("Swap").field(&g.node_count()).finish(),
        }
    }
}

/// Counters shared between the two halves (lock-free, monotonically increasing).
#[derive(Default)]
struct Counters {
    /// Swaps applied by the audio thread.
    applied: AtomicU64,
    /// Swaps rejected by the audio thread (configuration mismatch).
    rejected: AtomicU64,
}

/// Control-thread half: owns a draft [`AudioGraph`] and publishes prepared swaps.
///
/// See the [module docs](self) for the protocol. All methods allocate freely
/// and call `debug_assert_not_rt` where it matters; never use this type from
/// the audio callback.
pub struct GraphEditor {
    draft: AudioGraph,
    config: Option<(SampleRate, usize)>,
    swaps: Producer<GraphCommand>,
    retired: Consumer<Box<AudioGraph>>,
    counters: Arc<Counters>,
    published: u64,
    reclaimed: u64,
}

impl GraphEditor {
    /// Stores the configuration every published graph is prepared with.
    ///
    /// Must match the configuration the paired [`LiveGraph`] was (or will be)
    /// prepared with; otherwise published graphs are rejected on arrival.
    pub fn configure(&mut self, sample_rate: SampleRate, max_frames: usize) {
        self.config = Some((sample_rate, max_frames));
    }

    /// The configuration set by [`configure`](GraphEditor::configure).
    pub fn config(&self) -> Option<(SampleRate, usize)> {
        self.config
    }

    /// The draft graph (edits go here).
    pub fn graph(&self) -> &AudioGraph {
        &self.draft
    }

    /// Mutable access to the draft graph.
    pub fn graph_mut(&mut self) -> &mut AudioGraph {
        &mut self.draft
    }

    /// Replaces the draft wholesale, returning the previous draft.
    ///
    /// Useful to continue editing a graph recovered with
    /// [`take_retired`](GraphEditor::take_retired).
    pub fn replace_draft(&mut self, graph: AudioGraph) -> AudioGraph {
        mem::replace(&mut self.draft, graph)
    }

    /// Prepares the draft and hands it to the audio thread.
    ///
    /// Steps: reclaim (drop) any graphs the audio thread has retired, prepare
    /// the draft with the configured sample rate / block size, move it into
    /// the swap queue and reset the draft to an empty graph. On error the
    /// draft is left untouched (still dirty) so the caller can fix and retry.
    ///
    /// Errors: [`SuperbError::InvalidArgument`] if
    /// [`configure`](GraphEditor::configure) was never called,
    /// [`SuperbError::CapacityExceeded`] if the previous publish has not been
    /// consumed yet (see [`is_busy`](GraphEditor::is_busy)), plus anything
    /// [`AudioGraph::prepare`] can return.
    pub fn publish(&mut self) -> Result<()> {
        debug_assert_not_rt("GraphEditor::publish");
        self.try_reclaim();
        let (sample_rate, max_frames) = self.config.ok_or(SuperbError::InvalidArgument {
            what: "GraphEditor::configure was not called",
        })?;
        if self.swaps.is_full() {
            return Err(SuperbError::CapacityExceeded {
                what: "graph swap queue",
                capacity: self.swaps.capacity(),
            });
        }
        self.draft.prepare(sample_rate, max_frames)?;
        let ready = mem::take(&mut self.draft);
        match self.swaps.push(GraphCommand::Swap(Box::new(ready))) {
            Ok(()) => {
                self.published += 1;
                Ok(())
            }
            Err(GraphCommand::Swap(back)) => {
                self.draft = *back;
                Err(SuperbError::CapacityExceeded {
                    what: "graph swap queue",
                    capacity: self.swaps.capacity(),
                })
            }
        }
    }

    /// Drops every graph the audio thread has retired so far. Returns how many.
    ///
    /// `publish` does this implicitly; call it directly from a UI tick if you
    /// publish rarely and want memory back sooner.
    pub fn try_reclaim(&mut self) -> usize {
        debug_assert_not_rt("GraphEditor::try_reclaim");
        let n = self.retired.drain(usize::MAX, drop);
        self.reclaimed += n as u64;
        n
    }

    /// Takes the oldest retired graph instead of dropping it, e.g. to pull
    /// nodes out of it with [`AudioGraph::remove_node`] and reuse them.
    pub fn take_retired(&mut self) -> Option<Box<AudioGraph>> {
        debug_assert_not_rt("GraphEditor::take_retired");
        let g = self.retired.pop();
        if g.is_some() {
            self.reclaimed += 1;
        }
        g
    }

    /// `true` if the swap queue is full, i.e. `publish` would fail right now.
    pub fn is_busy(&self) -> bool {
        self.swaps.is_full()
    }

    /// `true` if the paired [`LiveGraph`] has been dropped.
    pub fn is_detached(&self) -> bool {
        self.swaps.is_abandoned()
    }

    /// Number of successful `publish` calls.
    pub fn published_count(&self) -> u64 {
        self.published
    }

    /// Number of swaps the audio thread has applied.
    pub fn applied_count(&self) -> u64 {
        self.counters.applied.load(Ordering::Acquire)
    }

    /// Number of swaps the audio thread rejected because the incoming graph
    /// was prepared with a different configuration than the live graph.
    pub fn rejected_count(&self) -> u64 {
        self.counters.rejected.load(Ordering::Acquire)
    }

    /// Published graphs not yet applied or rejected.
    pub fn pending_count(&self) -> u64 {
        self.published
            .saturating_sub(self.applied_count() + self.rejected_count())
    }

    /// Retired graphs dropped or taken back so far.
    pub fn reclaimed_count(&self) -> u64 {
        self.reclaimed
    }
}

impl core::fmt::Debug for GraphEditor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GraphEditor")
            .field("draft_nodes", &self.draft.node_count())
            .field("config", &self.config)
            .field("published", &self.published)
            .field("applied", &self.applied_count())
            .field("rejected", &self.rejected_count())
            .field("reclaimed", &self.reclaimed)
            .finish()
    }
}

/// Audio-thread half: applies pending swaps, then processes the active graph.
///
/// Implements [`BlockProcessor`], so it can drive `superb-io`'s `Engine`
/// directly. Starts with an empty, unprepared graph: `process` returns
/// [`SuperbError::NotPrepared`] until either [`prepare`](LiveGraph::prepare)
/// is called or the first published graph arrives.
pub struct LiveGraph {
    active: Box<AudioGraph>,
    commands: Consumer<GraphCommand>,
    recycle: Producer<Box<AudioGraph>>,
    /// A retired graph whose recycle push failed; retried before any new swap.
    orphan: Option<Box<AudioGraph>>,
    config: Option<(SampleRate, usize)>,
    counters: Arc<Counters>,
    swap_count: u64,
    rejected_count: u64,
}

impl LiveGraph {
    /// Applies pending swap commands. Called automatically by `process`.
    ///
    /// Returns `true` if the active graph changed. Wait-free and
    /// allocation-free: each swap is two pointer moves and two queue
    /// operations. If several swaps are queued they are applied in order, so
    /// the newest graph ends up active and every intermediate one is recycled.
    /// Never drops a graph: if the recycle queue is full the retired graph is
    /// parked and further swaps wait until it has been handed back.
    pub fn apply_pending(&mut self) -> bool {
        if let Some(old) = self.orphan.take() {
            if let Err(back) = self.recycle.push(old) {
                self.orphan = Some(back);
                return false;
            }
        }
        let mut changed = false;
        while let Some(GraphCommand::Swap(mut incoming)) = self.commands.pop() {
            let accepted = match self.config {
                None => incoming.prepared_config().is_some(),
                Some(cfg) => incoming.prepared_config() == Some(cfg),
            };
            if accepted {
                if self.config.is_none() {
                    self.config = incoming.prepared_config();
                }
                mem::swap(&mut self.active, &mut incoming);
                self.swap_count += 1;
                self.counters.applied.fetch_add(1, Ordering::Release);
                changed = true;
            } else {
                self.rejected_count += 1;
                self.counters.rejected.fetch_add(1, Ordering::Release);
            }
            // `incoming` now holds the graph to retire (old active, or the rejected one).
            if let Err(back) = self.recycle.push(incoming) {
                self.orphan = Some(back);
                break;
            }
        }
        changed
    }

    /// Prepares the active graph and fixes the configuration published
    /// graphs must match. Off the audio thread.
    ///
    /// Any swaps already queued are applied first (whatever their
    /// configuration) so the newest graph is the one that gets prepared.
    pub fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        debug_assert_not_rt("LiveGraph::prepare");
        self.config = None;
        self.apply_pending();
        self.active.prepare(sample_rate, max_frames)?;
        self.config = Some((sample_rate, max_frames));
        Ok(())
    }

    /// Applies pending swaps, then processes the active graph. Allocation-free.
    pub fn process(
        &mut self,
        time: BlockTime,
        frames: usize,
        midi: &MidiEventBuffer,
    ) -> Result<()> {
        self.apply_pending();
        self.active.process(time, frames, midi)
    }

    /// The active graph's output from the last `process` call.
    pub fn output(&self) -> &PlanarBuffer {
        self.active.output()
    }

    /// Resets every node of the active graph without reallocating.
    pub fn reset(&mut self) {
        self.active.reset();
    }

    /// The active graph. Only meaningful on the thread that calls `process`.
    pub fn graph(&self) -> &AudioGraph {
        &self.active
    }

    /// Mutable access to the active graph, e.g. for fixture setters in tests.
    ///
    /// Structural edits here would dirty the active graph and make `process`
    /// fail; do them on the editor's draft instead.
    pub fn graph_mut(&mut self) -> &mut AudioGraph {
        &mut self.active
    }

    /// Number of swaps applied so far.
    pub fn swap_count(&self) -> u64 {
        self.swap_count
    }

    /// Number of published graphs rejected for a configuration mismatch.
    pub fn rejected_count(&self) -> u64 {
        self.rejected_count
    }

    /// `true` if a swap is queued and will be applied by the next `process`.
    pub fn has_pending(&self) -> bool {
        !self.commands.is_empty() || self.orphan.is_some()
    }

    /// Node count of the active graph.
    pub fn active_node_count(&self) -> usize {
        self.active.node_count()
    }

    /// Configuration incoming graphs must match (set by `prepare` or adopted
    /// from the first accepted swap).
    pub fn prepared_config(&self) -> Option<(SampleRate, usize)> {
        self.config
    }

    /// `true` if the paired [`GraphEditor`] has been dropped.
    pub fn is_detached(&self) -> bool {
        self.commands.is_abandoned()
    }
}

impl BlockProcessor for LiveGraph {
    fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        LiveGraph::prepare(self, sample_rate, max_frames)
    }

    fn process(&mut self, time: BlockTime, frames: usize, midi: &MidiEventBuffer) -> Result<()> {
        LiveGraph::process(self, time, frames, midi)
    }

    fn output(&self) -> &PlanarBuffer {
        LiveGraph::output(self)
    }

    fn output_channels(&self) -> usize {
        BlockProcessor::output_channels(&*self.active)
    }

    fn reset(&mut self) {
        LiveGraph::reset(self)
    }

    fn latency_frames(&self) -> usize {
        self.active.latency_frames()
    }
}

impl core::fmt::Debug for LiveGraph {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LiveGraph")
            .field("active_nodes", &self.active.node_count())
            .field("config", &self.config)
            .field("swap_count", &self.swap_count)
            .field("rejected_count", &self.rejected_count)
            .field("pending", &self.commands.len())
            .field("orphan", &self.orphan.is_some())
            .finish()
    }
}

/// Creates a connected editor + live processor with a swap queue of
/// [`DEFAULT_SWAP_CAPACITY`] (one publish in flight at a time).
pub fn live_pair() -> (GraphEditor, LiveGraph) {
    live_pair_with_capacity(DEFAULT_SWAP_CAPACITY)
}

/// Creates a connected editor + live processor whose swap queue holds
/// `swap_capacity` publishes at once. The recycle queue gets one extra slot.
///
/// # Panics
/// Panics if `swap_capacity == 0`.
pub fn live_pair_with_capacity(swap_capacity: usize) -> (GraphEditor, LiveGraph) {
    assert!(swap_capacity > 0, "swap capacity must be at least 1");
    let (swap_tx, swap_rx) = channel::<GraphCommand>(swap_capacity);
    let (recycle_tx, recycle_rx) = channel::<Box<AudioGraph>>(swap_capacity + 1);
    let counters = Arc::new(Counters::default());
    let editor = GraphEditor {
        draft: AudioGraph::new(),
        config: None,
        swaps: swap_tx,
        retired: recycle_rx,
        counters: Arc::clone(&counters),
        published: 0,
        reclaimed: 0,
    };
    let live = LiveGraph {
        active: Box::new(AudioGraph::new()),
        commands: swap_rx,
        recycle: recycle_tx,
        orphan: None,
        config: None,
        counters,
        swap_count: 0,
        rejected_count: 0,
    };
    (editor, live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::nodes::{ConstantNode, GainNode, PassThroughNode, SineNode};
    use crate::primitives::graph::{Node, NodeChannels, ProcessContext, ProcessStatus};
    use crate::primitives::realtime::thread::{is_rt_thread, with_rt};
    use core::sync::atomic::{AtomicBool, AtomicUsize};

    const R: SampleRate = SampleRate::HZ_48K;

    fn no_midi() -> MidiEventBuffer {
        MidiEventBuffer::with_capacity(0)
    }

    fn run(live: &mut LiveGraph, frames: usize) {
        live.process(BlockTime::free_running(), frames, &no_midi())
            .unwrap();
    }

    fn publish_constant(editor: &mut GraphEditor, value: f32) {
        let c = editor
            .graph_mut()
            .add_node(Box::new(ConstantNode::new(value, 1)));
        editor.graph_mut().set_output(c).unwrap();
        editor.publish().unwrap();
    }

    /// Records where it is dropped so tests can prove drops stay off the RT thread.
    struct DropProbe {
        drops: Arc<AtomicUsize>,
        dropped_on_rt: Arc<AtomicBool>,
    }

    impl Node for DropProbe {
        fn channels(&self) -> NodeChannels {
            NodeChannels::generator(1)
        }
        fn process(
            &mut self,
            _: &ProcessContext<'_>,
            _: &PlanarBuffer,
            out: &mut PlanarBuffer,
        ) -> ProcessStatus {
            out.fill(1.0);
            ProcessStatus::Normal
        }
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
            if is_rt_thread() {
                self.dropped_on_rt.store(true, Ordering::SeqCst);
            }
        }
    }

    #[test]
    fn publish_then_process_swaps_and_changes_output() {
        let (mut editor, mut live) = live_pair();
        editor.configure(R, 64);
        assert_eq!(
            live.process(BlockTime::free_running(), 64, &no_midi()),
            Err(SuperbError::NotPrepared)
        );

        publish_constant(&mut editor, 0.25);
        assert!(live.has_pending());
        assert_eq!(editor.pending_count(), 1);
        run(&mut live, 64);
        assert_eq!(live.output().channel(0), &[0.25; 64]);
        assert_eq!(live.swap_count(), 1);
        assert_eq!(live.active_node_count(), 1);
        assert_eq!(live.prepared_config(), Some((R, 64)));
        assert_eq!(editor.applied_count(), 1);
        assert_eq!(editor.pending_count(), 0);
        assert!(!live.has_pending());

        publish_constant(&mut editor, 0.5);
        run(&mut live, 64);
        assert_eq!(live.output().channel(0), &[0.5; 64]);
        assert_eq!(live.swap_count(), 2);
        // The draft is fresh after each publish.
        assert_eq!(editor.graph().node_count(), 0);
        assert!(editor.graph().is_dirty());
    }

    #[test]
    fn edits_land_mid_stream_without_stopping() {
        let (mut editor, mut live) = live_pair();
        editor.configure(R, 32);
        let s = editor
            .graph_mut()
            .add_node(Box::new(SineNode::new(1_000.0, 1.0, 1)));
        let g = editor.graph_mut().add_node(Box::new(GainNode::new(0.5, 1)));
        editor.graph_mut().connect(s, g).unwrap();
        editor.graph_mut().set_output(g).unwrap();
        editor.publish().unwrap();

        let mut peaks = Vec::new();
        for block in 0..40 {
            if block == 20 {
                // Structural edit while the "stream" is running: a new chain
                // with a different gain, no stop/prepare on the live side.
                let s = editor
                    .graph_mut()
                    .add_node(Box::new(SineNode::new(1_000.0, 1.0, 1)));
                let g = editor.graph_mut().add_node(Box::new(GainNode::new(0.1, 1)));
                let p = editor
                    .graph_mut()
                    .add_node(Box::new(PassThroughNode::new(1)));
                editor.graph_mut().connect(s, g).unwrap();
                editor.graph_mut().connect(g, p).unwrap();
                editor.graph_mut().set_output(p).unwrap();
                editor.publish().unwrap();
            }
            run(&mut live, 32);
            peaks.push(live.output().peak());
        }
        let before = peaks[..20].iter().cloned().fold(0.0f32, f32::max);
        let after = peaks[25..].iter().cloned().fold(0.0f32, f32::max);
        assert!(before > 0.4 && before <= 0.5 + 1e-5, "before {before}");
        assert!(after > 0.08 && after <= 0.1 + 1e-5, "after {after}");
        assert_eq!(live.swap_count(), 2);
        assert_eq!(live.active_node_count(), 3);
    }

    #[test]
    fn publish_requires_configure_and_prepares_the_draft() {
        let (mut editor, mut live) = live_pair();
        let c = editor
            .graph_mut()
            .add_node(Box::new(ConstantNode::new(1.0, 1)));
        editor.graph_mut().set_output(c).unwrap();
        assert!(matches!(
            editor.publish(),
            Err(SuperbError::InvalidArgument { .. })
        ));
        // Draft untouched by the failed publish.
        assert_eq!(editor.graph().node_count(), 1);
        assert!(editor.graph().is_dirty());
        assert_eq!(editor.published_count(), 0);

        editor.configure(R, 16);
        assert_eq!(editor.config(), Some((R, 16)));
        editor.publish().unwrap();
        assert_eq!(editor.published_count(), 1);
        run(&mut live, 16);
        assert!(!live.graph().is_dirty());
        assert_eq!(live.graph().prepared_config(), Some((R, 16)));
        assert_eq!(live.output().channel(0), &[1.0; 16]);

        // A prepare failure (max_frames == 0) leaves the draft intact.
        editor.configure(R, 0);
        let c = editor
            .graph_mut()
            .add_node(Box::new(ConstantNode::new(1.0, 1)));
        editor.graph_mut().set_output(c).unwrap();
        assert!(matches!(
            editor.publish(),
            Err(SuperbError::InvalidArgument { .. })
        ));
        assert_eq!(editor.graph().node_count(), 1);
    }

    #[test]
    fn full_swap_queue_is_reported_and_draft_is_kept() {
        let (mut editor, mut live) = live_pair();
        editor.configure(R, 8);
        publish_constant(&mut editor, 0.1);
        assert!(editor.is_busy());
        let c = editor
            .graph_mut()
            .add_node(Box::new(ConstantNode::new(0.2, 1)));
        editor.graph_mut().set_output(c).unwrap();
        assert!(matches!(
            editor.publish(),
            Err(SuperbError::CapacityExceeded { .. })
        ));
        assert_eq!(editor.graph().node_count(), 1);
        assert_eq!(editor.published_count(), 1);
        // Once the audio thread consumes the swap, the retry succeeds.
        run(&mut live, 8);
        assert!(!editor.is_busy());
        editor.publish().unwrap();
        run(&mut live, 8);
        assert!((live.output().channel(0)[0] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn larger_capacity_applies_newest_and_recycles_intermediates() {
        let (mut editor, mut live) = live_pair_with_capacity(3);
        editor.configure(R, 8);
        publish_constant(&mut editor, 0.1);
        publish_constant(&mut editor, 0.2);
        publish_constant(&mut editor, 0.3);
        assert!(editor.is_busy());
        run(&mut live, 8);
        assert_eq!(live.swap_count(), 3);
        assert!((live.output().channel(0)[0] - 0.3).abs() < 1e-6);
        // Three retired graphs (empty initial + 0.1 + 0.2) come back.
        assert_eq!(editor.try_reclaim(), 3);
        assert_eq!(editor.reclaimed_count(), 3);
    }

    #[test]
    fn retired_graphs_are_dropped_on_the_control_thread_only() {
        let drops = Arc::new(AtomicUsize::new(0));
        let on_rt = Arc::new(AtomicBool::new(false));
        let probe = || {
            Box::new(DropProbe {
                drops: Arc::clone(&drops),
                dropped_on_rt: Arc::clone(&on_rt),
            })
        };

        let (mut editor, mut live) = live_pair();
        editor.configure(R, 8);
        let p = editor.graph_mut().add_node(probe());
        editor.graph_mut().set_output(p).unwrap();
        editor.publish().unwrap();
        with_rt(|| run(&mut live, 8));
        assert_eq!(live.output().channel(0), &[1.0; 8]);

        // Replace it; the probe graph is retired but must not be dropped on the RT thread.
        publish_constant(&mut editor, 0.5);
        with_rt(|| run(&mut live, 8));
        assert_eq!(drops.load(Ordering::SeqCst), 0);

        // Second publish reclaims and drops it here, on the control thread.
        publish_constant(&mut editor, 0.75);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(!on_rt.load(Ordering::SeqCst));
        with_rt(|| run(&mut live, 8));
        assert_eq!(live.output().channel(0), &[0.75; 8]);

        // take_retired hands the graph back instead of dropping it.
        let p = editor.graph_mut().add_node(probe());
        editor.graph_mut().set_output(p).unwrap();
        editor.publish().unwrap();
        with_rt(|| run(&mut live, 8));
        publish_constant(&mut editor, 0.0);
        with_rt(|| run(&mut live, 8));
        let mut retired = editor.take_retired().expect("retired probe graph");
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let node = retired.remove_node(p).unwrap();
        drop(retired);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        // Reuse the node in a new draft.
        let id = editor.graph_mut().add_node(node);
        editor.graph_mut().set_output(id).unwrap();
        editor.publish().unwrap();
        with_rt(|| run(&mut live, 8));
        assert_eq!(live.output().channel(0), &[1.0; 8]);
        drop(editor);
        drop(live);
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(!on_rt.load(Ordering::SeqCst));
    }

    #[test]
    fn mismatched_configuration_is_rejected_not_activated() {
        let (mut editor, mut live) = live_pair();
        live.prepare(R, 64).unwrap();
        run(&mut live, 64); // empty but prepared graph processes fine
        assert_eq!(live.output().channels(), 0);

        editor.configure(SampleRate::CD, 64);
        publish_constant(&mut editor, 0.9);
        run(&mut live, 64);
        assert_eq!(live.swap_count(), 0);
        assert_eq!(live.rejected_count(), 1);
        assert_eq!(editor.rejected_count(), 1);
        assert_eq!(editor.pending_count(), 0);
        assert_eq!(live.output().channels(), 0);
        assert_eq!(editor.try_reclaim(), 1);

        editor.configure(R, 64);
        publish_constant(&mut editor, 0.9);
        run(&mut live, 64);
        assert_eq!(live.swap_count(), 1);
        assert_eq!(live.output().channel(0)[0], 0.9);
    }

    #[test]
    fn live_prepare_adopts_pending_graph_and_reconfigures() {
        let (mut editor, mut live) = live_pair();
        editor.configure(R, 16);
        publish_constant(&mut editor, 0.3);
        // Engine-style prepare with a different block size: the pending graph
        // is applied and re-prepared rather than rejected.
        live.prepare(R, 256).unwrap();
        assert_eq!(live.swap_count(), 1);
        assert_eq!(live.prepared_config(), Some((R, 256)));
        run(&mut live, 256);
        assert_eq!(live.output().frames(), 256);
        assert_eq!(live.output().channel(0)[0], 0.3);
        // Subsequent publishes must use the new configuration.
        publish_constant(&mut editor, 0.6);
        run(&mut live, 256);
        assert_eq!(live.rejected_count(), 1);
        editor.configure(R, 256);
        publish_constant(&mut editor, 0.6);
        run(&mut live, 256);
        assert_eq!(live.output().channel(0)[0], 0.6);
        live.reset();
        assert_eq!(BlockProcessor::output_channels(&live), 1);
    }

    #[test]
    fn draft_still_rejects_cycles_and_bad_edges() {
        let (mut editor, _live) = live_pair();
        let a = editor
            .graph_mut()
            .add_node(Box::new(PassThroughNode::new(1)));
        let b = editor
            .graph_mut()
            .add_node(Box::new(PassThroughNode::new(1)));
        editor.graph_mut().connect(a, b).unwrap();
        assert_eq!(editor.graph_mut().connect(b, a), Err(SuperbError::Cycle));
        assert!(editor.graph_mut().connect(a, b).is_err());
        let mut fresh = AudioGraph::new();
        let c = fresh.add_node(Box::new(ConstantNode::new(1.0, 1)));
        fresh.set_output(c).unwrap();
        let old = editor.replace_draft(fresh);
        assert_eq!(old.node_count(), 2);
        assert_eq!(editor.graph().node_count(), 1);
    }

    #[test]
    fn concurrent_publish_while_audio_thread_runs() {
        let (mut editor, mut live) = live_pair();
        editor.configure(R, 64);
        publish_constant(&mut editor, 0.25);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_rt = Arc::clone(&stop);
        let audio = std::thread::spawn(move || {
            with_rt(|| {
                let midi = no_midi();
                let mut seen_first = false;
                let mut seen_second = false;
                let mut blocks = 0u64;
                while !stop_rt.load(Ordering::Acquire) {
                    live.process(BlockTime::free_running(), 64, &midi).unwrap();
                    blocks += 1;
                    let v = live.output().channel(0)[0];
                    if (v - 0.25).abs() < 1e-6 {
                        assert!(!seen_second, "output went backwards");
                        seen_first = true;
                    } else if (v - 0.75).abs() < 1e-6 {
                        seen_second = true;
                    } else {
                        panic!("unexpected sample {v}");
                    }
                    std::hint::spin_loop();
                }
                (live, seen_first, seen_second, blocks)
            })
        });

        // Wait for the first swap to be applied, then publish a second graph.
        while editor.applied_count() < 1 {
            std::thread::yield_now();
        }
        publish_constant(&mut editor, 0.75);
        while editor.applied_count() < 2 {
            std::thread::yield_now();
        }
        stop.store(true, Ordering::Release);
        let (live, seen_first, seen_second, blocks) = audio.join().unwrap();
        assert!(seen_first && seen_second, "blocks {blocks}");
        assert_eq!(live.swap_count(), 2);
        // The second publish already reclaimed the initial empty graph; the
        // retired 0.25 graph is still waiting.
        assert_eq!(editor.try_reclaim(), 1);
        assert_eq!(editor.reclaimed_count(), 2);
        assert!(!editor.is_detached());
        drop(live);
        assert!(editor.is_detached());
    }

    #[test]
    fn detached_live_is_detectable() {
        let (editor, live) = live_pair();
        assert!(!live.is_detached());
        drop(editor);
        assert!(live.is_detached());
        let _ = format!("{live:?}");
    }

    #[test]
    #[should_panic(expected = "swap capacity")]
    fn zero_capacity_is_rejected() {
        let _ = live_pair_with_capacity(0);
    }

    #[test]
    fn debug_impls() {
        let (mut editor, live) = live_pair();
        editor.configure(R, 8);
        let s = format!("{editor:?} {live:?}");
        assert!(s.contains("GraphEditor") && s.contains("LiveGraph"));
        let cmd = GraphCommand::Swap(Box::default());
        assert!(format!("{cmd:?}").starts_with("Swap"));
    }
}
