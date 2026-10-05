# Superb

An open-source Rust toolkit for building a digital audio workstation (DAW).
One crate, `superb`, dual-licensed Apache-2.0 OR MIT.

Superb is the engine side of a DAW: sample-accurate time, audio buffers, a
real-time-safe processing graph, a mixer with tracks, buses, sends and
insert chains, MIDI, automation, a session document with undo, CLAP plugin
hosting, recording, offline export, and audio device and file I/O. It has
no user interface; you bring your own.

## Two tiers

The crate is organised so you always know what a call will do.

**Flows** are top-level modules named after the things you work with and
the tasks you do. Each one is a set of free functions that take a
`Session` and do the whole job: `session`, `track`, `transport`, `mix`,
`midi`, `record`, `plugins`, `export`, `devices`. Every flow function
documents the primitive calls it is *made of*, so when one does not do
exactly what you want, you call those primitives yourself. Nothing is
hidden behind an object; the `Session` is a plain struct with public
fields.

**Primitives** live under `superb::primitives` and are the engine proper.
They know nothing about flows or the `Session`.

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
| `primitives::session` | the document | `Document`: tracks and their state, clips, markers, routing, inserts, tempo map, JSON save/load, undoable commands | [session.md](docs/primitives/session.md) |
| `primitives::plugins` | plugin host | plugin instance model, `PluginNode`, CLAP host (VST3 is a stub) | [plugins.md](docs/primitives/plugins.md) |
| `primitives::io` | engine, devices, files | block loop, live engine with control handle, offline render, capture queue, WAV, decode/resample (feature `files`), device streams (feature `devices`) | [io.md](docs/primitives/io.md) |

| flow | what it does | doc |
|---|---|---|
| `session` | create, open and save a session; clips, markers, tempo and loop; undo and redo; the engine side in `session::steps` | [session.md](docs/flows/session.md) |
| `track` | add, remove, rename; a track's state: mute, solo, arm, monitor, volume, pan, route | [track.md](docs/flows/track.md) |
| `transport` | start, stop, seek, rewind, loop; where the playhead is | [transport.md](docs/flows/transport.md) |
| `mix` | buses, sends, meters | [mix.md](docs/flows/mix.md) |
| `midi` | send notes, connect a keyboard | [midi.md](docs/flows/midi.md) |
| `record` | start a recording, stop it, get a clip on every armed track | [record.md](docs/flows/record.md) |
| `plugins` | find plugins, insert one on a track, set parameters, bypass | [plugins.md](docs/flows/plugins.md) |
| `export` | render a range or the whole session to WAV, optionally resampled | [export.md](docs/flows/export.md) |
| `devices` | list audio and MIDI hardware; play through an output; configurations that match the session | [devices.md](docs/flows/devices.md) |

[docs/flows.md](docs/flows.md) explains how the flows fit together (the
`Session`, edits versus state, how the mixer follows the document). [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
covers the engine and the rules every module follows. If the DAW words are
new to you, start with [docs/TERMINOLOGY.md](docs/TERMINOLOGY.md).

## Quick start

```rust
use superb::prelude::*;
use superb::{session, track, transport};

let mut session = session::create("demo", SampleRate::HZ_48K, 256)?;
let lead = track::add(&mut session, "lead", TrackKind::Audio)?;

// Audio comes from the cache: a decoded file, or a buffer you made.
let mut tone = PlanarBuffer::new(2, 48_000);
tone.fill(0.25);
session.cache.insert("tone.wav", tone);
session::add_audio_clip(&mut session, lead, "tone.wav", TimePosition::ZERO, TimeDuration::from_samples(48_000))?;
track::volume(&mut session, lead, -6.0)?;

transport::start(&mut session)?;                                   // builds the mixer, starts playing
let audio = session::steps::render_blocks(&mut session, 8)?;       // 8 blocks, no audio device needed
assert!(audio.peak() > 0.1);
```

Every example is a short, commented program that runs without hardware
unless hardware is the point:

```sh
cargo test                                     # the whole suite, no audio hardware needed
cargo run --example new_session                # session and track: clips, undo, save and open
cargo run --example play_session               # transport: start, seek, loop, offline blocks
cargo run --example mix_session                # track state and mix: faders, bus, sends, meters
cargo run --example midi_keys                  # midi: notes into a monitoring track
cargo run --example record_take                # record: a recording from a synthetic microphone
cargo run --example insert_plugin              # plugins: scan, insert, set a parameter
cargo run --example export_session             # export: bounce to WAV
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
swapped in under a running engine. Mixer changes made through the flows
are published as whole prepared graphs and swapped in between two blocks.
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) spells out the rules.

## Status

Working today: a document with tracks and their state, clips, markers,
routing, inserts, JSON files and undo; a mixer built from the document with
faders, buses, sends and insert chains, swapped glitch-free into a running
engine; offline rendering and WAV export; device output and capture;
recording into clips with per-track input selection; MIDI messages, live monitoring and hardware
input; automation envelopes and modes; CLAP plugin hosting with parameter
edits reaching the running instance; file decoding, resampling and
bouncing behind the `files` feature.

Not yet: duplex device streams, aligning hardware MIDI timestamps with the
audio clock, updating a running mixer incrementally (a rebuild replaces it,
which resets plugin tails), monitoring a live input through a strip,
punch-in/out and loop recording, VST3 hosting, SysEx and
Standard MIDI Files, and encoders other than WAV.

## Licence

Apache-2.0 OR MIT, at your option. All code in this repository is original
work written for Superb. Every dependency in the default build is
Apache-2.0, MIT or ISC. The one MPL-2.0 dependency, the symphonia decoder,
is opt-in behind the `files` feature and confined to a single source file;
a test fails if it leaks anywhere else. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the policy.
