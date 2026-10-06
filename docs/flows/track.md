# track

Tracks: add them, name them, set their state, read their meters.

## Purpose

A track's *state* is what its strip shows: volume, pan, mute, solo, arm,
monitor, and where its output goes. It lives in the memory as
`Track.state: TrackState` and is saved with the session. Mute, solo, arm
and monitor are switches in the UI sense: heard at the next block, never
undo steps.
Volume and pan are continuous: they coalesce in the undo history and reach
the live strip at once. Adding, removing and routing change the mixer's
shape, so they rebuild it.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, name, kind) -> TrackId` | an Audio, Midi or Bus track | one |
| `remove(session, track)` | with everything on it; undo brings it back | one |
| `rename(session, track, name)` | | quick renames merge |
| `volume(session, track, db)` | the fader, in dB; memory + live strip | drag (coalesces) |
| `pan(session, track, pan)` | `-1.0` left to `1.0` right | drag |
| `mute(session, track, on)` / `solo(session, track, on)` | state; memory + live strip | — |
| `arm(session, track, on)` | state; audio tracks only; armed tracks get a clip when a recording stops | — |
| `monitor(session, track, on)` | state; a MIDI track carries a live instrument, an audio track plays the session's audio input (`devices::input`) on the channel(s) its input names; not for buses; rebuilds | — |
| `input(session, track, Option<Input>)` | state; audio tracks only; which input channel(s) feed the track when it records: `Input::Mono(0)`, `Input::Stereo(0, 1)`, or `None` for everything captured | — |
| `route(session, track, Some(bus) or None)` | output into a bus or the master; loops refused | one |
| `peak(session, track) -> Option<[f32; 2]>` | the strip's peak meter, read and reset; `None` before the first rebuild | |

## Usage

```rust
use superb::prelude::*;
use superb::{session, track};

let mut session = session::create("demo.superb", SampleRate::HZ_48K, 256)?;
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
let keys = track::add(&mut session, "keys", TrackKind::Midi)?;
for db in [-1.0, -2.0, -3.0] {
    track::volume(&mut session, vox, db)?;   // one undo step, heard at once
}
track::mute(&mut session, vox, true)?;       // state: saved, not undoable
track::arm(&mut session, vox, true)?;
track::input(&mut session, vox, Some(Input::Mono(0)))?;   // records the first input, mono
track::monitor(&mut session, keys, true)?;   // the keys track now sounds when MIDI arrives
track::monitor(&mut session, vox, true)?;    // and vox plays the audio input (hear yourself)
assert_eq!(session.memory.track(vox)?.state.volume_db, -3.0);
```

## Made of

`add`: `Memory::allocate_track_id` → `Track::with_id` → `AddTrack`
through `session::structural`. `volume` / `pan`: `set_track_volume_db` /
`set_track_pan` through `session::edit`, then `StripParams::set_volume_db`
/ `set_pan` on the live strip. `mute` / `solo` / `arm` / `monitor`:
`TrackState::{mute, solo, arm, monitor, input}` through `session::set_state`,
then `StripParams::set_mute` / `set_solo` for the first two and a rebuild
for `monitor` (a `MonoSynthNode` or a `LiveInputNode` on the strip). `route`: `SetTrackOutput` through `session::structural`.
`peak`: `StripParams::take_peak` for channels 0 and 1.

## Notes

* Meters hold the peak since the last read on the first two channels.
* Pan law is a live-strip setting (`StripParams::set_pan_law`), not in the
  memory; tracks default to constant power (−3 dB centre), buses to
  balance.
* Solo is a shared counter across strips: soloing one track silences every
  other track; buses ignore solo.
* The instrument a monitoring MIDI track gets is `MonoSynthNode`, a simple
  synth for hearing that MIDI arrives; instrument plugins come later.
* Monitoring an audio track needs the audio input open
  (`devices::input`); without it the tap is silent.
