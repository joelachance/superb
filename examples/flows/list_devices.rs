//! `devices`: what hardware is there, and a configuration that fits the rig.
//!
//! What you'll learn:
//! - Audio device listings mark the system default and its channel count
//! - `output_config` / `input_config` copy the rig's sample rate and block
//!   size, which the engine's published graphs are prepared for
//! - With feature `midi-io`, MIDI input ports are listed too
//!
//! ```text
//! cargo run --example list_devices --features devices
//! cargo run --example list_devices --features hardware   # audio + MIDI
//! ```

use superb::prelude::*;
use superb::{devices, project};

/// `max_channels` is 0 when the host does not say.
fn channels(max: usize) -> String {
    if max == 0 {
        "channels unknown".into()
    } else {
        format!("{max} ch max")
    }
}

fn main() -> Result<()> {
    let rig = project::create("devices", SampleRate::HZ_48K, 256)?;

    println!("outputs:");
    for d in devices::outputs()? {
        println!(
            "  {}{} ({})",
            d.name,
            if d.is_default { " *" } else { "" },
            channels(d.max_channels)
        );
    }
    println!("inputs:");
    for d in devices::inputs()? {
        println!(
            "  {}{} ({})",
            d.name,
            if d.is_default { " *" } else { "" },
            channels(d.max_channels)
        );
    }

    // Configurations that match the rig. `None` picks the default device;
    // pass a listed name to choose another.
    let out = devices::output_config(&rig, None);
    println!(
        "output config: {} Hz, {} ch, {}-frame blocks, device {:?}",
        out.sample_rate.hz(),
        out.channels,
        out.max_block_frames,
        out.device_name
    );
    let inp = devices::input_config(&rig, None, 1);
    println!(
        "input config:  {} Hz, {} ch, {}-frame blocks",
        inp.sample_rate.hz(),
        inp.channels,
        inp.max_block_frames
    );

    #[cfg(feature = "midi-io")]
    {
        println!("MIDI inputs:");
        for p in devices::midi_inputs()? {
            println!("  {}: {}", p.index, p.name);
        }
    }
    Ok(())
}
