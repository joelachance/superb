# play

Hearing the session: build the console from the document, drive the engine.

## Purpose

`play` is where the document meets the engine. `rebuild` turns the `Document`
into a `Mixer` full of source nodes and inserts, publishes it to the
`EngineHandle`, and keeps the control-side handles in `session.live`. The
transport verbs send commands to the running engine. Without an audio
device, `drive` renders blocks by hand.

## Verbs

| verb | does |
|---|---|
| `rebuild(session)` | rebuild the console from the session and swap it in |
| `start(session)` | rebuild if needed, then `Play` |
| `stop(session)` / `seek(session, to)` / `rewind(session)` | transport commands |
| `set_loop_enabled(session, on)` | loop the range set with `session::set_loop` |
| `position(session)` / `state(session)` / `is_rolling(session)` | read the engine's atomics |
| `drive(session, blocks) -> PlanarBuffer` | tick the engine loop `blocks` times and return the audio; collects a running take |
| `start_output(session, &OutputConfig)` (feature `devices`) | move the engine loop into a device stream; `drive` is then unavailable |

## Steps (`play::steps`)

| step | does |
|---|---|
| `build_mixer(&session, &cache, channels) -> (Mixer, Wiring)` | strips, buses, sends, clip sources, live instruments for monitoring MIDI tracks |
| `attach_inserts(&session, &mut PluginLoader, &mut Mixer, &Wiring) -> (Vec<LiveInsert>, failed)` | instantiate and chain each track's inserts; failures are returned, not fatal |
| `collect_handles(&Mixer, &Wiring) -> (StripHandles, SendHandles)` | the atomics the control side keeps |
| `publish(&mut EngineHandle, Mixer)` | prepare and hand the graph to the engine |

`session::bridge` (`MixerBridge`: the session track ↔ mixer strip
map, with routing and parameter mirroring) and `session::wiring` (`Wiring`,
`WiredClip`, `schedule_midi_notes`: a node per clip) are the pieces the
steps use; `AudioCache` lives in `primitives::io::cache`. The
`session_steps` example builds a console from them without a `Session`.

## Usage

```rust
use superb::prelude::*;
use superb::{session, transport};

let mut session = session::create("session", SampleRate::HZ_48K, 256)?;
// ... add tracks and clips ...
transport::start(&mut session)?;
let audio = session::steps::render_blocks(&mut session, 47)?;          // ~0.25 s at 48 kHz
println!("{:?} at {} samples", transport::state(&session), transport::playhead(&session).samples());
transport::seek(&mut session, TimePosition::from_seconds(1.0, SampleRate::HZ_48K))?;
transport::stop(&mut session)?;
```

With the `devices` feature:

```rust
let speakers = superb::devices::output_config(&session, None);
devices::output(&mut session, &speakers)?;   // the device now drives the engine
transport::start(&mut session)?;
```

## Made of

`rebuild`: `EngineHandle::reclaim` → `build_mixer` → `attach_inserts` →
`collect_handles` → `publish`, then `LiveGraph::apply_pending` while the
loop is still in the session. `start` and the transport verbs:
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
  `session.live.failed_inserts` (or `plugins::failures`).
