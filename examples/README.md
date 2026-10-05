# Examples

Each example is a short, commented program: the comments explain the Rust
and the DAW idea side by side. Everything runs without hardware unless
hardware is the point, and every example is registered in `Cargo.toml`, so
`cargo run --example <name>` is all you need.

## Flows

One per flow module, in the order you would use them to make a session.

| example | flow | shows | run |
|---|---|---|---|
| `new_session` | `session`, `track` | tracks, an audio clip from the cache, a MIDI clip, tempo, marker, undo/redo, save and open | `cargo run --example new_session` |
| `play_session` | `transport` | start, offline blocks, playhead, seek, loop, stop, rewind; device output with `devices` | `cargo run --example play_session` |
| `mix_session` | `track`, `send` | a volume drag as one undo step, pan, mute, solo, a bus with sends, routing, meters | `cargo run --example mix_session` |
| `midi_keys` | `midi` | a monitoring MIDI track, notes and a chord, release, any message; ports with `midi-io` | `cargo run --example midi_keys` |
| `record_take` | `record` | arm, punch in, a recording from a synthetic microphone, clips, undo; a real input with `devices` | `cargo run --example record_take` |
| `export_session` | `export` | bounce the session, a range, render to memory; resample with `files` | `cargo run --example export_session` |
| `session_steps` | `session::steps` | build the mixer from a document by hand (strips, a bus, a send, the live controls) and render it through an offline engine | `cargo run --example session_steps` |
| `list_devices` | `devices` | audio devices with the default marked; MIDI ports with `midi-io` | `cargo run --example list_devices --features devices` |

## Primitives

| example | primitive | shows | run |
|---|---|---|---|
| `sine_through_graph` | `graph`, `mixer`, `io` | sine → mixer → WAV with a looping transport | `cargo run --example sine_through_graph` |
| `automation_gain_ramp` | `automation` | sample-accurate envelope into an `AtomicF32` | `cargo run --example automation_gain_ramp` |
| `live_graph_edit` | `graph::live` | swap graphs while a block loop keeps running | `cargo run --example live_graph_edit` |
| `transport_tempo_map` | `transport` | tempo and meter changes, bars/beats, loop segments | `cargo run --example transport_tempo_map` |
| `realtime_queue` | `realtime` | SPSC queue and atomics between a UI and an audio thread | `cargo run --example realtime_queue` |
| `midi_route` | `midi`, `graph` | MIDI edges and a sample-accurate queued note | `cargo run --example midi_route` |
| `midi_monitor` | `midi::input` | print hardware MIDI for ten seconds | `cargo run --example midi_monitor --features midi-io` |
| `play_sine_device` | `io::device` | a sine on the default output device, with load stats | `cargo run --example play_sine_device --features devices` |
| `decode_bounce` | `io::files` | bounce → decode → resample → export | `cargo run --example decode_bounce --features files` |
| `session_undo` | `session`, `undo` | undoable document edits, JSON save and load | `cargo run --example session_undo` |
| `clap_host_scan` | `plugins::clap` | scan and instantiate a CLAP plugin, render offline | `cargo run --example clap_host_scan -- [dir ...] [--id …]` |

The `buffers` and `undo` modules have no example of their own: every
example above uses `PlanarBuffer`, and `session_undo` and `new_session` are
the undo stack in practice.
