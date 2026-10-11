//! The dynamics nodes process without heap activity, parameters moving.

use std::sync::Arc;

use aura::primitives::buffers::PlanarBuffer;
use aura::primitives::graph::dynamics::{
    Compressor, CompressorParams, DeEsser, DeEsserParams, Gate, GateParams, Leveler, LevelerParams,
    Limiter, LimiterParams,
};
use aura::primitives::graph::nodes::SineNode;
use aura::primitives::graph::{AudioGraph, BlockTime};
use aura::primitives::midi::MidiEventBuffer;
use aura::primitives::realtime::CountingAllocator;
use aura::primitives::SampleRate;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::system();

#[test]
fn a_chain_of_dynamics_processes_without_allocating() {
    let comp = Arc::new(CompressorParams::new(-18.0, 3.0));
    let lim = Arc::new(LimiterParams::new(-1.0));
    let gate = Arc::new(GateParams::new(-50.0));
    let ess = Arc::new(DeEsserParams::new(6_500.0, -24.0));
    let ride = Arc::new(LevelerParams::new(-18.0));

    let mut graph = AudioGraph::new();
    let source = graph.add_node(Box::new(SineNode::new(440.0, 0.8, 2)));
    let g = graph.add_node(Box::new(Gate::new(2, Arc::clone(&gate))));
    let d = graph.add_node(Box::new(DeEsser::new(2, Arc::clone(&ess))));
    let c = graph.add_node(Box::new(Compressor::new(2, Arc::clone(&comp))));
    let r = graph.add_node(Box::new(Leveler::new(2, Arc::clone(&ride))));
    let l = graph.add_node(Box::new(Limiter::new(2, Arc::clone(&lim))));
    for (from, to) in [(source, g), (g, d), (d, c), (c, r), (r, l)] {
        graph.connect(from, to).unwrap();
    }
    graph.set_output(l).unwrap();
    graph.prepare(SampleRate::HZ_48K, 256).unwrap();
    let midi = MidiEventBuffer::with_capacity(16);
    // Warm-up block.
    graph
        .process(BlockTime::default(), 256, &midi, None)
        .unwrap();

    aura::primitives::realtime::assert_no_alloc(|| {
        for i in 0..96u32 {
            // Parameters move while it runs, including the de-esser's
            // filter frequency, which is redesigned in place.
            comp.set_threshold_db(-18.0 - (i % 6) as f32);
            lim.set_ceiling_db(-1.0 - (i % 3) as f32);
            gate.set_threshold_db(-50.0 + (i % 4) as f32);
            ess.set_frequency_hz(5_000.0 + 500.0 * (i % 4) as f32);
            ride.set_target_db(-18.0 + (i % 2) as f32);
            let frames = if i % 2 == 0 { 256 } else { 200 };
            graph
                .process(BlockTime::default(), frames, &midi, None)
                .unwrap();
            let _ = (comp.reduction_db(), lim.reduction_db(), ride.gain_db());
        }
    });
    let out: &PlanarBuffer = graph.output();
    assert!(out.peak() > 0.0, "it passes audio");
}
