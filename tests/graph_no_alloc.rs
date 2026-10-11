//! Proves that `AudioGraph::process` performs no heap operations once prepared.

use aura::primitives::buffers::PlanarBuffer;
use std::sync::Arc;

use aura::primitives::graph::nodes::{
    ClickNode, ClickParams, DelayNode, GainNode, LiveInputNode, MonoSynthNode, PassThroughNode,
    SineNode,
};
use aura::primitives::graph::{AudioGraph, BlockTime, MidiPort};
use aura::primitives::midi::{MidiChannel, MidiEvent, MidiEventBuffer, MidiMessage};
use aura::primitives::realtime::CountingAllocator;
use aura::primitives::{SampleRate, TimePosition};

#[global_allocator]
static ALLOC: CountingAllocator = CountingAllocator::system();

#[test]
fn process_is_allocation_free() {
    let mut graph = AudioGraph::new();
    let sine = graph.add_node(Box::new(SineNode::new(220.0, 0.5, 2)));
    let synth = graph.add_node(Box::new(MonoSynthNode::new(2)));
    // A monitoring node copying the engine's live input.
    let live = graph.add_node(Box::new(LiveInputNode::new([0, 1])));
    // The metronome ticks on every beat of the rolling blocks below.
    let click = graph.add_node(Box::new(ClickNode::new(
        2,
        Arc::new(ClickParams::new(true, 0.5)),
    )));
    let gain = graph.add_node(Box::new(GainNode::new(0.8, 2)));
    let bus = graph.add_node(Box::new(PassThroughNode::new(2)));
    // A parallel path with latency: the direct `gain -> bus` edge gets a
    // 37-frame compensation delay line, so the delayed-mix path is under
    // the allocation proof too.
    let lookahead = graph.add_node(Box::new(DelayNode::new(37, 2)));
    graph.connect(sine, gain).unwrap();
    graph.connect(synth, gain).unwrap();
    graph.connect(live, gain).unwrap();
    graph.connect(click, gain).unwrap();
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
    let mut input = PlanarBuffer::new(2, 512);
    input.fill(0.1);
    // Warm-up call (first-time lazy initialisation, if any, happens here).
    graph
        .process(BlockTime::free_running(), 512, &midi, Some(&input))
        .unwrap();

    let mut time = BlockTime {
        rolling: true,
        ..BlockTime::free_running()
    };
    aura::primitives::realtime::assert_no_alloc(|| {
        for i in 0..64 {
            let frames = if i % 3 == 0 { 512 } else { 480 };
            input.set_frames(frames).unwrap();
            graph.process(time, frames, &midi, Some(&input)).unwrap();
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
        graph.process(time, 480, &midi, None).unwrap();
        let _ = TimePosition::ZERO;
    });
}
