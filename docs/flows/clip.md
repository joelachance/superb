# clip

Clips: put audio or MIDI on a track, move, trim and split it, shape it,
archive it.

## Purpose

A clip is a region placed on a track. It plays audio the session holds in
memory, named by the key it was stored under, or MIDI notes. `add` works
out which from what you hand it and how long the clip is: the length of
the audio, or the end of the last note. `move_to`, `trim` and `split`
place its edges; `gain`, `fade_in` and `fade_out` shape it; `mute` silences it as
state. Nothing here deletes: `archive` takes a clip off the timeline and
`restore` brings it back. Every change alters what the mixer plays, so it
rebuilds the mixer.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, content, at) -> ClipId` | `content` is a key into the session's audio (`&str`, `String`, path) or `Vec<MidiNote>`; an audio clip is as long as its audio and named after the key's stem, a MIDI clip ends with its last note and is named after the track; fails when the audio is not in memory, there are no notes, or the track is a bus | one |
| `move_to(session, clip, track, at)` | to a track (its own to move in time only), starting at `at` | moves within half a second are one step |
| `split(session, clip, at) -> ClipId` | cut in two; the left keeps the id, the right is returned; what is underneath stays put, fades stay at the outer edges | one |
| `trim(session, clip, start, end)` | move the edges; the audio underneath and MIDI notes stay where they are on the timeline | edge moves within half a second are one step |
| `gain(session, clip, db)` | the clip's own gain, before the fader; heard at the next rebuild | drag (coalesces) |
| `mute(session, clip, on)` | state, like a track's mute; the clip still counts toward the session's end | — |
| `fade_in(session, clip, length)` / `fade_out(session, clip, length)` | fades from the start and to the end | drag (coalesces) |
| `archive(session, clip)` | kept with its place and audio, silent, not counted toward the session's end | one |
| `restore(session, clip)` | back on the timeline | one |
| `rename(session, clip, name)` | | quick renames merge |
| `add_note(session, clip, MidiNote)` | a note into a MIDI clip, beats from the clip start; a note past the end extends the clip | one |
| `remove_note(session, clip, MidiNote)` | the first note equal to it | one |
| `change_note(session, clip, from, to)` | replace a note with another: moved, longer, louder, whatever differs | drag (note changes in one clip coalesce) |

## Reference

Every function of the `clip` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `add`

```rust
pub fn add( session: &mut Session, track: TrackId, content: impl Into<ClipSource>, at: TimePosition, ) -> Result<ClipId>
```

Places a clip on `track` starting at `at`. `content` is the name audio was
stored under in `Session::audio` (`&str`, `String` or a path) or the notes
of a MIDI clip (`Vec<MidiNote>`, beats relative to `at`). An audio clip is
as long as its audio and named after the key's file stem; a MIDI clip ends
with its last note and is named after the track.  Fails when the audio is
not in memory, when there are no notes, or when the track is a bus.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `track: TrackId` — the track's id, from `track::add` or `session.memory.tracks()`
- `content: impl Into<ClipSource>` — audio: the key the audio is held under in `session.audio` (`&str`, `String`, `PathBuf`), from `session::import_audio` or a recording; MIDI: a `Vec<MidiNote>` with beats from the clip start
- `at: TimePosition` — a position on the timeline, in samples (`TimePosition::from_seconds` converts)

**Returns** `Result<ClipId>` — the new clip's id

**Errors** audio not in `session.audio` under that key; no notes; a bus.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `AudioCache::get` or `TempoMap::beat_to_samples` for the length → `Memory::allocate_clip_id` → `Clip::with_id` → `AddClip` through `session::structural`.

**Example**

```rust
let take = session::import_audio(&mut session, "/music/take.wav")?;
let c = clip::add(&mut session, vox, take.key, TimePosition::from_seconds(4.0, rate))?;
let m = clip::add(&mut session, keys, vec![MidiNote { beat: 0.0, length: 1.0, note: 60, velocity: 100, channel: 0 }], TimePosition::ZERO)?;
```

### `move_to`

```rust
pub fn move_to( session: &mut Session, clip: ClipId, track: TrackId, at: TimePosition, ) -> Result<()>
```

Moves a clip to `track`, starting at `at`. Pass the clip's own track to move
it in time only. Moves of the same clip within `Session::COALESCE_WINDOW` of
each other are one undo step, so a drag undoes in one go. Buses take no
clips.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `track: TrackId` — the track to put it on; its own to move in time only
- `at: TimePosition` — a position on the timeline, in samples (`TimePosition::from_seconds` converts)

**Returns** `Result<()>` — nothing

**Errors** an unknown clip or track; a bus.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `MoveClip::to_track` through `session::structural`.

**Example**

```rust
clip::move_to(&mut session, c, vox, TimePosition::from_seconds(8.0, rate))?;   // same track, later
clip::move_to(&mut session, c, gtr, TimePosition::from_seconds(8.0, rate))?;   // another track
```

### `split`

```rust
pub fn split(session: &mut Session, clip: ClipId, at: TimePosition) -> Result<ClipId>
```

Cuts a clip in two at `at`, which must fall inside it. The left part keeps
the clip's id; the right part is returned. What is underneath stays put,
fades stay at the outer edges, gain, mute and archived carry over, and both
halves keep the name. One undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `at: TimePosition` — a position on the timeline, in samples (`TimePosition::from_seconds` converts)

**Returns** `Result<ClipId>` — the id of the right half; the left keeps the clip's id

**Errors** an unknown clip; a position outside the clip.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `Memory::allocate_clip_id` → `SplitClip` through `session::structural`.

**Example**

```rust
let right = clip::split(&mut session, c, transport::playhead(&session))?;
```

### `trim`

```rust
pub fn trim( session: &mut Session, clip: ClipId, start: TimePosition, end: TimePosition, ) -> Result<()>
```

Moves the clip's edges to `start` and `end` on the timeline. What is
underneath stays where it was: trimming the head shows later audio and MIDI
notes keep their place. Edge moves of the same clip within
`Session::COALESCE_WINDOW` are one undo step. Fails when `end` is not after
`start`.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `start: TimePosition` — the new start on the timeline; the audio underneath stays put
- `end: TimePosition` — the new end on the timeline

**Returns** `Result<()>` — nothing

**Errors** an unknown clip; an end not after the start.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `TrimClip` through `session::structural`.

**Example**

```rust
clip::trim(&mut session, c, start.offset(4_800), end)?;   // 0.1 s off the front at 48 kHz
```

### `gain`

```rust
pub fn gain(session: &mut Session, clip: ClipId, db: f32) -> Result<()>
```

Sets the clip's own gain in dB, before the track's fader. Moves within
`Session::COALESCE_WINDOW` are one undo step; heard at the next rebuild (at
once while a device plays).

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `db: f32` — a level in decibels; `0.0` is unity, negative is quieter

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `set_clip_gain_db` through `session::structural`.

**Example**

```rust
clip::gain(&mut session, c, -3.0)?;
```

### `mute`

```rust
pub fn mute(session: &mut Session, clip: ClipId, on: bool) -> Result<()>
```

Mutes or unmutes a clip. State, like a track's mute: saved with the session,
not an undo step; the clip stays on the timeline and counts toward the
session's end. Rebuilds the mixer.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `on: bool` — `true` to switch on, `false` to switch off

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** none: state, not an edit (saved through the state file, see `session::state_file`); rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `ClipState::mute` through `session::set_state` → rebuild.

**Example**

```rust
clip::mute(&mut session, c, true)?;
```

### `fade_in`

```rust
pub fn fade_in(session: &mut Session, clip: ClipId, length: TimeDuration) -> Result<()>
```

Sets the fade-in length from the clip's start. Coalesces like a drag.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `length: TimeDuration` — how long the fade lasts from the clip start

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `set_clip_fade_in` through `session::structural`.

**Example**

```rust
clip::fade_in(&mut session, c, TimeDuration::from_seconds(0.05, rate))?;
```

### `fade_out`

```rust
pub fn fade_out(session: &mut Session, clip: ClipId, length: TimeDuration) -> Result<()>
```

Sets the fade-out length to the clip's end. Coalesces like a drag.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `length: TimeDuration` — how long the fade lasts up to the clip end

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `set_clip_fade_out` through `session::structural`.

**Example**

```rust
clip::fade_out(&mut session, c, TimeDuration::from_seconds(0.5, rate))?;
```

### `archive`

```rust
pub fn archive(session: &mut Session, clip: ClipId) -> Result<()>
```

Takes a clip off the timeline without deleting it: it stops playing and no
longer counts toward the session's end, but keeps its place and its audio.
`restore` brings it back.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `ArchiveClip` through `session::structural`.

**Example**

```rust
clip::archive(&mut session, c)?;
```

### `restore`

```rust
pub fn restore(session: &mut Session, clip: ClipId) -> Result<()>
```

Puts an archived clip back on the timeline.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `ArchiveClip` (restoring) through `session::structural`.

**Example**

```rust
clip::restore(&mut session, c)?;
```

### `rename`

```rust
pub fn rename(session: &mut Session, clip: ClipId, name: &str) -> Result<()>
```

Renames a clip. Quick successive renames of one clip are one undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `name: &str` — the new name

**Returns** `Result<()>` — nothing

**Errors** an unknown clip.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** `set_clip_name` through `session::edit`.

**Example**

```rust
clip::rename(&mut session, c, "verse vocal")?;
```

### `add_note`

```rust
pub fn add_note(session: &mut Session, clip: ClipId, note: MidiNote) -> Result<()>
```

Adds a note to a MIDI clip. `note.beat` and `note.length` are beats from the
clip start; a note that ends past the clip end extends the clip. Notes are
their content: two identical notes are one note twice, and `remove_note` or
`change_note` takes the first. One undo step; how notes are picked, drawn or
quantised is the caller's business.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `note: MidiNote` — the note, with `beat` and `length` in quarter notes from the clip start

**Returns** `Result<()>` — nothing

**Errors** an audio clip; a note before the clip start, with no length, or out of MIDI range (note 0..=127, velocity 1..=127, channel 0..=15).

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `AddNote` through `session::structural`.

**Example**

```rust
clip::add_note(&mut session, m, MidiNote { beat: 2.0, length: 0.5, note: 64, velocity: 90, channel: 0 })?;
```

### `remove_note`

```rust
pub fn remove_note(session: &mut Session, clip: ClipId, note: MidiNote) -> Result<()>
```

Removes a note from a MIDI clip; undo puts it back.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `note: MidiNote` — the note to remove; the first one equal to it goes

**Returns** `Result<()>` — nothing

**Errors** an audio clip; no note equal to it.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `RemoveNote` through `session::structural`.

**Example**

```rust
clip::remove_note(&mut session, m, note)?;
```

### `change_note`

```rust
pub fn change_note( session: &mut Session, clip: ClipId, from: MidiNote, to: MidiNote, ) -> Result<()>
```

Replaces a note of a MIDI clip with another: moved, lengthened, louder, on
another channel, whatever differs. Changes to the same note within
`Session::COALESCE_WINDOW` are one undo step, so a drag that calls this as
it goes undoes in one. A note that ends past the clip end extends the clip.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `clip: ClipId` — the clip's id, from `clip::add` or `track.clips()`
- `from: MidiNote` — the note as it is now
- `to: MidiNote` — the note as it should be: moved, longer, louder, on another channel, whatever differs

**Returns** `Result<()>` — nothing

**Errors** an audio clip; no note equal to `from`; a `to` out of range.

**Undo** one step per drag: note changes in one clip within the coalesce window merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `ChangeNote` through `session::structural`.

**Example**

```rust
clip::change_note(&mut session, m, note, MidiNote { beat: note.beat + 0.25, ..note })?;   // nudge it later
```

## Usage

```rust
use superb::prelude::*;
use superb::{clip, session, track};

let mut session = session::create("demo.superb", SampleRate::HZ_48K, 256)?;
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;

let mut audio = PlanarBuffer::new(2, 48_000);
audio.fill(0.2);
session.audio.insert("vox 1", audio);
let take = clip::add(&mut session, vox, "vox 1", TimePosition::ZERO)?;   // one second long
clip::move_to(&mut session, take, vox, TimePosition::from_samples(24_000))?;
let tail = clip::split(&mut session, take, TimePosition::from_samples(48_000))?;
clip::trim(&mut session, take, TimePosition::from_samples(30_000), TimePosition::from_samples(60_000))?;
clip::fade_in(&mut session, take, TimeDuration::from_samples(2_400))?;
clip::archive(&mut session, take)?;      // silent, still in the session
session::undo(&mut session)?;            // playing again
```

## Made of

`add`: `AudioCache::get` (or `TempoMap::beat_to_samples` for notes) for
the length → `Memory::allocate_clip_id` → `Clip::with_id` → `AddClip`
through `session::structural`. `move_to`: `MoveClip::to_track`; `trim`:
`TrimClip`; `split`: `Memory::allocate_clip_id` → `SplitClip`;
`gain`, `fade_in`, `fade_out`: `set_clip_gain_db` / `set_clip_fade_in` /
`set_clip_fade_out`; `archive` / `restore`: `ArchiveClip`; all through
`session::structural`. `mute`: `ClipState::mute` through
`session::set_state`, then a rebuild.

## Notes

* Audio has to be in the session before a clip can play it:
  `session::import_audio` brings a file in and returns the key `add`
  takes; a recording's audio arrives the same way as it stops.
* `ClipSource` is the primitive behind `content`: `&str`, `String`, a path
  and `Vec<MidiNote>` convert into it. Pass a `ClipSource::Audio` with an
  `offset` yourself to start partway into the audio.
* A key is a path relative to the session folder (`audio/vox 1.wav`).
  `session::save` writes any audio a clip refers to that is not on disk
  yet, and `session::open` reads the WAV files back; audio you insert
  under a key yourself travels with the session the same way.
* A clip's settable fields live in `Clip.state: ClipState { gain_db, mute,
  fade_in, fade_out, archived }`.
* Both halves of a split keep the clip's name and share its audio.
* Notes are their content: a `MidiNote` is identified by its beat, length,
  pitch, velocity and channel, so there is no note id to keep. Two
  identical notes are one note twice, and `remove_note` takes the first.
  How notes are picked, drawn, quantised or auditioned is the caller's
  business; the verbs only change the data, with undo.
