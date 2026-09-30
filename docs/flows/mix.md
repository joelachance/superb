# mix

Shaping the sound: faders, pan, mute and solo, buses and sends, meters.

## Purpose

Level changes take two paths at once: into the document through undo and
onto the live strip's atomics, so they are heard on the next block without
a sync. Changing the console's shape (a bus, a send, a route) is a
structural edit and marks the rig for a sync.

## Verbs

| verb | reaches | undo |
|---|---|---|
| `set_gain_db(rig, track, db)` | document + live strip | drag (coalesces) |
| `set_pan(rig, track, -1.0..=1.0)` | document + live strip | drag |
| `set_mute(rig, track, on)` / `set_solo(rig, track, on)` | document + live strip | switch |
| `add_bus(rig, name) -> TrackId` | document; needs sync | one |
| `route(rig, track, Some(bus) or None)` | document; needs sync; loops refused | one |
| `add_send(rig, track, bus, level_db, pre_fader) -> SendId` / `remove_send` | document; needs sync | one |
| `set_send_level_db(rig, send, db)` | document + live send | drag |
| `set_send_enabled(rig, send, on)` | document + live send | switch |
| `peak(rig, track) -> Option<[f32; 2]>` | live strip meter, read and reset | — |
| `finish_gesture(rig)` | re-export of `project::finish_gesture` | |

Buses are tracks of kind `Bus`; `add_bus` is `project::add_track` with that
kind. `track` arguments accept a bus's ID wherever a strip makes sense.

## Usage

```rust
use superb::{mix, play, project};

let room = mix::add_bus(&mut rig, "room")?;
let send = mix::add_send(&mut rig, vox, room, -6.0, false)?;   // post-fader
play::sync(&mut rig)?;                                          // the console has the bus now

for db in [-1.0, -2.0, -3.0] {
    mix::set_gain_db(&mut rig, vox, db)?;                       // heard immediately
}
mix::finish_gesture(&mut rig);                                  // one undo step
mix::set_mute(&mut rig, vox, true)?;                            // its own undo step
if let Some([l, r]) = mix::peak(&rig, room) {
    println!("room {l:.2} {r:.2}");
}
```

## Made of

Level verbs: the matching `set_track_*` / `set_send_*` command from
`primitives::session::commands` pushed through undo, then
`StripParams::set_*` or `SendParams::set_*` on the handle in `rig.live`.
Structural verbs: `AddSend` / `RemoveSend` / `SetTrackOutput` commands and
`rig.needs_sync = true`. `peak`: `StripParams::take_peak` for channels 0
and 1.

## Notes

* Pan law is a live-strip setting (`StripParams::set_pan_law`) and not in
  the song; tracks default to constant power (−3 dB centre), buses to
  balance.
* Solo is a shared counter across strips: soloing one track silences every
  other track; buses ignore solo.
* Meters hold the peak since the last read on the first two channels.
