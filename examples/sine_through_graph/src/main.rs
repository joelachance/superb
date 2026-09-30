//! Renders three seconds of a 440 Hz sine through a graph and a mixer, offline,
//! and writes `sine_through_graph.wav` next to the current directory.
//!
//! What you'll learn:
//! - Mixer strip (gain dB + pan) sitting on top of graph nodes
//! - Transport + TempoMap loop that wraps mid-block
//! - `offline::render` → `PlanarBuffer` → WAV file
//! - Reading engine stats and track meters after a bounce
//!
//! ```text
//! cargo run -p sine_through_graph
//! ```

use superb::io::offline;
use superb::io::wav::{self, WavFormat};
use superb::prelude::*;

fn main() -> Result<()> {
    let sample_rate = SampleRate::HZ_48K;
    let block = 256;
    let seconds = 3.0;

    // Mixer: one track, sine source, -6 dB fader, panned slightly left.
    // Mixer::new(2) = stereo master.
    let mut mixer = Mixer::new(2);
    let track = mixer.add_track("lead");
    // Oscillator and a linear gain node live in the mixer's shared AudioGraph.
    let osc = mixer.graph_mut().add_node(Box::new(SineNode::new(440.0, 0.8, 2)));
    let gain = mixer.graph_mut().add_node(Box::new(GainNode::new(0.5, 2)));
    mixer.graph_mut().connect(osc, gain)?;
    // connect_source feeds that gain into the track strip (then to master).
    mixer.connect_source(gain, track)?;
    let params = mixer.track_params(track)?;
    params.set_gain_db(-6.0);
    // pan: -1.0 = hard left, 0.0 = center, +1.0 = hard right.
    params.set_pan(-0.3);

    // Transport at 120 bpm with a 3-beat loop; its length is not a multiple
    // of the block size, so wraps land mid-block and split blocks in two.
    let mut transport = Transport::new(sample_rate, 120.0);
    let loop_end = transport.tempo_map().beat_to_samples(BeatPosition::new(3.0));
    transport.set_loop(Some(LoopRange::new(TimePosition::ZERO, loop_end)?));
    transport.set_loop_enabled(true);
    transport.play();

    let mut engine = Engine::new(mixer, transport);
    // seconds_to_samples converts wall time → frame count at this rate.
    let total = sample_rate.seconds_to_samples(seconds) as usize;
    let audio = offline::render(&mut engine, sample_rate, block, total)?;

    let path = "sine_through_graph.wav";
    wav::write(path, &audio, sample_rate, WavFormat::Float32)?;

    let stats = engine.stats();
    let snap = engine.transport().snapshot();
    println!("wrote {path}: {} frames x {} ch @ {}", audio.frames(), audio.channels(), sample_rate);
    // fold walks each channel finding max absolute sample (a simple peak meter).
    println!("peak L/R: {:.3} / {:.3}", audio.channel(0).iter().fold(0.0f32, |m, s| m.max(s.abs())), audio.channel(1).iter().fold(0.0f32, |m, s| m.max(s.abs())));
    // segments > blocks when loop wraps split a block into two process calls.
    println!("blocks: {}, segments (loop wraps add one): {}", stats.blocks, stats.segments);
    println!("transport now at {} = {}", snap.position, snap.bbt);
    // params still refers to the strip meters updated during render.
    println!("track meter L/R: {:.3} / {:.3}", params.peak(0), params.peak(1));
    Ok(())
}
