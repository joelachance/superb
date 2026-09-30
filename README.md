# Superb

An open-source Rust toolkit for building a digital audio workstation (DAW).
One crate, `superb`, dual-licensed Apache-2.0 OR MIT.

Superb is the engine side of a DAW: sample-accurate time, audio buffers, a
real-time-safe processing graph, a mixer with tracks, buses, sends and
insert chains, MIDI, automation, a project document with undo, CLAP plugin
hosting, recording, offline export, and audio device and file I/O. It has
no user interface; you bring your own.

## Two tiers

The crate is organised so you always know what a call will do.

**Flows** are top-level modules named after everyday tasks. Each one is a
set of free functions that take a `Rig` (the state of a song in progress)
and do the whole job: `project`, `play`, `mix`, `midi`, `record`,
`plugins`, `export`, `devices`. Every flow function documents the
primitive calls it is *made of*, so when one does not do exactly what you
want, you call those primitives yourself. Nothing is hidden behind an
object; the `Rig` is a plain struct with public fields.

**Primitives** live under `superb::primitives` and are the engine proper.
They know nothing about flows or the `Rig`.

| module | in DAW terms | what it holds | doc |
|---|---|---|---|
| `primitives` (root) | shared vocabulary | sample-accurate time, beats, sample rate, IDs, errors | [primitives.md](docs/primitives.md) |
| `primitives::buffers` | audio buffers | planar and interleaved `f32` blocks, gain and dB helpers | [buffers.md](docs/primitives/buffers.md) |
| `primitives::realtime` | real-time helpers | wait-free queue, atomic floats, allocation counting, load meter | [realtime.md](docs/primitives/realtime.md) |
| `primitives::transport` | transport bar | play/stop/record state, playhead, tempo and meter map, loop range | [transport.md](docs/primitives/transport.md) |
| `primitives::midi` | MIDI | messages, per-block event buffer, hardware input bridge (feature `midi-io`) | [midi.md](docs/primitives/midi.md) |
| `primitives::graph` | signal path | the `Node` trait, the processing graph, silence propagation, latency compensation, live swapping | [graph.md](docs/primitives/graph.md) |
| `primitives::mixer` | mixer | tracks, buses, sends, insert chains, pan laws, mute/solo, peak meters | [mixer.md](docs/primitives/mixer.md) |
| `primitives::automation` | automation | envelopes, read/touch/latch/write modes, sample-accurate apply | [automation.md](docs/primitives/automation.md) |
| `primitives::undo` | undo history | coalescing undo/redo stack with groups and clean-state tracking | [undo.md](docs/primitives/undo.md) |
| `primitives::session` | project / song | tracks, clips, markers, routing, inserts, tempo map, JSON save/load, undoable commands | [session.md](docs/primitives/session.md) |
| `primitives::plugins` | plugin host | plugin instance model, `PluginNode`, CLAP host (VST3 is a stub) | [plugins.md](docs/primitives/plugins.md) |
| `primitives::io` | engine, devices, files | block loop, live engine with control handle, offline render, capture queue, WAV, decode/resample (feature `files`), device streams (feature `devices`) | [io.md](docs/primitives/io.md) |

| flow | what it does | doc |
|---|---|---|
| `project` | create, open and save a song; add tracks, clips, markers; tempo and loop; undo and redo | [project.md](docs/flows/project.md) |
| `play` | build the console from the song and hear it: start, stop, seek, loop, render offline or on a device | [play.md](docs/flows/play.md) |
| `mix` | faders, pan, mute, solo, buses, sends, routing, meters | [mix.md](docs/flows/mix.md) |
| `midi` | monitor a MIDI track live, send notes, connect a keyboard | [midi.md](docs/flows/midi.md) |
| `record` | arm tracks, capture a take, turn it into clips | [record.md](docs/flows/record.md) |
| `plugins` | find plugins, insert one on a track, set parameters, bypass | [plugins.md](docs/flows/plugins.md) |
| `export` | render a range or the whole song to WAV, optionally resampled | [export.md](docs/flows/export.md) |
| `devices` | list audio and MIDI hardware; configurations that match the rig | [devices.md](docs/flows/devices.md) |

[docs/flows.md](docs/flows.md) explains how the flows fit together (the
`Rig`, syncing, the two undo behaviours). [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
covers the engine and the rules every module follows. If the DAW words are
new to you, start with [docs/TERMINOLOGY.md](docs/TERMINOLOGY.md).

## Quick start

```rust
use superb::prelude::*;
use superb::{mix, play, project};

let mut rig = project::create("demo", SampleRate::HZ_48K, 256)?;
let lead = project::add_track(&mut rig, "lead", TrackKind::Audio)?;

// Audio comes from the cache: a decoded file, or a buffer you made.
let mut tone = PlanarBuffer::new(2, 48_000);
tone.fill(0.25);
rig.cache.insert("tone.wav", tone);
project::add_audio_clip(&mut rig, lead, "tone.wav", TimePosition::ZERO, TimeDuration::from_samples(48_000))?;
mix::set_gain_db(&mut rig, lead, -6.0)?;

play::start(&mut rig)?;                 // builds the console, starts the transport
let audio = play::advance(&mut rig, 8)?; // renders 8 blocks without an audio device
assert!(audio.peak() > 0.1);
```

Every example is a short, commented program that runs without hardware
unless hardware is the point:

```sh
cargo test                                     # the whole suite, no audio hardware needed
cargo run --example new_song                   # project: tracks, clips, undo, save and open
cargo run --example play_song                  # play: transport, loop, offline blocks
cargo run --example mix_session                # mix: faders, bus, sends, meters
cargo run --example midi_keys                  # midi: notes into a monitoring track
cargo run --example record_take                # record: a take from a synthetic microphone
cargo run --example insert_plugin              # plugins: scan, insert, set a parameter
cargo run --example export_song                # export: bounce to WAV
cargo run --example list_devices --features devices
```

See [examples/README.md](examples/README.md) for the full list, including
one example per primitive.

## Cargo features

Everything that touches hardware or a copyleft dependency is opt-in, so the
default build and test run need nothing but Rust.

| feature | enables |
|---|---|
| `clap` (default) | CLAP plugin hosting through `clap-sys` and `libloading` |
| `devices` | audio input and output streams through cpal |
| `midi-io` | hardware MIDI input through midir |
| `hardware` | both of the above |
| `files` | audio file decoding (WAV, FLAC), resampling and bounce helpers |

On Debian or Ubuntu the hardware features need `pkg-config libasound2-dev`.

## Real-time discipline

Everything on the audio path is allocated in `prepare` and never afterwards.
`primitives::realtime` ships a counting global allocator, and the
integration tests under `tests/` prove that `process` performs zero heap
operations, including while parameters move and while a new graph is
swapped in under a running engine. Console changes made through the flows
are published as whole prepared graphs and swapped in between two blocks.
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) spells out the rules.

## Status

Working today: a project document with tracks, clips, markers, routing,
inserts, JSON files and undo; a console built from the document with
faders, buses, sends and insert chains, swapped glitch-free into a running
engine; offline rendering and WAV export; device output and capture;
recording takes into clips; MIDI messages, live monitoring and hardware
input; automation envelopes and modes; CLAP plugin hosting with parameter
edits reaching the running instance; file decoding, resampling and
bouncing behind the `files` feature.

Not yet: duplex device streams, aligning hardware MIDI timestamps with the
audio clock, updating a running console incrementally (a sync rebuilds it,
which resets plugin tails), monitoring a live input through a strip while
recording, punch-in/out and loop recording, VST3 hosting, SysEx and
Standard MIDI Files, and encoders other than WAV.

## Licence

Apache-2.0 OR MIT, at your option. All code in this repository is original
work written for Superb. Every dependency in the default build is
Apache-2.0, MIT or ISC. The one MPL-2.0 dependency, the symphonia decoder,
is opt-in behind the `files` feature and confined to a single source file;
a test fails if it leaks anywhere else. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the policy.
