//! Lists MIDI input ports, connects to the first one (or the index given as
//! the first argument) and prints messages for ten seconds. Demonstrates the
//! non-RT → RT hand-off: the midir callback pushes into an SPSC queue and the
//! "audio thread" (here: the main thread) drains it.
//!
//! What you'll learn:
//! - Feature-gated hardware MIDI (`midi-io` / midir)
//! - `Option` chaining with `and_then` / `unwrap_or`
//! - SPSC receive loop: pop until empty, then sleep
//! - Why dropped messages matter on a full lock-free queue
//!
//! ```text
//! cargo run -p midi_monitor --features midi-io -- [port_index]
//! ```

use std::time::{Duration, Instant};

use superb::midi::input::{list_input_ports, MidiInputBridge};
use superb::prelude::*;

fn main() -> Result<()> {
    // Ask the OS (via midir) for available MIDI input ports.
    let ports = list_input_ports()?;
    if ports.is_empty() {
        println!("no MIDI input ports found");
        return Ok(());
    }
    for p in &ports {
        println!("[{}] {}", p.index, p.name);
    }

    // nth(1) = first CLI arg after the binary name.
    // and_then(|s| s.parse().ok()) tries to parse as usize; invalid → None.
    // unwrap_or(0) picks port 0 when no valid index was given.
    let index = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);

    // connect opens the port; callbacks push TimestampedMessage into an SPSC.
    // We get (bridge metadata, rx consumer). Capacity 1024 messages.
    let (bridge, mut rx) = MidiInputBridge::connect(index, 1024)?;
    println!("listening on '{}' for 10 s ...", bridge.port_name());

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        // Drain everything currently in the queue (non-blocking pop).
        while let Some(TimestampedMessage {
            timestamp_us,
            message,
        }) = rx.pop()
        {
            // Destructure the struct in the pattern; {:?} debug-prints the message.
            println!("{timestamp_us:>12} us  {message:?}");
        }
        // Brief sleep so we don't busy-spin when the queue is empty.
        std::thread::sleep(Duration::from_millis(5));
    }
    // If the producer outran the consumer, midir messages were dropped.
    println!("dropped (queue full): {}", bridge.dropped());
    Ok(())
}
