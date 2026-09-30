# superb

The facade crate of the Superb DAW SDK.

## Purpose

`superb` sits at the top of the dependency chain (core → buffers/rt →
transport → midi / automation → graph → mixer → io, with undo → session
beside it): it depends on every other crate and re-exports each as a
module, so an application can depend on `superb`
alone instead of naming every crate individually. Beyond re-exports it
owns a thin `superb::bridge` module that maps session document tracks
onto mixer strips/buses and mirrors the document's routing (outputs,
sends) onto the mixer, and `superb::playback`. The curated `prelude`
covers the day-to-day types (buffers, IDs/time, graph/nodes, the engine,
MIDI, mixer, automation, session/undo, RT primitives, transport).

## Key types

Re-export table (from the crate doc comment):

| module | crate | purpose |
|---|---|---|
| `core` | `superb-core` | sample-accurate time, beats, IDs, errors |
| `buffers` | `superb-buffers` | planar / interleaved `f32` blocks |
| `rt` | `superb-rt` | SPSC queues, atomics, no-alloc proofs, load meter |
| `transport` | `superb-transport` | playhead, tempo map, loop segmentation |
| `midi` | `superb-midi` | MIDI messages, block event buffer, midir bridge |
| `graph` | `superb-graph` | process graph, `Node` trait, fixture nodes |
| `mixer` | `superb-mixer` | tracks, buses, sends, pan, mute/solo, meters |
| `automation` | `superb-automation` | envelopes, read/touch/latch/write, sample-accurate apply |
| `undo` | `superb-undo` | coalescing undo/redo command stack, groups, clean-state |
| `session` | `superb-session` | document model: tracks, items, markers, tempo map, JSON snapshots |
| `plugins` | `superb-plugins` | plugin instance model, `PluginNode`, CLAP host (`clap-sys`), VST3 stub |
| `io` | `superb-io` | engine loop, offline render, WAV, file decode/resample/bounce (`files`), cpal input/output |
| `bridge` | (facade) | session → mixer strip/bus map, strip-param mirror, output/send routing mirror |
| `playback` | (facade) | session item → strip wiring + offline region render |

`superb::prelude::*` brings in the day-to-day types from all of them:
`PlanarBuffer`/`InterleavedBuffer`; `SampleRate`, `TimePosition`,
`TimeDuration`, `BeatPosition`, `TimeSignature`, `TrackId`/`BusId`/`NodeId`/
`SendId`, `Result`/`SuperbError`; `AudioGraph`, `Node`, `NodeChannels`,
`ProcessContext`, `ProcessStatus`, `BlockProcessor`, `BlockTime`, and the
fixture nodes `SineNode`/`GainNode`/`MonoSynthNode`/`PassThroughNode`;
`Engine`/`EngineStats`; `MidiChannel`/`MidiEvent`/`MidiEventBuffer`/
`MidiMessage`/`TimestampedMessage`; `Mixer`/`PanLaw`/`SendParams`/
`SendTap`/`StripParams`; `AutomationLane`/`AutomationMode`/`Envelope`/
`EnvelopePoint`; `Session`/`SessionSnapshot`/`Track`/`TrackKind`/`TrackSend`/`Item`/
`ItemSource`/`Marker`; `UndoStack`/`Command`/`CoalesceKey`/`PropertyEdit`;
`PluginNode`/`PluginInstance`/`PluginDescriptor`; `AtomicF32`/`LoadMeter`;
and `Transport`/`TransportState`/`TempoMap`/`LoopRange`.

## Usage

```rust
use superb::prelude::*;

// A stereo mixer with one track fed by a sine, rendered offline.
let mut mixer = Mixer::new(2);
let track = mixer.add_track("lead");
let osc = mixer.graph_mut().add_node(Box::new(SineNode::new(440.0, 0.5, 2)));
mixer.connect_source(osc, track).unwrap();
mixer.track_params(track).unwrap().set_gain_db(-6.0);

let mut engine = Engine::new(mixer, Transport::new(SampleRate::HZ_48K, 120.0));
engine.transport_mut().play();
let audio = superb::io::offline::render(&mut engine, SampleRate::HZ_48K, 256, 48_000).unwrap();
assert_eq!(audio.frames(), 48_000);
assert!(audio.peak() > 0.1 && audio.peak() < 0.5);
```

## Session → mixer bridge

```rust
use superb::bridge::SessionMixerBridge;
use superb::prelude::*;

let mut session = Session::new("song", SampleRate::HZ_48K);
let lead = session.add_track("lead", TrackKind::Audio);
let verb = session.add_track("verb", TrackKind::Bus);
session.track_mut(lead).unwrap().gain_db = -3.0;
session.set_track_output(lead, Some(verb)).unwrap();               // lead → verb → master
let send = session.add_send(lead, TrackSend::new(verb).level_db(-12.0)).unwrap();

let mut mixer = Mixer::new(2);
let mut bridge = SessionMixerBridge::new();
bridge.sync(&session, &mut mixer);                       // creates mixer strips/buses
let edits = bridge.apply_routing(&session, &mut mixer).unwrap(); // outputs + sends
bridge.apply_strip_params(&session, &mixer);             // gain/pan/mute/solo + send levels
let strip = bridge.mixer_track(lead).unwrap();
assert_eq!(mixer.track(strip).unwrap().output, bridge.mixer_bus(verb).unwrap());
assert!(bridge.mixer_send(send).is_some());
assert_eq!(edits, 2); // one re-route, one send created → `prepare` before processing
```

`sync` is additive: it creates missing mixer objects and drops stale
bindings, but never removes mixer strips (so live audio is not cut).
`apply_routing` mirrors every track's `output()` (master when `None`) and
every `TrackSend` (destination, tap, level, enable); a send whose
destination or tap changed is re-created and mixer sends whose session
send vanished are removed. It returns the number of structural edits so
the caller knows a `prepare` is due. Item playback onto strips is handled
by `superb::playback` (see below); `SessionPlayer::wire` runs all three
steps.


## Session item playback

```rust
use superb::playback::{render_region, AudioCache, RenderOptions, SessionPlayer};
use superb::prelude::*;
use std::path::PathBuf;

let mut session = Session::new("song", SampleRate::HZ_48K);
let vox = session.add_track("vox", TrackKind::Audio);
let mut cache = AudioCache::new();
let mut buf = PlanarBuffer::new(2, 1000);
buf.fill(0.2);
cache.insert("take.wav", buf);
session.add_item(vox, Item::new(
    "take",
    TimePosition::ZERO,
    TimeDuration::from_samples(1000),
    ItemSource::Audio { path: PathBuf::from("take.wav"), offset: TimeDuration::ZERO },
).unwrap()).unwrap();

// One-shot: wire + offline render a region
let audio = render_region(
    &session, &cache, TimePosition::ZERO, 1000, &RenderOptions::default(),
).unwrap();
assert_eq!(audio.frames(), 1000);

// Or wire into an existing mixer yourself:
let mut mixer = Mixer::new(2);
let player = SessionPlayer::wire(&session, &mut mixer, &cache).unwrap();
assert!(!player.sources().is_empty());
```

`AudioCache` holds path → `Arc<PlanarBuffer>` entries (tests use synthetic
buffers; apps can fill via `superb_io::files::decode` / `AudioCache::load`
behind the `files` feature). `ItemSource::Empty` and missing cache paths
are silence; MIDI items become `ScheduledSynthNode`s.

## Real-time notes

The facade adds nothing on the audio thread — every `pub use` is a
zero-cost re-export, so the real-time guarantees are exactly those of the
underlying crate (`AudioGraph::process`, `Mixer::process` and
`Engine::render` are all allocation-free; see `docs/superb-graph.md`,
`docs/superb-mixer.md` and `docs/superb-io.md`). The one thing worth noting
here is feature selection: `devices`, `midi-io`, `hardware` and `files`
are all **off by default**. `cargo test -p superb` never needs `cpal`,
ALSA headers, `midir` or the MPL decoder; hardware access and the
symphonia file layer are opt-in per application (`files` is also pulled
in by the `decode_bounce` example via feature unification).

## Limitations / next

* **Feature flags gate hardware and the MPL file decoder** —
  `devices` enables `superb-io/devices` (cpal output), `midi-io` enables
  `superb-midi/midir` (hardware MIDI input), `hardware` is both, and
  `files` enables `superb-io/files` (symphonia decode + rubato resample +
  bounce). `AudioFile` is in the prelude only when `files` is on.
* **Not yet implemented** — duplex device streams (one stream for input
  and output), aligning hardware MIDI timestamps with the audio clock,
  incremental re-wiring of a running mixer when the session changes, and
  recording an armed track into a clip.
* The prelude is a fixed, curated list — crate-specific or less common
  types (e.g. `wav::WavFormat`, `device::OutputConfig`) still need an
  explicit `superb::io::...` / `superb::mixer::...` path, as the doctest's
  use of `superb::io::offline::render` shows.
