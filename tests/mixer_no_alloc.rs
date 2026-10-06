//! A prepared mixer processes without heap activity, even while parameters move.

use superb::primitives::graph::nodes::{GainNode, SineNode};
use superb::primitives::graph::{BlockProcessor, BlockTime};
use superb::primitives::midi::MidiEventBuffer;
use superb::primitives::mixer::{Mixer, SendTap};
use superb::primitives::realtime::CountingAllocator;
use superb::primitives::SampleRate;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::system();

#[test]
fn mixer_process_is_allocation_free() {
    let mut mixer = Mixer::new(2);
    let a = mixer.add_track("a");
    let b = mixer.add_track("b");
    let fx = mixer.add_bus("fx");
    let sa = mixer
        .graph_mut()
        .add_node(Box::new(SineNode::new(220.0, 0.5, 2)));
    let sb = mixer
        .graph_mut()
        .add_node(Box::new(SineNode::new(330.0, 0.5, 1)));
    mixer.connect_source(sa, a).unwrap();
    mixer.connect_source(sb, b).unwrap();
    let send = mixer.add_send(a, fx, 0.3, SendTap::PreFader).unwrap();
    mixer.set_track_output(b, fx).unwrap();
    // An insert on track a and one on the fx bus are under the proof too.
    mixer
        .add_insert(a, Box::new(GainNode::new(0.7, 2)))
        .unwrap();
    mixer
        .add_insert(fx, Box::new(GainNode::new(0.9, 2)))
        .unwrap();
    mixer.prepare(SampleRate::HZ_48K, 256).unwrap();

    let pa = mixer.track_params(a).unwrap();
    let pb = mixer.track_params(b).unwrap();
    let ps = mixer.send_params(send).unwrap();
    let midi = MidiEventBuffer::with_capacity(0);
    mixer
        .process(BlockTime::free_running(), 256, &midi, None)
        .unwrap();

    superb::primitives::realtime::assert_no_alloc(|| {
        for i in 0..100 {
            pa.set_gain(0.5 + (i % 5) as f32 * 0.1);
            pa.set_pan((i % 7) as f32 / 3.0 - 1.0);
            pb.set_mute(i % 4 == 0);
            pb.set_solo(i % 9 == 0);
            ps.set_enabled(i % 3 != 0);
            mixer
                .process(
                    BlockTime::free_running(),
                    if i % 2 == 0 { 256 } else { 200 },
                    &midi,
                    None,
                )
                .unwrap();
            let _ = pa.take_peak(0);
        }
    });
    assert!(mixer.output().peak() > 0.0);
}
