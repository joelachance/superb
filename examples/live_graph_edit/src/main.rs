//! Edits a running audio graph without stopping it.
//!
//! A `LiveGraph` renders blocks in a loop (standing in for a device
//! callback) while a `GraphEditor` builds new graphs off-thread and publishes
//! them. Each publish is applied between two blocks with a pointer swap; the
//! retired graph travels back to the editor and is dropped there.
//!
//! What you'll learn:
//! - `live_pair()`: control-thread editor vs RT-ish live runner
//! - Building graphs with `Box::new` nodes, `connect`, `set_output`
//! - A local closure (`render`) that borrows `&mut LiveGraph`
//! - Why retired graphs must be reclaimed on the control thread
//!
//! ```text
//! cargo run -p live_graph_edit
//! ```

use superb::graph::live_pair;
use superb::prelude::*;

fn main() -> Result<()> {
    let sample_rate = SampleRate::HZ_48K;
    let block = 256;
    // Empty MIDI buffer — this demo is audio-only.
    let midi = MidiEventBuffer::with_capacity(0);

    // live_pair() → (GraphEditor, LiveGraph) linked by lock-free queues.
    let (mut editor, mut live) = live_pair();
    // Tell both sides the rate/block size before the first publish.
    editor.configure(sample_rate, block);

    // Generation 1: 440 Hz sine at full scale through a -6 dB gain (0.5 linear).
    let sine = editor.graph_mut().add_node(Box::new(SineNode::new(440.0, 1.0, 2)));
    let gain = editor.graph_mut().add_node(Box::new(GainNode::new(0.5, 2)));
    editor.graph_mut().connect(sine, gain)?;
    editor.graph_mut().set_output(gain)?;
    // publish() queues a Swap for the live side; applied between blocks.
    editor.publish()?;
    println!("published generation 1 (sine -> gain 0.5)");

    // BlockTime tracks playhead / rolling state for each process call.
    // Struct update: copy free_running() defaults, then set rolling: true.
    let mut time = BlockTime { rolling: true, ..BlockTime::free_running() };

    // Closure that mutably borrows the live graph and runs N blocks.
    // `|live: &mut LiveGraph, ...|` is how we pass exclusive access in.
    let mut render = |live: &mut LiveGraph, label: &str, blocks: usize| -> Result<()> {
        let mut peak = 0.0f32;
        for _ in 0..blocks {
            live.process(time, block, &midi)?;
            time = time.advanced_by(block, sample_rate);
            // output() is the PlanarBuffer from the last process; peak() = max |sample|.
            peak = peak.max(live.output().peak());
        }
        println!("{label}: {blocks} blocks, peak {peak:.3}, active nodes {}, swaps {}", live.active_node_count(), live.swap_count());
        Ok(())
    };

    render(&mut live, "gen 1 running   ", 20)?;

    // Generation 2 is built while generation 1 keeps rendering: a second
    // oscillator, a quieter gain and a pass-through "bus" node.
    let sine_a = editor.graph_mut().add_node(Box::new(SineNode::new(440.0, 1.0, 2)));
    let sine_b = editor.graph_mut().add_node(Box::new(SineNode::new(660.0, 1.0, 2)));
    let gain = editor.graph_mut().add_node(Box::new(GainNode::new(0.125, 2)));
    let bus = editor.graph_mut().add_node(Box::new(PassThroughNode::new(2)));
    // Two sources into one gain: the graph mixes those inputs.
    editor.graph_mut().connect(sine_a, gain)?;
    editor.graph_mut().connect(sine_b, gain)?;
    editor.graph_mut().connect(gain, bus)?;
    editor.graph_mut().set_output(bus)?;
    // Keep rendering gen 1 while the draft for gen 2 sits unpublished.
    render(&mut live, "gen 1 while edit", 10)?;
    editor.publish()?;
    println!("published generation 2 (two sines -> gain 0.125 -> bus); pending: {}", editor.pending_count());

    render(&mut live, "gen 2 running   ", 20)?;

    // Generation 3: a constant DC value, published while gen 2 runs.
    let c = editor.graph_mut().add_node(Box::new(superb::graph::nodes::ConstantNode::new(0.75, 2)));
    editor.graph_mut().set_output(c)?;
    editor.publish()?;
    println!("published generation 3 (constant 0.75)");
    render(&mut live, "gen 3 running   ", 10)?;

    // Retired graphs come back to the control thread; drop them here
    // (never free graph memory on the audio/live side).
    let reclaimed = editor.try_reclaim();
    println!("reclaimed {reclaimed} retired graph(s) on the control thread; applied {}, rejected {}", editor.applied_count(), editor.rejected_count());
    Ok(())
}
