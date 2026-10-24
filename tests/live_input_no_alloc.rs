//! A running engine moves captured audio into the graph without heap activity.

use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::graph::nodes::LiveInputNode;
use superb::primitives::graph::AudioGraph;
use superb::primitives::io::capture::capture_queue;
use superb::primitives::io::{Engine, LiveInput};
use superb::primitives::realtime::CountingAllocator;
use superb::primitives::transport::Transport;
use superb::primitives::SampleRate;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::system();

#[test]
fn rendering_with_a_live_input_is_allocation_free() {
    let mut graph = AudioGraph::new();
    let tap = graph.add_node(Box::new(LiveInputNode::new([0, 1])));
    graph.set_output(tap).unwrap();
    let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
    engine.prepare(SampleRate::HZ_48K, 256).unwrap();
    let (mut mic, blocks) = capture_queue(2, 256, 8).unwrap();
    engine.set_live_input(Some(LiveInput::new(blocks, 2, 256).unwrap()));
    engine.transport_mut().play();
    let mut block = PlanarBuffer::new(2, 256);
    block.fill(0.25);
    let mut out = PlanarBuffer::new(2, 256);
    assert!(mic.push(&block, 0));
    // Warm-up block.
    engine.render(&mut out).unwrap();

    superb::primitives::realtime::assert_no_alloc(|| {
        for i in 0..64u64 {
            // Sometimes two blocks arrive, sometimes none: the ring absorbs
            // both, dropping the oldest frames or filling with silence.
            for _ in 0..(i % 3) {
                let _ = mic.push(&block, i);
            }
            let frames = if i % 2 == 0 { 256 } else { 200 };
            out.set_frames(frames).unwrap();
            engine.render(&mut out).unwrap();
            assert_eq!(out.frames(), frames);
        }
    });
    let live = engine.live_input().unwrap();
    assert!(
        live.overruns() + live.underruns() > 0,
        "the ring was exercised both ways"
    );
}
