# transport

The transport bar: start, stop, seek, loop, and where the playhead is.

## Purpose

Every function here sends a command to the running engine, which applies
it at the start of its next block, or reads what the engine published after
its last block. Only `looping` touches the memory: whether playback loops
is saved with the session, like a track's mute.

## Verbs

| verb | does |
|---|---|
| `start(session)` | play from where the playhead is; rebuilds the mixer first if the memory changed |
| `stop(session)` | |
| `seek(session, to)` | move the playhead, stopped or rolling |
| `rewind(session)` | seek to zero |
| `looping(session, on)` | loop over the range set with `session::set_loop`; state: saved, not an undo step |
| `playhead(session) -> TimePosition` | where the playhead is |
| `state(session) -> TransportState` | Stopped, Playing or Recording |

## Usage

```rust
use superb::prelude::*;
use superb::{session, transport};

transport::start(&mut session)?;
let audio = session::steps::render_blocks(&mut session, 47)?;   // ~0.25 s at 48 kHz, no device
println!("{:?} at {} samples", transport::state(&session), transport::playhead(&session).samples());
transport::seek(&mut session, TimePosition::from_seconds(1.0, SampleRate::HZ_48K))?;
transport::stop(&mut session)?;
```

With the `devices` feature the same session plays through the speakers:

```rust
superb::devices::output(&mut session, None)?;   // the default output now drives the engine
transport::start(&mut session)?;
```

## Made of

`EngineHandle::send(EngineCommand::{Play, Stop, Seek})`; `looping`:
`Memory::set_looping` through `session::set_state`, then
`EngineHandle::send(SetLoopEnabled)`. `start` runs `session::steps::rebuild`
first when the memory changed since the last build; a rebuild also sends
the memory's loop range and looping switch to the engine, so an opened
session loops the way it was saved. `playhead` and `state` read the
engine's atomics.

## Notes

* Commands apply at the engine's next block; read `state` after the next
  `render_blocks` (or a few milliseconds later on a device).
* `sync` is reserved for a future transport primitive that aligns the
  playhead and incoming events to an external clock; flows will call it
  themselves.
