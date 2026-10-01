# play

Hearing the session: build the console from the document, drive the engine.

## Purpose

`play` is where the document meets the engine. `rebuild` turns the `Session`
into a `Mixer` full of source nodes and inserts, publishes it to the
`EngineHandle`, and keeps the control-side handles in `rig.live`. The
transport verbs send commands to the running engine. Without an audio
device, `drive` renders blocks by hand.

## Verbs

| verb | does |
|---|---|
| `rebuild(rig)` | rebuild the console from the session and swap it in |
| `start(rig)` | rebuild if needed, then `Play` |
| `stop(rig)` / `seek(rig, to)` / `rewind(rig)` | transport commands |
| `set_loop_enabled(rig, on)` | loop the range set with `session::set_loop` |
| `position(rig)` / `state(rig)` / `is_rolling(rig)` | read the engine's atomics |
| `drive(rig, blocks) -> PlanarBuffer` | tick the engine loop `blocks` times and return the audio; collects a running take |
| `start_output(rig, &OutputConfig)` (feature `devices`) | move the engine loop into a device stream; `drive` is then unavailable |

## Steps (`play::steps`)

| step | does |
|---|---|
| `build_console(&session, &cache, channels) -> (Mixer, Wiring)` | strips, buses, sends, clip sources, live instruments for monitoring MIDI tracks |
| `attach_inserts(&session, &mut PluginLoader, &mut Mixer, &Wiring) -> (Vec<LiveInsert>, failed)` | instantiate and chain each track's inserts; failures are returned, not fatal |
| `collect_handles(&Mixer, &Wiring) -> (StripHandles, SendHandles)` | the atomics the control side keeps |
| `publish(&mut EngineHandle, Mixer)` | prepare and hand the graph to the engine |

`play::bridge` (`SessionMixerBridge`: the session track ↔ mixer strip
map, with routing and parameter mirroring) and `play::wiring` (`Wiring`,
`WiredClip`, `schedule_midi_notes`: a node per clip) are the pieces the
steps use; `AudioCache` lives in `primitives::io::cache`. The
`console_steps` example builds a console from them without a `Rig`.

## Usage

```rust
use superb::prelude::*;
use superb::{play, session};

let mut rig = session::create("session", SampleRate::HZ_48K, 256)?;
// ... add tracks and clips ...
play::start(&mut rig)?;
let audio = play::drive(&mut rig, 47)?;          // ~0.25 s at 48 kHz
println!("{:?} at {} samples", play::state(&rig), play::position(&rig).samples());
play::seek(&mut rig, TimePosition::from_seconds(1.0, SampleRate::HZ_48K))?;
play::stop(&mut rig)?;
```

With the `devices` feature:

```rust
let speakers = superb::devices::output_config(&rig, None);
play::start_output(&mut rig, &speakers)?;   // the device now drives the engine
play::start(&mut rig)?;
```

## Made of

`rebuild`: `EngineHandle::reclaim` → `build_console` → `attach_inserts` →
`collect_handles` → `publish`, then `LiveGraph::apply_pending` while the
loop is still in the rig. `start` and the transport verbs:
`EngineHandle::send(EngineCommand::…)`. `drive`: `EngineLoop::tick` per
block plus `record::steps::collect`. `start_output`:
`EngineLoop::run_on_device`.

## Notes

* Commands apply at the engine's next block; read `state` after the next
  `drive` (or a few milliseconds later on a device).
* A rebuild builds a new console; plugin instances are recreated with their
  stored parameters and state, so reverb tails and meters reset when the
  console's shape changes. Level edits never cause a rebuild.
* `rebuild` succeeds even if a plugin fails to load; look at
  `rig.live.failed_inserts` (or `plugins::failures`).
