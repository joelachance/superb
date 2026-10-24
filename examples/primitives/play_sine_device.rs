//! Plays a 440 Hz sine for three seconds on the default output device and
//! prints the DSP load afterwards.
//!
//! What you'll learn:
//! - Feature `devices` (cpal) for real hardware output
//! - Building a tiny graph (sine → gain) and handing it to `Engine`
//! - `run_engine` starts a callback stream; sleep while it plays
//! - Reading a shared `LoadMeter` (callbacks, avg/peak %, overruns)
//!
//! ```text
//! cargo run --example play_sine_device --features devices
//! ```

use std::time::Duration;

use superb::prelude::*;
use superb::primitives::io::device::{output_devices, run_engine, OutputConfig};

fn main() -> Result<()> {
    // List every output device cpal can see; mark the system default.
    for d in output_devices()? {
        println!("{}{}", d.name, if d.is_default { " (default)" } else { "" });
    }

    let mut graph = AudioGraph::new();
    // Quiet sine (amp 0.2) so default speakers are not blasted.
    let osc = graph.add_node(Box::new(SineNode::new(440.0, 0.2, 2)));
    let gain = graph.add_node(Box::new(GainNode::new(1.0, 2)));
    graph.connect(osc, gain)?;
    graph.set_output(gain)?;

    let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
    engine.transport_mut().play();
    // load() returns an Arc-shared LoadMeter the device callback updates.
    let load = engine.load();

    // OutputConfig::default() picks the default device / rate / buffer hints.
    // run_engine moves the engine into the audio callback thread.
    let config = OutputConfig::default();
    let stream = run_engine(&config, engine)?;
    println!(
        "playing on '{}' @ {} ...",
        stream.device_name(),
        stream.sample_rate()
    );
    // Keep this thread alive while the stream plays; Drop stops the stream.
    std::thread::sleep(Duration::from_secs(3));
    println!(
        "callbacks: {}, avg load: {:.1}%, peak load: {:.1}%, overruns: {}",
        load.callbacks(),
        load.load() * 100.0,
        load.peak() * 100.0,
        load.overruns()
    );
    Ok(())
}
