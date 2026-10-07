# send

Sends: a copy of a track into a bus.

## Purpose

A bus is a track of kind `Bus` (`track::add`); `track::route` sends a whole
track through one. A *send* feeds a bus a copy of a track instead, tapped
before or after the fader, so the track is heard both directly and through
the bus (the classic reverb return). Adding or removing a send changes the
mixer's shape and rebuilds it. A send's level is an edit (memory + live
send, coalescing); whether a send is on is state.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, bus, level_db, SendTap) -> SendId` | a copy of a track into a bus, tapped pre- or post-fader; rebuilds | one |
| `remove(session, send)` | rebuilds | one |
| `level(session, send, db)` | memory + live send | drag (coalesces) |
| `enable(session, send, on)` | state; memory + live send | — |

## Reference

Every function of the `send` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `add`

```rust
pub fn add( session: &mut Session, track: TrackId, bus: TrackId, level_db: f32, tap: SendTap, ) -> Result<SendId>
```

Adds a send from a track into a bus at `level_db`, tapped before or after
the fader (`SendTap::PreFader` / `SendTap::PostFader`).

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `bus: TrackId` — the bus track that receives the copy
- `level_db: f32` — the send level in decibels
- `tap: SendTap` — `SendTap::PostFader` (after the strip's fader and pan) or `SendTap::PreFader` (before)

**Returns** `Result<SendId>` — the new send's id

**Errors** an unknown track or bus; a target that is not a bus; the master as source or target; a bus sending to itself.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `Memory::allocate_send_id` → `TrackSend::with_id` → `AddSend` through `session::structural`; rebuilds the mixer.

**Example**

```rust
let s = send::add(&mut session, vox, verb, -12.0, SendTap::PostFader)?;
```

### `remove`

```rust
pub fn remove(session: &mut Session, send: SendId) -> Result<()>
```

Removes a send.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `send: SendId` — the send's id, from `send::add` or `track.sends()`

**Returns** `Result<()>` — nothing

**Errors** an unknown send.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `RemoveSend` through `session::structural`; rebuilds the mixer.

**Example**

```rust
send::remove(&mut session, s)?;
```

### `level`

```rust
pub fn level(session: &mut Session, send: SendId, db: f32) -> Result<()>
```

Sets a send's level in dB. Moves within [`Session::COALESCE_WINDOW`] of each
other are one undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `send: SendId` — the send's id, from `send::add` or `track.sends()`
- `db: f32` — a level in decibels; `0.0` is unity, negative is quieter

**Returns** `Result<()>` — nothing

**Errors** an unknown send.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; the live send hears it at once.

**Made of** `set_send_level_db` through `session::edit` → `SendParams::set_level_db` on the live send.

**Example**

```rust
send::level(&mut session, s, -6.0)?;
```

### `enable`

```rust
pub fn enable(session: &mut Session, send: SendId, on: bool) -> Result<()>
```

Switches a send on or off. State, like mute: saved, heard at the next block,
not an undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `send: SendId` — the send's id, from `send::add` or `track.sends()`
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** an unknown send.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); the live send hears it at once.

**Made of** `SendState::enabled` through `session::set_state` → `SendParams::set_enabled` on the live send.

**Example**

```rust
send::enable(&mut session, s, false)?;
```

## Usage

```rust
use superb::prelude::*; // SendTap, TrackKind
use superb::{send, session, track};

let room = track::add(&mut session, "room", TrackKind::Bus)?;
let vox_room = send::add(&mut session, vox, room, -6.0, SendTap::PostFader)?;
send::level(&mut session, vox_room, -12.0)?;      // heard at once, one undo step per drag
send::enable(&mut session, vox_room, false)?;     // state: saved, not undoable
track::route(&mut session, gtr, Some(room))?;     // gtr goes through the bus instead
```

## Made of

`add`: `Memory::allocate_send_id` → `TrackSend::with_id` → `AddSend`
through `session::structural`. `remove`: `RemoveSend` through
`session::structural`. `level`: `set_send_level_db` through `session::edit`
→ `SendParams::set_level_db`. `enable`: `SendState::enabled` through
`session::set_state` → `SendParams::set_enabled`.

## Notes

* Buses are tracks, so `track::volume`, `track::mute`, `track::route` and
  `track::peak` work on them.
* A send into a bus that feeds the sending track's own output chain is
  fine; a bus routed into itself is refused by `track::route`.
