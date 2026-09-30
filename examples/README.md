# Superb SDK examples

Each package is a small, commented walkthrough for learning Rust + Superb.

| Example | Purpose | Run |
|---|---|---|
| `sine_through_graph` | Offline sine → mixer → WAV (looping transport) | `cargo run --example sine_through_graph` |
| `automation_gain_ramp` | Sample-accurate gain envelope into `AtomicF32` | `cargo run --example automation_gain_ramp` |
| `live_graph_edit` | Swap graphs while a block loop keeps running | `cargo run --example live_graph_edit` |
| `midi_route` | MIDI edge + sample-accurate queued note (offline) | `cargo run --example midi_route` |
| `midi_monitor` | Print hardware MIDI for 10 s | `cargo run --example midi_monitor --features midi-io` |
| `play_sine_device` | Play a sine on the default output device | `cargo run --example play_sine_device --features devices` |
| `decode_bounce` | Bounce → decode → resample → export (`files`) | `cargo run --example decode_bounce` |
| `session_undo` | Undoable session edits, save/load JSON | `cargo run --example session_undo` |
| `session_playback` | Session clips + bus/send → offline WAV | `cargo run --example session_playback` |
| `clap_host_scan` | Scan/instantiate a CLAP plugin, render offline | `cargo run --example clap_host_scan -- [dir ...] [--id …]` |

Open `examples/*/src/main.rs` for line-by-line teaching comments (Rust idioms + DAW concepts).
