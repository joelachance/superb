# transport

The transport bar: start, stop, seek, loop, and where the playhead is.

## Purpose

Every function here sends a command to the running engine, which applies
it at the start of its next block, or reads what the engine published after
its last block. Nothing here touches the document.

## Verbs

| verb | does |
|---|---|
| `start(session)` | play from where the playhead is; rebuilds the mixer first if the document changed |
| `stop(session)` | |
| `seek(session, to)` | move the playhead, stopped or rolling |
| `rewind(session)` | seek to zero |
| `set_loop_enabled(session, on)` | loop the range set with `session::set_loop` |
| `playhead(session) -> TimePosition` | where the playhead is |
| `state(session) -> TransportState` | Stopped, Playing or Recording |
| `is_rolling(session)` | playing or recording |

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

`EngineHandle::send(EngineCommand::{Play, Stop, Seek, SetLoopEnabled})`;
`start` runs `session::steps::rebuild` first when `needs_rebuild` is set.
`playhead`, `state` and `is_rolling` read the engine's atomics.

## Notes

* Commands apply at the engine's next block; read `state` after the next
  `render_blocks` (or a few milliseconds later on a device).
* `sync` is reserved for a future transport primitive that aligns the
  playhead and incoming events to an external clock; flows will call it
  themselves.
