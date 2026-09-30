//! Proves that `AudioGraph::process` performs no heap operations once prepared.

use superb_core::{SampleRate, TimePosition};
use superb_graph::nodes::{DelayNode, GainNode, MonoSynthNode, PassThroughNode, SineNode};
use superb_graph::{AudioGraph, BlockTime, MidiPort};
use superb_midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage};
use superb_rt::CountingAllocator;

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::system();

#[test]
fn process_is_allocation_free() {
    let mut graph = AudioGraph::new();
    let sine = graph.add_node(Box::new(SineNode::new(220.0, 0.5, 2)));
    let synth = graph.add_node(Box::new(MonoSynthNode::new(2)));
    let gain = graph.add_node(Box::new(GainNode::new(0.8, 2)));
    let bus = graph.add_node(Box::new(PassThroughNode::new(2)));
    // A parallel path with latency: the direct `gain -> bus` edge gets a
    // 37-frame compensation delay line, so the delayed-mix path is under
    // the allocation proof too.
    let lookahead = graph.add_node(Box::new(DelayNode::new(37, 2)));
    graph.connect(sine, gain).unwrap();
    graph.connect(synth, gain).unwrap();
    graph.connect(gain, bus).unwrap();
    graph.connect(gain, lookahead).unwrap();
    graph.connect(lookahead, bus).unwrap();
    graph.set_output(bus).unwrap();
    // A MIDI edge switches the graph from broadcast to per-node routing; the
    // sine/gain/bus nodes then receive the pre-allocated empty buffer.
    graph.connect_midi(MidiPort::External, synth).unwrap();
    graph.prepare(SampleRate::HZ_48K, 512).unwrap();
    assert_eq!(graph.latency_frames(), 37);
    assert_eq!(graph.edge_delay(gain, bus).unwrap(), 37);

    let ch = MidiChannel::new(0).unwrap();
    let mut midi = MidiEventBuffer::with_capacity(8);
    midi.push(MidiEvent {
        frame: 3,
        message: MidiMessage::NoteOn {
            channel: ch,
            note: 60,
            velocity: 100,
        },
    })
    .unwrap();
    let gain_handle = match graph.node_mut(gain) {
        Ok(_) => (),
        Err(e) => panic!("{e}"),
    };
    let _ = gain_handle;

    // Warm-up call (first-time lazy initialisation, if any, happens here).
    graph
        .process(BlockTime::free_running(), 512, &midi)
        .unwrap();

    let mut time = BlockTime {
        rolling: true,
        ..BlockTime::free_running()
    };
    superb_rt::assert_no_alloc(|| {
        for i in 0..64 {
            let frames = if i % 3 == 0 { 512 } else { 480 };
            graph.process(time, frames, &midi).unwrap();
            time = time.advanced_by(frames, SampleRate::HZ_48K);
            let out = graph.output();
            assert_eq!(out.frames(), frames);
            assert!(out.peak() > 0.0);
        }
        assert!(
            graph.stats().delayed_mixes > 0,
            "the compensated edge is live"
        );
        graph.reset();
        graph.process(time, 480, &midi).unwrap();
        let _ = TimePosition::ZERO;
    });
}
