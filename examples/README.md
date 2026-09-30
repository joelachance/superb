# Superb SDK examples

Each package is a small, commented walkthrough for learning Rust + Superb.

| Example | Purpose | Run |
|---|---|---|
| `sine_through_graph` | Offline sine → mixer → WAV (looping transport) | `cargo run -p sine_through_graph` |
| `automation_gain_ramp` | Sample-accurate gain envelope into `AtomicF32` | `cargo run -p automation_gain_ramp` |
| `live_graph_edit` | Swap graphs while a block loop keeps running | `cargo run -p live_graph_edit` |
| `midi_route` | MIDI edge + sample-accurate queued note (offline) | `cargo run -p midi_route` |
| `midi_monitor` | Print hardware MIDI for 10 s | `cargo run -p midi_monitor --features midi-io` |
| `play_sine_device` | Play a sine on the default output device | `cargo run -p play_sine_device --features devices` |
| `decode_bounce` | Bounce → decode → resample → export (`files`) | `cargo run -p decode_bounce` |
| `session_undo` | Undoable session edits, save/load JSON | `cargo run -p session_undo` |
| `session_playback` | Session items + bus/send → offline WAV | `cargo run -p session_playback` |
| `clap_host_scan` | Scan/instantiate a CLAP plugin, render offline | `cargo run -p clap_host_scan -- [dir ...] [--id …]` |

Open `examples/*/src/main.rs` for line-by-line teaching comments (Rust idioms + DAW concepts).
