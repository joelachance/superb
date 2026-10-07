# track

Tracks: add them, name them, set their state, read their meters, archive
them.

## Purpose

A track's *state* is what its strip shows: volume, pan, mute, solo, arm,
monitor, and where its output goes. It lives in the memory as
`Track.state: TrackState` and is saved with the session. Mute, solo, arm
and monitor are switches in the UI sense: heard at the next block, never
undo steps.
Volume and pan are continuous: they coalesce in the undo history and reach
the live strip at once. Adding, archiving and routing change the mixer's
shape, so they rebuild it. Nothing is deleted: a track that leaves is
archived, and comes back whole.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, name, kind) -> TrackId` | an Audio, Midi or Bus track | one |
| `archive(session, track)` | off the mixer, kept with its clips and files: no strip, silent, nothing routes into it, neither records nor monitors, not counted toward the session's end | one |
| `restore(session, track)` | back on the mixer | one |
| `rename(session, track, name)` | | quick renames merge |
| `volume(session, track, db)` | the fader, in dB; memory + live strip | drag (coalesces) |
| `pan(session, track, pan)` | `-1.0` left to `1.0` right | drag |
| `mute(session, track, on)` / `solo(session, track, on)` | state; memory + live strip | — |
| `arm(session, track, on)` | state; audio and MIDI tracks, not buses; an armed track gets a clip when a recording stops: the audio captured, or the notes that arrived | — |
| `monitor(session, track, on)` | state; a MIDI track carries a live instrument, an audio track plays the session's audio input (`devices::input`) on the channel(s) its input names; not for buses; rebuilds | — |
| `input(session, track, Option<Input>)` | state; audio tracks only; which input channel(s) feed the track when it records: `Input::Mono(0)`, `Input::Stereo(0, 1)`, or `None` for everything captured | — |
| `midi_channel(session, track, Option<u8>)` | state; MIDI tracks only; which MIDI channel (`0..=15`) feeds the track when it monitors and records, or `None` for all; rebuilds | — |
| `route(session, track, Some(bus) or None)` | output into a bus or the master; loops refused | one |
| `move_to(session, track, index)` | to another place in the order (0 first); nothing rebuilds | one |
| `color(session, track, Option<u32>)` | display colour as `0xRRGGBB`, or none | quick changes merge |
| `peak(session, track) -> Option<[f32; 2]>` | the strip's peak meter, read and reset; `None` before the first rebuild | |

## Reference

Every function of the `track` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `add`

```rust
pub fn add(session: &mut Session, name: &str, kind: TrackKind) -> Result<TrackId>
```

Adds a track at the end. `TrackKind::Bus` makes a bus.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `name: &str` — the track's name; recordings on it are named after it (`vox 1`)
- `kind: TrackKind` — `TrackKind::Audio`, `TrackKind::Midi` or `TrackKind::Bus`

**Returns** `Result<TrackId>` — the new track's id

**Errors** none in practice.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `Memory::allocate_track_id` → `Track::with_id` → push `AddTrack`; rebuilds the mixer.

**Example**

```rust
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
let keys = track::add(&mut session, "keys", TrackKind::Midi)?;
let verb = track::add(&mut session, "reverb", TrackKind::Bus)?;
```

### `archive`

```rust
pub fn archive(session: &mut Session, track: TrackId) -> Result<()>
```

Takes a track off the mixer without deleting it: it keeps its clips and
their files but gets no strip, so nothing on it plays, nothing routes into
it, it neither records nor monitors, and it no longer counts toward the
session's end. `restore` brings it back. Nothing is ever removed: this is
the one way a track leaves.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`

**Returns** `Result<()>` — nothing

**Errors** an unknown track; the master.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `ArchiveTrack` through `session::structural`.

**Example**

```rust
track::archive(&mut session, vox)?;
```

### `restore`

```rust
pub fn restore(session: &mut Session, track: TrackId) -> Result<()>
```

Puts an archived track back on the mixer.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`

**Returns** `Result<()>` — nothing

**Errors** an unknown track; the master.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `ArchiveTrack` (restoring) through `session::structural`.

**Example**

```rust
track::restore(&mut session, vox)?;
```

### `midi_channel`

```rust
pub fn midi_channel(session: &mut Session, track: TrackId, channel: Option<u8>) -> Result<()>
```

Says which MIDI channel feeds a MIDI track: `Some(0..=15)` for one channel,
`None` for all of them. Monitoring plays only that channel's notes and a
recording keeps only them; every connected port still counts. State, like
`input`; rebuilds, since the monitor instrument is built to listen.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `channel: Option<u8>` — `Some(0..=15)` for one MIDI channel (numbered from 0, so channel 1 is `Some(0)`); `None` for all

**Returns** `Result<()>` — nothing

**Errors** an audio track or bus; a channel above 15.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `TrackState::midi_channel` through `session::set_state`, then `session::changed_structure`.

**Example**

```rust
track::midi_channel(&mut session, keys, Some(0))?;   // channel 1 only
track::midi_channel(&mut session, keys, None)?;      // all channels
```

### `move_to`

```rust
pub fn move_to(session: &mut Session, track: TrackId, index: usize) -> Result<()>
```

Moves a track to `index` among the tracks (0 first). One undo step; the
mixer does not care about order, so nothing rebuilds.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `index: usize` — the new place in `session.memory.tracks()`, 0 first; archived tracks count

**Returns** `Result<()>` — nothing

**Errors** an index past the end; the master.

**Undo** one undo step.

**Made of** `MoveTrack` through `session::edit`.

**Example**

```rust
track::move_to(&mut session, keys, 0)?;   // first
```

### `color`

```rust
pub fn color(session: &mut Session, track: TrackId, color: Option<u32>) -> Result<()>
```

Sets a track's display colour as `0xRRGGBB`, or clears it. One undo step;
quick changes to one track merge.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `color: Option<u32>` — `Some(0xRRGGBB)` or `None` for no colour

**Returns** `Result<()>` — nothing

**Errors** an unknown track.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** `set_track_color` through `session::edit`.

**Example**

```rust
track::color(&mut session, vox, Some(0xff8800))?;
```

### `rename`

```rust
pub fn rename(session: &mut Session, track: TrackId, name: &str) -> Result<()>
```

Renames a track. Quick successive renames of one track are one undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `name: &str` — the new name

**Returns** `Result<()>` — nothing

**Errors** an unknown track.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** push `set_track_name`.

**Example**

```rust
track::rename(&mut session, vox, "lead vocal")?;
```

### `volume`

```rust
pub fn volume(session: &mut Session, track: TrackId, db: f32) -> Result<()>
```

Sets a track's or bus's volume: the fader, in dB. Moves within
`Session::COALESCE_WINDOW` of each other are one undo step, so a fader ride
undoes in one go.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `db: f32` — a level in decibels; `0.0` is unity, negative is quieter

**Returns** `Result<()>` — nothing

**Errors** an unknown track.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; the live strip hears it at once.

**Made of** push `set_track_volume_db` → `StripParams::set_volume_db` on the live strip.

**Example**

```rust
track::volume(&mut session, vox, -6.0)?;
```

### `pan`

```rust
pub fn pan(session: &mut Session, track: TrackId, pan: f32) -> Result<()>
```

Sets pan, `-1.0` (left) to `1.0` (right). An edit, like volume.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `pan: f32` — `-1.0` hard left to `1.0` hard right, `0.0` centre

**Returns** `Result<()>` — nothing

**Errors** an unknown track.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; the live strip hears it at once.

**Made of** push `set_track_pan` → `StripParams::set_pan` on the live strip.

**Example**

```rust
track::pan(&mut session, vox, -0.3)?;   // a little left
```

### `mute`

```rust
pub fn mute(session: &mut Session, track: TrackId, on: bool) -> Result<()>
```

Mutes or unmutes. State: saved with the session, heard at the next block,
not an undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** an unknown track.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); the live strip hears it at once.

**Made of** `TrackState::mute` through `session::set_state` → `StripParams::set_mute` on the live strip.

**Example**

```rust
track::mute(&mut session, vox, true)?;
```

### `solo`

```rust
pub fn solo(session: &mut Session, track: TrackId, on: bool) -> Result<()>
```

Solos or un-solos. While anything is soloed, every other track is silent;
buses keep passing what reaches them. State, like mute.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** an unknown track; the master.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); the live strip hears it at once.

**Made of** `TrackState::solo` through `session::set_state` → `StripParams::set_solo` on the live strip.

**Example**

```rust
track::solo(&mut session, vox, true)?;   // every other track falls silent
```

### `arm`

```rust
pub fn arm(session: &mut Session, track: TrackId, on: bool) -> Result<()>
```

Arms or disarms a track for recording. An armed audio track receives a clip
of the captured audio when a recording stops, an armed MIDI track a clip of
the notes that arrived meanwhile. State, like mute. Buses are not armed.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** a bus.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`).

**Made of** `TrackState::arm` through `session::set_state`.

**Example**

```rust
track::arm(&mut session, vox, true)?;   // the next recording lands here
```

### `monitor`

```rust
pub fn monitor(session: &mut Session, track: TrackId, on: bool) -> Result<()>
```

Turns monitoring on or off. A monitoring MIDI track carries a live
instrument that plays whatever MIDI reaches the engine. A monitoring audio
track plays the session's audio input (`devices::input`, feature `devices`)
through its strip, on the channel(s) its `input` names (the first two when
none is set), so you hear yourself while recording. State, saved with the
session; it changes the mixer's shape, so it rebuilds. Buses do not monitor.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** a bus.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `TrackState::monitor` through `session::set_state` → rebuild (the instrument or the input tap is a node in the mixer).

**Example**

```rust
track::monitor(&mut session, vox, true)?;   // hear the input on this strip
```

### `input`

```rust
pub fn input(session: &mut Session, track: TrackId, input: Option<Input>) -> Result<()>
```

Chooses which channel(s) of the audio input feed an audio track when it
records: `Input::Mono(0)` for the first input, `Input::Stereo(0, 1)` for the
first pair, `None` for everything the recording captures. State: saved with
the session, not an undo step. `record::start` captures as many channels as
the armed tracks' inputs need.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `input: Option<Input>` — `Some(Input::Mono(channel))`, `Some(Input::Stereo(left, right))` (0-based device channels) or `None` for everything the recording captures

**Returns** `Result<()>` — nothing

**Errors** a MIDI track or bus.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`).

**Made of** `TrackState::input` through `session::set_state`.

**Example**

```rust
track::input(&mut session, vox, Some(Input::Mono(0)))?;        // the first input channel
track::input(&mut session, gtr, Some(Input::Stereo(2, 3)))?;   // a stereo pair
```

### `route`

```rust
pub fn route(session: &mut Session, track: TrackId, bus: Option<TrackId>) -> Result<()>
```

Sends a track's (or bus's) output into `bus`, or back to the master with
`None`. Bus chains that would loop are refused.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `bus: Option<TrackId>` — `Some(bus)` a bus track to feed; `None` the master

**Returns** `Result<()>` — nothing

**Errors** an unknown track; a target that is not a bus; a routing loop; the master (it has no output).

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** push `SetTrackOutput`; rebuilds the mixer.

**Example**

```rust
track::route(&mut session, vox, Some(verb))?;   // into the reverb bus
track::route(&mut session, vox, None)?;         // back to the master
```

### `peak`

```rust
pub fn peak(session: &Session, track: TrackId) -> Option<[f32; 2]>
```

Reads and resets a track's or bus's peak meter: the loudest sample per
channel since the last call, as linear amplitude. `None` before the first
rebuild.

**Takes**
- `session: &Session` — the open session, borrowed
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`

**Returns** `Option<[f32; 2]>` — `Some([left, right])` as linear amplitude since the last call, or `None` before the first rebuild or for an archived track

**Errors** never; this is a query.

**Undo** none: a meter.

**Made of** `StripParams::take_peak` for channels 0 and 1 on the live strip.

**Example**

```rust
if let Some([l, r]) = track::peak(&session, vox) { draw_meter(l, r); }
```

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
through `session::structural`. `archive` / `restore`: `ArchiveTrack`
through `session::structural`. `volume` / `pan`: `set_track_volume_db` /
`set_track_pan` through `session::edit`, then `StripParams::set_volume_db`
/ `set_pan` on the live strip. `mute` / `solo` / `arm` / `monitor`:
`TrackState::{mute, solo, arm, monitor, input}` through `session::set_state`,
then `StripParams::set_mute` / `set_solo` for the first two and a rebuild
for `monitor` (a `MonoSynthNode` or a `LiveInputNode` on the strip). `route`: `SetTrackOutput` through `session::structural`.
`peak`: `StripParams::take_peak` for channels 0 and 1.

## Notes

* The master is a track too: `session.memory.master()` gives it, and
  `volume`, `pan`, `mute`, `rename` and `peak` work on it like on any
  track. It is not in `memory.tracks()`, is always there, and refuses
  archive, solo, arm, monitor, input, routing and sends; routing a track
  to it is the default, spelled `None`.
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
