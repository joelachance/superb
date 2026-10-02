# mix

Shaping the sound: faders, pan, mute and solo, buses and sends, meters.

## Purpose

Level changes take two paths at once: into the document through undo and
onto the live strip's atomics, so they are heard on the next block without
a rebuild. Changing the console's shape (a bus, a send, a route) is a
structural edit and marks the session for a rebuild.

## Verbs

| verb | reaches | undo |
|---|---|---|
| `set_gain_db(session, track, db)` | document + live strip | drag (coalesces) |
| `set_pan(session, track, -1.0..=1.0)` | document + live strip | drag |
| `set_mute(session, track, on)` / `set_solo(session, track, on)` | document + live strip | switch (not undoable) |
| `add_bus(session, name) -> TrackId` | document; needs rebuild | one |
| `route(session, track, Some(bus) or None)` | document; needs rebuild; loops refused | one |
| `add_send(session, track, bus, level_db, SendTap) -> SendId` / `remove_send` | document; needs rebuild | one |
| `set_send_level_db(session, send, db)` | document + live send | drag |
| `set_send_enabled(session, send, on)` | document + live send | switch (not undoable) |
| `peak(session, track) -> Option<[f32; 2]>` | live strip meter, read and reset | — |

Buses are tracks of kind `Bus`; `add_bus` is `track::add` with that
kind. `track` arguments accept a bus's ID wherever a strip makes sense.

## Usage

```rust
use superb::prelude::*; // SendTap
use superb::{mix, session, transport};

let room = mix::add_bus(&mut session, "room")?;
let send = mix::add_send(&mut session, vox, room, -6.0, SendTap::PostFader)?;
session::steps::rebuild(&mut session)?;                                          // the console has the bus now

for db in [-1.0, -2.0, -3.0] {
    track::volume(&mut session, vox, db)?;                       // heard immediately
}
// Moves within half a second merged into one undo step; a UI may also
// call `session.history.seal()` on mouse up.
track::mute(&mut session, vox, true)?;                            // a switch: saved, not undoable
if let Some([l, r]) = mix::peak(&session, room) {
    println!("room {l:.2} {r:.2}");
}
```

## Made of

Level verbs: the matching `set_track_*` / `set_send_*` command from
`primitives::session::commands` pushed through undo, then
`StripParams::set_*` or `SendParams::set_*` on the handle in `session.live`.
Structural verbs: `AddSend` / `RemoveSend` / `SetTrackOutput` commands and
`session.needs_rebuild = true`. `peak`: `StripParams::take_peak` for channels 0
and 1.

## Notes

* Pan law is a live-strip setting (`StripParams::set_pan_law`) and not in
  the session; tracks default to constant power (−3 dB centre), buses to
  balance.
* Solo is a shared counter across strips: soloing one track silences every
  other track; buses ignore solo.
* Meters hold the peak since the last read on the first two channels.
