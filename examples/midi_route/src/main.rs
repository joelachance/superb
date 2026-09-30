//! Per-node MIDI routing and sample-accurate external MIDI, offline.
//!
//! Two `MonoSynthNode`s are summed into a pass-through sink. A single MIDI
//! edge (`MidiPort::External -> synth A`) means only A hears the note; B
//! stays silent. The note itself is pushed into the engine's MIDI queue as a
//! `TimestampedMessage` stamped on the engine's stream clock, so it lands
//! at an exact frame inside the block instead of at frame 0.
//!
//! What you'll learn:
//! - Audio edges vs MIDI edges (`connect` vs `connect_midi`)
//! - SPSC `channel` + `Engine::set_midi_input` for external MIDI
//! - Stream-clock timestamps → sample-accurate note placement
//! - Inspecting per-node output peaks after a render
//!
//! ```text
//! cargo run -p midi_route
//! ```

use superb::graph::MidiPort;
use superb::prelude::*;

fn main() -> Result<()> {
    let sample_rate = SampleRate::HZ_48K;
    // 480 frames @ 48 kHz = 10 ms — a typical small block for demos.
    let block = 480; // 10 ms at 48 kHz

    let mut graph = AudioGraph::new();
    // MonoSynthNode: simple MIDI-triggered sine (1 = mono output channels).
    let synth_a = graph.add_node(Box::new(MonoSynthNode::new(1)));
    let synth_b = graph.add_node(Box::new(MonoSynthNode::new(1)));
    let sink = graph.add_node(Box::new(PassThroughNode::new(1)));
    // Audio wiring: both synths sum into the sink (mix on input).
    graph.connect(synth_a, sink)?;
    graph.connect(synth_b, sink)?;
    graph.set_output(sink)?;
    // The one MIDI edge: external MIDI goes to A only.
    // Without MIDI edges, external MIDI would broadcast to every node.
    graph.connect_midi(MidiPort::External, synth_a)?;

    let mut engine = Engine::new(graph, Transport::new(sample_rate, 120.0));
    // prepare allocates RT scratch / compiles the schedule for this block size.
    engine.prepare(sample_rate, block)?;
    engine.transport_mut().play();

    // A non-RT producer (here: main) stamps messages on the engine's stream
    // clock. The first block spans 0..10 000 µs; 5 000 µs is frame 240.
    // spsc::channel is wait-free: safe for RT consumer / non-RT producer.
    let (mut tx, rx) = superb::rt::spsc::channel::<TimestampedMessage>(16);
    engine.set_midi_input(Some(rx));
    // MidiChannel::new validates 0..15; unwrap() is OK for a known-good 0.
    let channel = MidiChannel::new(0).unwrap();
    let note_on_us = engine.stream_clock_us() + 5_000;
    // map_err converts a full-queue push failure into SuperbError.
    tx.push(TimestampedMessage { timestamp_us: note_on_us, message: MidiMessage::NoteOn { channel, note: 69, velocity: 127 } })
        .map_err(|_| SuperbError::Message("midi queue full".into()))?;

    // PlanarBuffer: separate Vec per channel (not interleaved LRLR...).
    let mut out = PlanarBuffer::new(1, block);
    engine.render(&mut out)?;

    // First half of the block should be silence; second half should sound.
    let silent_before = out.channel(0)[..240].iter().all(|&s| s == 0.0);
    let sounding_after = out.channel(0)[240..].iter().any(|&s| s != 0.0);
    println!(
        "block 0: note-on stamped {note_on_us} us -> frame 240: silent before = {silent_before}, sounding after = {sounding_after}"
    );
    // processor() borrows the live graph so we can peek each node's last output.
    let graph = engine.processor();
    println!(
        "  synth A peak {:.3}, synth B peak {:.3}, mix peak {:.3}",
        graph.node_output(synth_a)?.peak(),
        graph.node_output(synth_b)?.peak(),
        out.peak()
    );

    // Second block: a note-off 2.5 ms in (frame 120).
    let note_off_us = engine.stream_clock_us() + 2_500;
    tx.push(TimestampedMessage { timestamp_us: note_off_us, message: MidiMessage::NoteOff { channel, note: 69, velocity: 0 } })
        .map_err(|_| SuperbError::Message("midi queue full".into()))?;
    engine.render(&mut out)?;
    // rposition finds the last non-zero sample index from the end.
    let last_sound = out.channel(0).iter().rposition(|&s| s != 0.0);
    println!("block 1: note-off stamped {note_off_us} us -> last non-zero sample at frame {last_sound:?} (expected Some(119))");
    println!("engine stats: {:?}", engine.stats());
    Ok(())
}
