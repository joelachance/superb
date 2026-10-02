# mix

The mix beyond the strip: buses, sends, meters.

## Purpose

Faders, pan, mute and solo are a track's state and live in
[`track`](track.md). `mix` holds what connects tracks: buses, sends into
them, and the meters that show the result. A send's level is an edit
(document + live send, coalescing); whether a send is on is state.

## Verbs

| verb | does | undo |
|---|---|---|
| `add_bus(session, name) -> TrackId` | a Bus track; route tracks into it with `track::route` | one |
| `add_send(session, track, bus, level_db, SendTap) -> SendId` | a copy of a track into a bus, tapped pre- or post-fader; rebuilds | one |
| `remove_send(session, send)` | rebuilds | one |
| `level(session, send, db)` | document + live send | drag (coalesces) |
| `enable(session, send, on)` | state; document + live send | — |
| `peak(session, track) -> Option<[f32; 2]>` | the strip's peak meter, read and reset; `None` before the first rebuild | |

## Usage

```rust
use superb::prelude::*; // SendTap
use superb::{mix, session, track};

let room = mix::add_bus(&mut session, "room")?;
let send = mix::add_send(&mut session, vox, room, -6.0, SendTap::PostFader)?;
track::route(&mut session, gtr, Some(room))?;   // gtr goes through the bus instead
mix::level(&mut session, send, -12.0)?;         // heard at once, one undo step per drag
mix::enable(&mut session, send, false)?;        // state: saved, not undoable
if let Some([l, r]) = mix::peak(&session, room) {
    println!("room {l:.2} {r:.2}");
}
```

## Made of

`add_bus`: `track::add` with `TrackKind::Bus`. `add_send`:
`Document::allocate_send_id` → `TrackSend::with_id` → `AddSend` through
`session::structural`. `level`: `set_send_level_db` through `session::edit`
→ `SendParams::set_level_db`. `enable`: `SendState::enabled` through
`session::set_state` → `SendParams::set_enabled`. `peak`:
`StripParams::take_peak` for channels 0 and 1.

## Notes

* Meters hold the peak since the last read on the first two channels.
* Buses are tracks of kind `Bus`, so `track::volume`, `track::mute` and
  `track::route` work on them.
