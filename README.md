# Superb

<!-- ANCHOR: intro -->
An open-source Rust toolkit for building a digital audio workstation (DAW).
One crate, `superb`, dual-licensed Apache-2.0 OR MIT.

Superb is the engine side of a DAW: sample-accurate time, audio buffers, a
real-time-safe processing graph, a mixer with tracks, buses, sends and
insert chains, MIDI, automation, the session's memory (tracks, clips,
markers, routing, tempo) with undo, CLAP plugin hosting, recording, offline
export, audio import, and audio device and file I/O. It has no user
interface; you bring your own.
<!-- ANCHOR_END: intro -->

## Two tiers

The crate is organised so you always know what a call will do.

**Flows** are top-level modules named after the things you work with and
the tasks you do. Each one is a group of free functions that borrow a
`Session` and do the whole job: `session`, `track`, `clip`, `marker`,
`transport`, `send`, `midi`, `record`, `metronome`, `export`, `devices`.
They are modules of functions, not classes: there is no object to
construct and no method to look up, and the `Session` is a plain struct
with public fields. Every flow function documents the primitive calls it
is *made of*, so when one does not do exactly what you want, you call
those primitives yourself.

**Primitives** live under `superb::primitives` and are the engine proper.
They know nothing about flows or the `Session`.

| flow | what it does | doc |
|---|---|---|
| `session` | create, open and save a session; import an audio file; the state file; tempo, meter and loop range; undo and redo; the engine side in `session::steps` | [session.md](docs/flows/session.md) |
| `track` | add, archive, restore, rename, reorder, colour; mute, solo, arm, monitor, input, MIDI channel; volume, pan, route; meters; the master | [track.md](docs/flows/track.md) |
| `clip` | audio or MIDI on a track: add, move, split, trim, gain, fades, mute, rename, archive, restore; notes in a MIDI clip | [clip.md](docs/flows/clip.md) |
| `marker` | named places on the timeline: add, move, rename, remove | [marker.md](docs/flows/marker.md) |
| `transport` | start, stop, seek, rewind, loop; where the playhead is | [transport.md](docs/flows/transport.md) |
| `send` | a copy of a track into a bus: add, remove, level, on/off | [send.md](docs/flows/send.md) |
| `midi` | play and release notes, send any message, connect a keyboard | [midi.md](docs/flows/midi.md) |
| `record` | start a recording, stop it, get a clip on every armed track, audio and MIDI; punch points | [record.md](docs/flows/record.md) |
| `metronome` | a click while the transport rolls, its level, bars of count-in before a recording | [metronome.md](docs/flows/metronome.md) |
| `export` | bounce the session, or a range of it, to WAV, optionally resampled | [export.md](docs/flows/export.md) |
| `devices` | list audio and MIDI hardware; play through an output, switch outputs, open an input | [devices.md](docs/flows/devices.md) |

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
| `primitives::session` | the memory | `Memory`: tracks and their state, clips, markers, routing, inserts, tempo map, JSON save/load, the state file, undoable commands | [session.md](docs/primitives/session.md) |
| `primitives::plugins` | plugin host | plugin instance model, `PluginNode`, CLAP host (VST3 is a stub) | [plugins.md](docs/primitives/plugins.md) |
| `primitives::io` | engine, devices, files | block loop, live engine with control handle, offline render, capture queue, WAV, decode/resample (feature `files`), device streams (feature `devices`) | [io.md](docs/primitives/io.md) |

[docs/flows.md](docs/flows.md) explains how the flows fit together (the
`Session`, edits versus state, how the mixer follows the memory).
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) covers the engine and the
rules every module follows. If the DAW words are new to you, start with
[docs/TERMINOLOGY.md](docs/TERMINOLOGY.md).

## Reading the documentation

<!-- ANCHOR: reading -->
Each flow has one page under `docs/flows/`, and every page has the same
shape, so once you have read one you can find your way around all of them:

* **Purpose**: what the group of functions is for.
* **Verbs**: a table of every function, what it does, and how it undoes.
* **Reference**: every function in full. The signature as it is in the
  source; what each argument is (*Takes*); what comes back (*Returns*);
  what makes it fail (*Errors*); whether it is one undo step, a coalescing
  edit, or state that never enters the history (*Undo*); the primitive
  calls it is made of (*Made of*); and a short example.
* **Types**: the structs and enums the flow defines, field by field, and
  the methods on them.
* **Usage**: a program that uses the whole group together.
* **Made of** and **Notes**: the primitives behind the group, and the
  things that surprise people.

A few conventions hold on every page, so they are stated here once:

* A function that changes something takes `&mut Session` first and borrows
  the session for the call. A query takes `&Session`.
* Tracks, clips, sends and markers are named by id (`TrackId`, `ClipId`,
  `SendId`, `MarkerId`). The id comes back from the function that made the
  thing, and the memory lists them (`session.memory.tracks()`).
* A function that can fail returns `Result<T>`: `Ok(T)` or a `SuperbError`,
  so `?` works. Its *Errors* line lists the mistakes you can make; a device
  or a file can always fail besides.
* Edits (adding, moving, levels) enter the undo history. State (mute, solo,
  arm, monitor, input, looping, the click, the playhead) does not, and is
  kept across runs only once `session::state_file` has named a file.
* Time on the timeline is in samples at the session's rate (`TimePosition`,
  `TimeDuration`); notes inside a MIDI clip are in beats from the clip's
  start.

`cargo doc --no-deps --open` builds the same text as rustdoc, with every
signature linked.
<!-- ANCHOR_END: reading -->

## Quick start

<!-- ANCHOR: quickstart -->
```rust
use superb::prelude::*;
use superb::{clip, session, track, transport};

let mut session = session::create("demo.superb", SampleRate::HZ_48K, 256)?;   // a folder
let lead = track::add(&mut session, "lead", TrackKind::Audio)?;

// Audio lives in memory: a decoded file, or a buffer you made.
let mut tone = PlanarBuffer::new(2, 48_000);
tone.fill(0.25);
session.audio.insert("tone.wav", tone);
clip::add(&mut session, lead, "tone.wav", TimePosition::ZERO)?;     // as long as the audio
track::volume(&mut session, lead, -6.0)?;

transport::start(&mut session)?;                                   // builds the mixer, starts playing
let audio = session::steps::render_blocks(&mut session, 8)?;       // 8 blocks, no audio device needed
assert!(audio.peak() > 0.1);
```

Every example is a short, commented program that runs without hardware
unless hardware is the point:

```sh
cargo test                                     # the whole suite, no audio hardware needed
cargo run --example new_session                # session, track, clip: undo, save and open
cargo run --example play_session               # transport: start, seek, loop, offline blocks
cargo run --example mix_session                # track and send: faders, a bus, sends, meters
cargo run --example midi_keys                  # midi: notes into a monitoring track
cargo run --example record_take                # record: a recording from a synthetic microphone
cargo run --example metronome_count_in         # metronome: the click, and a count-in before recording
cargo run --example export_session             # export: bounce to WAV, whole or a range
cargo run --example list_devices --features devices
```

<!-- ANCHOR_END: quickstart -->

See [examples/README.md](examples/README.md) for the full list, including
one example per primitive.

## Cargo features

<!-- ANCHOR: features -->
Everything that touches hardware or a copyleft dependency is opt-in, so the
default build and test run need nothing but Rust.

| feature | enables |
|---|---|
| `clap` (default) | CLAP plugin hosting through `clap-sys` and `libloading` |
| `devices` | audio input and output streams through cpal |
| `midi-io` | hardware MIDI input through midir |
| `hardware` | both of the above |
| `files` | audio file decoding (WAV, AIFF, FLAC, ALAC, MP3, AAC in MP4/M4A, Ogg Vorbis, CAF, Matroska), resampling, import of any of them at the session's rate, and bounce helpers |

Without `files`, `session::import_audio` reads WAV only, and only at the
session's sample rate; anything else is an error that names the feature.

On Debian or Ubuntu the hardware features need `pkg-config libasound2-dev`.
<!-- ANCHOR_END: features -->

## Real-time discipline

<!-- ANCHOR: realtime -->
Everything on the audio path is allocated in `prepare` and never afterwards.
`primitives::realtime` ships a counting global allocator, and the
integration tests under `tests/` prove that `process` performs zero heap
operations, including while parameters move and while a new graph is
swapped in under a running engine. Mixer changes made through the flows
are published as whole prepared graphs and swapped in between two blocks.
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) spells out the rules.
<!-- ANCHOR_END: realtime -->

## Status

<!-- ANCHOR: status -->
Working today, through the flows: the session's memory with tracks and
their state, a master track that is always there, clips, markers and
routing (tracks and clips are archived, never deleted), JSON files and
undo; an opt-in state file for mute, solo, arm, monitor, inputs, looping,
the click and the playhead; a mixer built from the memory with faders,
buses and sends, swapped glitch-free into a running engine; tempo and
meter changes the engine and the click follow; audio import in every
format the decoder reads, resampled once to the session's rate, with a
report of what came in; clip split, trim, gain and fades; notes added,
changed and removed in MIDI clips; bouncing to WAV; device output, output
switching, and input on every channel the device has; recording audio and
MIDI into clips, lined up by the device clock, written into the session
folder, with punch in and out and a metronome with count-in; hearing the
input and a live instrument on a track's strip; MIDI messages, a MIDI
channel per track, and hardware input. In the primitives only, for now:
insert chains and CLAP plugin hosting, automation envelopes and modes.

Not yet: a plugins flow, telling MIDI ports apart per track, loop
recording, tempo ramps (a tempo that glides rather than steps), duplex
device streams, aligning hardware MIDI timestamps with the audio clock,
updating a running mixer incrementally (a rebuild replaces it), VST3
hosting, SysEx and Standard MIDI Files, and encoders other than WAV.
<!-- ANCHOR_END: status -->

## Licence

Apache-2.0 OR MIT, at your option. All code in this repository is original
work written for Superb. Every dependency in the default build is
Apache-2.0, MIT or ISC. The one MPL-2.0 dependency, the symphonia decoder,
is opt-in behind the `files` feature and confined to a single source file;
a test fails if it leaks anywhere else. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the policy.
