//! `devices`: what hardware is there.
//!
//! What you'll learn:
//! - Audio device listings mark the system default and its channel count
//! - A device is chosen by name (or `None` for the default) when you play
//!   through it (`devices::output`) or record from it (`record::start`);
//!   the stream is matched to the session's sample rate and block size for
//!   you
//! - With feature `midi-io`, MIDI input ports are listed too
//!
//! ```text
//! cargo run --example list_devices --features devices
//! cargo run --example list_devices --features hardware   # audio + MIDI
//! ```

use aura::devices;
use aura::prelude::*;

/// `max_channels` is 0 when the host does not say.
fn channels(max: usize) -> String {
    if max == 0 {
        "channels unknown".into()
    } else {
        format!("{max} ch max")
    }
}

fn main() -> Result<()> {
    // `*` marks the default device: the one `None` picks.
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

    #[cfg(feature = "midi-io")]
    {
        println!("MIDI inputs:");
        for p in devices::midi_inputs()? {
            println!("  {}: {}", p.index, p.name);
        }
    }
    Ok(())
}
