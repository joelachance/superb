//! Proves that `LiveGraph::process` performs no heap operations, both when no
//! swap is pending and when a published graph is swapped in.

use superb_core::SampleRate;
use superb_graph::nodes::{ConstantNode, GainNode, SineNode};
use superb_graph::{live_pair, BlockTime, GraphEditor};
use superb_midi::MidiEventBuffer;
use superb_rt::thread::with_rt;
use superb_rt::CountingAllocator;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::system();

const R: SampleRate = SampleRate::HZ_48K;

fn publish_sine_chain(editor: &mut GraphEditor, gain: f32) {
    let s = editor
        .graph_mut()
        .add_node(Box::new(SineNode::new(440.0, 1.0, 2)));
    let g = editor
        .graph_mut()
        .add_node(Box::new(GainNode::new(gain, 2)));
    editor.graph_mut().connect(s, g).unwrap();
    editor.graph_mut().set_output(g).unwrap();
    editor.publish().unwrap();
}

#[test]
fn live_process_is_allocation_free_with_and_without_pending_swap() {
    let (mut editor, mut live) = live_pair();
    editor.configure(R, 256);
    publish_sine_chain(&mut editor, 0.5);
    let midi = MidiEventBuffer::with_capacity(0);

    // Warm-up: applies the first swap and runs one block.
    live.process(BlockTime::free_running(), 256, &midi).unwrap();
    assert_eq!(live.swap_count(), 1);

    // Steady state: nothing pending.
    let mut time = BlockTime::free_running();
    with_rt(|| {
        superb_rt::assert_no_alloc(|| {
            for i in 0..32 {
                let frames = if i % 2 == 0 { 256 } else { 200 };
                live.process(time, frames, &midi).unwrap();
                time = time.advanced_by(frames, R);
                assert_eq!(live.output().frames(), frames);
                assert!(live.output().peak() > 0.0);
            }
        });
    });
    assert_eq!(live.swap_count(), 1);

    // Publish (allocates, control thread) ...
    let c = editor
        .graph_mut()
        .add_node(Box::new(ConstantNode::new(0.125, 2)));
    editor.graph_mut().set_output(c).unwrap();
    editor.publish().unwrap();
    assert!(live.has_pending());

    // ... then the swap itself happens inside process with no heap activity.
    with_rt(|| {
        superb_rt::assert_no_alloc(|| {
            live.process(time, 256, &midi).unwrap();
            assert_eq!(live.output().channel(0)[0], 0.125);
            live.process(time, 128, &midi).unwrap();
            assert_eq!(live.output().frames(), 128);
        });
    });
    assert_eq!(live.swap_count(), 2);
    assert!(!live.has_pending());

    // The retired sine graph is dropped here, on the control thread (the
    // initial empty graph was already reclaimed by the second publish).
    assert_eq!(editor.try_reclaim(), 1);
    assert_eq!(editor.reclaimed_count(), 2);
}
