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
| `state(session) -> TransportState` | Stopped, Playing or Recording; a recording with punch points switches between the last two on its own |

## Reference

Every function of the `transport` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `start`

```rust
pub fn start(session: &mut Session) -> Result<()>
```

Starts playing from where the playhead is. Rebuilds the mixer first if the
memory changed since it was built.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<()>` — nothing

**Errors** the mixer cannot be built (the memory names audio that is not in memory, say); the command queue is full.

**Undo** none: not an edit.

**Made of** [`session::steps::rebuild`](crate::session::steps::rebuild) when needed → `EngineHandle::send(Play)`.

**Example**

```rust
transport::start(&mut session)?;
```

### `stop`

```rust
pub fn stop(session: &mut Session) -> Result<()>
```

Stops the transport where it is.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<()>` — nothing

**Errors** the command queue is full.

**Undo** none: not an edit.

**Made of** `EngineHandle::send(Stop)`.

**Example**

```rust
transport::stop(&mut session)?;
```

### `seek`

```rust
pub fn seek(session: &mut Session, to: TimePosition) -> Result<()>
```

Moves the playhead. Works while stopped or rolling.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `to: TimePosition` — where to put the playhead

**Returns** `Result<()>` — nothing

**Errors** the command queue is full.

**Undo** none: not an edit.

**Made of** `EngineHandle::send(Seek)`.

**Example**

```rust
transport::seek(&mut session, TimePosition::from_seconds(30.0, rate))?;
```

### `rewind`

```rust
pub fn rewind(session: &mut Session) -> Result<()>
```

Moves the playhead to the start of the session.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<()>` — nothing

**Errors** the command queue is full.

**Undo** none: not an edit.

**Made of** [`seek`] to zero.

**Example**

```rust
transport::rewind(&mut session)?;
```

### `looping`

```rust
pub fn looping(session: &mut Session, on: bool) -> Result<()>
```

Turns looping over the session's loop range on or off (set the range with
[`session::set_loop`](crate::session::set_loop)). State, like mute: saved
with the session, applied at the engine's next block, not an undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** the command queue is full.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`).

**Made of** `Memory::set_looping` through `session::set_state` → `EngineHandle::send(SetLoopEnabled)`.

**Example**

```rust
transport::looping(&mut session, true)?;
```

### `playhead`

```rust
pub fn playhead(session: &Session) -> TimePosition
```

Where the playhead is: the position after the last block the engine
rendered.

**Takes**
- `session: &Session` — the open session, borrowed

**Returns** `TimePosition` — where the playhead is after the last block the engine rendered, in samples

**Errors** never; a query.

**Undo** none.

**Example**

```rust
let seconds = transport::playhead(&session).to_seconds(rate);
```

### `state`

```rust
pub fn state(session: &Session) -> TransportState
```

The transport state after the last block the engine rendered.

**Takes**
- `session: &Session` — the open session, borrowed

**Returns** `TransportState` — `TransportState::Stopped`, `Playing` or `Recording`, as of the last rendered block

**Errors** never; a query.

**Undo** none.

**Example**

```rust
if transport::state(&session).is_rolling() { /* draw a moving playhead */ }
```

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
the memory's tempo map, loop range and looping switch to the engine, so an
opened session plays at its tempo and meter and loops the way it was
saved. `playhead` and `state` read the
engine's atomics.

## Notes

* Commands apply at the engine's next block; read `state` after the next
  `render_blocks` (or a few milliseconds later on a device).
* `sync` is reserved for a future transport primitive that aligns the
  playhead and incoming events to an external clock; flows will call it
  themselves.
