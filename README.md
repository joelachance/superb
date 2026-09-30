# Superb SDK

An open-source Rust toolkit for building a digital audio workstation (DAW).
Dual-licensed under Apache-2.0 OR MIT.

Superb provides the engine side of a DAW: sample-accurate time, audio
buffers, a real-time-safe processing graph, a mixer with tracks, buses and
sends, MIDI, automation, a project document with undo, CLAP plugin hosting,
and audio device and file I/O. It has no user interface; you bring your own.

If you make music but have never read DAW engine code, start with
[docs/TERMINOLOGY.md](docs/TERMINOLOGY.md), which maps each crate to the
word a musician would use. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
then explains how the pieces fit together and the rules every module
follows.

## What is in the box

| crate | in DAW terms | what it holds | doc |
|---|---|---|---|
| `superb-core` | shared vocabulary | sample-accurate time, beats, sample rate, IDs, errors | [superb-core.md](docs/superb-core.md) |
| `superb-buffers` | audio buffers | planar and interleaved `f32` blocks, gain and dB helpers, format conversion | [superb-buffers.md](docs/superb-buffers.md) |
| `superb-rt` | real-time helpers | wait-free queue, atomic floats, allocation counting, load meter | [superb-rt.md](docs/superb-rt.md) |
| `superb-transport` | transport bar | play/stop/record state, playhead, tempo and meter map, loop range | [superb-transport.md](docs/superb-transport.md) |
| `superb-midi` | MIDI | messages, per-block event buffer, hardware input bridge (feature `midir`) | [superb-midi.md](docs/superb-midi.md) |
| `superb-graph` | signal path | the `Node` trait, the processing graph, silence propagation, latency compensation, live editing | [superb-graph.md](docs/superb-graph.md) |
| `superb-mixer` | mixer | tracks, buses, sends, pan laws, mute/solo, peak meters | [superb-mixer.md](docs/superb-mixer.md) |
| `superb-automation` | automation | envelopes, read/touch/latch/write modes, sample-accurate apply | [superb-automation.md](docs/superb-automation.md) |
| `superb-undo` | undo history | coalescing undo/redo stack with groups and clean-state tracking | [superb-undo.md](docs/superb-undo.md) |
| `superb-session` | project / song | tracks, clips, markers, routing, tempo map, JSON save/load, undoable commands | [superb-session.md](docs/superb-session.md) |
| `superb-plugins` | plugin host | plugin instance model, `PluginNode`, CLAP host (VST3 is a stub) | [superb-plugins.md](docs/superb-plugins.md) |
| `superb-io` | devices and files | engine block loop, offline render, WAV, decode/resample/bounce (feature `files`), device streams (feature `devices`) | [superb-io.md](docs/superb-io.md) |
| `superb` | the crate an app imports | re-exports everything, plus the session-to-mixer bridge and offline clip playback | [superb.md](docs/superb.md) |

Dependencies flow one way:
`core → buffers / rt → transport / midi / automation → graph → mixer → io → superb`,
with the document layer beside it: `core → undo → session` (session also
uses `transport`).

## Quick start

```sh
cargo test --workspace                  # every test; no audio hardware needed
cargo run -p sine_through_graph         # offline render to sine_through_graph.wav
cargo run -p session_playback           # session clips through the mixer to WAV
cargo run -p session_undo               # a project document with undo, saved as JSON
cargo run -p live_graph_edit            # swap graphs into a running block loop
cargo run -p midi_route                 # MIDI routing and sample-accurate note placement
cargo run -p automation_gain_ramp       # an envelope driving a shared parameter
cargo run -p decode_bounce              # bounce, decode, resample, export (files feature)
cargo run -p clap_host_scan -- [dir]    # find and run a CLAP plugin offline
cargo run -p play_sine_device --features devices   # real output device
cargo run -p midi_monitor --features midi-io       # real MIDI input
```

Every example is a short, commented program; see
[examples/README.md](examples/README.md).

```rust
use superb::prelude::*;

let mut mixer = Mixer::new(2);
let track = mixer.add_track("lead");
let osc = mixer.graph_mut().add_node(Box::new(SineNode::new(440.0, 0.5, 2)));
mixer.connect_source(osc, track)?;
mixer.track_params(track)?.set_gain_db(-6.0);

let mut engine = Engine::new(mixer, Transport::new(SampleRate::HZ_48K, 120.0));
engine.transport_mut().play();
let audio = superb::io::offline::render(&mut engine, SampleRate::HZ_48K, 256, 48_000)?;
```

## Cargo features

Everything that touches hardware or a copyleft dependency is opt-in, so the
default build and test run need nothing but Rust.

| feature (on `superb`) | enables |
|---|---|
| `devices` | audio input and output streams through cpal (`superb-io::device`) |
| `midi-io` | hardware MIDI input through midir (`superb-midi::input`) |
| `hardware` | both of the above |
| `files` | audio file decoding (WAV, FLAC), resampling and bounce helpers (`superb-io::files`) |

On Debian or Ubuntu the hardware features need `pkg-config libasound2-dev`.

## Real-time discipline

Everything on the audio path is allocated in `prepare` and never afterwards.
`superb-rt` ships a counting global allocator, and integration tests in
`superb-graph` and `superb-mixer` prove that `process` performs zero heap
operations, including while parameters move and while a new graph is
swapped in. [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) spells out the
rules.

## Status

Working today: offline rendering and WAV output; device output and capture;
MIDI messages, per-block buffers and hardware input; a processing graph with
silence propagation, latency compensation, per-node MIDI routing and
lock-free live editing; a mixer with tracks, buses, sends, pan laws and
meters; automation envelopes and modes; a project document with clips,
markers, routing, JSON files and undo; CLAP plugin hosting; a bridge from
the document onto the mixer with offline clip playback; and file decoding,
resampling and bouncing behind the `files` feature.

Not yet: recording into clips, duplex device streams, aligning hardware
MIDI timestamps with the audio clock, updating a running mixer
incrementally as the document changes, VST3 hosting, SysEx and Standard
MIDI Files, and encoders other than WAV.

## Licence

Apache-2.0 OR MIT, at your option. All code in this repository is original
work written for Superb. Every dependency in the default build is
Apache-2.0, MIT or ISC. The one MPL-2.0 dependency, the symphonia decoder,
is opt-in behind the `files` feature and confined to a single source file;
a test fails if it leaks anywhere else. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the policy.
