# send

Sends: a copy of a track into a bus.

## Purpose

A bus is a track of kind `Bus` (`track::add`); `track::route` sends a whole
track through one. A *send* feeds a bus a copy of a track instead, tapped
before or after the fader, so the track is heard both directly and through
the bus (the classic reverb return). Adding or removing a send changes the
mixer's shape and rebuilds it. A send's level is an edit (document + live
send, coalescing); whether a send is on is state.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, bus, level_db, SendTap) -> SendId` | a copy of a track into a bus, tapped pre- or post-fader; rebuilds | one |
| `remove(session, send)` | rebuilds | one |
| `level(session, send, db)` | document + live send | drag (coalesces) |
| `enable(session, send, on)` | state; document + live send | — |

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

`add`: `Document::allocate_send_id` → `TrackSend::with_id` → `AddSend`
through `session::structural`. `remove`: `RemoveSend` through
`session::structural`. `level`: `set_send_level_db` through `session::edit`
→ `SendParams::set_level_db`. `enable`: `SendState::enabled` through
`session::set_state` → `SendParams::set_enabled`.

## Notes

* Buses are tracks, so `track::volume`, `track::mute`, `track::route` and
  `track::peak` work on them.
* A send into a bus that feeds the sending track's own output chain is
  fine; a bus routed into itself is refused by `track::route`.
