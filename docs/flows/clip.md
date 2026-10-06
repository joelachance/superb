# clip

Clips: put audio or MIDI on a track, move and trim it, shape it, archive it.

## Purpose

A clip is a region placed on a track. It plays audio the session holds in
memory, named by the key it was stored under, or MIDI notes. `add` works
out which from what you hand it and how long the clip is: the length of
the audio, or the end of the last note. `move_to` and `trim` place its
edges; `gain`, `fade_in` and `fade_out` shape it; `mute` silences it as
state. Nothing here deletes: `archive` takes a clip off the timeline and
`restore` brings it back. Every change alters what the mixer plays, so it
rebuilds the mixer.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, content, at) -> ClipId` | `content` is a key into the session's audio (`&str`, `String`, path) or `Vec<MidiNote>`; an audio clip is as long as its audio and named after the key's stem, a MIDI clip ends with its last note and is named after the track; fails when the audio is not in memory, there are no notes, or the track is a bus | one |
| `move_to(session, clip, at)` | | moves within half a second are one step |
| `trim(session, clip, start, end)` | move the edges; the audio underneath and MIDI notes stay where they are on the timeline | edge moves within half a second are one step |
| `gain(session, clip, db)` | the clip's own gain, before the fader; heard at the next rebuild | drag (coalesces) |
| `mute(session, clip, on)` | state, like a track's mute; the clip still counts toward the session's end | — |
| `fade_in(session, clip, length)` / `fade_out(session, clip, length)` | fades from the start and to the end | drag (coalesces) |
| `archive(session, clip)` | kept with its place and audio, silent, not counted toward the session's end | one |
| `restore(session, clip)` | back on the timeline | one |

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
clip::move_to(&mut session, take, TimePosition::from_samples(24_000))?;
clip::trim(&mut session, take, TimePosition::from_samples(30_000), TimePosition::from_samples(60_000))?;
clip::fade_in(&mut session, take, TimeDuration::from_samples(2_400))?;
clip::archive(&mut session, take)?;      // silent, still in the session
session::undo(&mut session)?;            // playing again
```

## Made of

`add`: `AudioCache::get` (or `TempoMap::beat_to_samples` for notes) for
the length → `Memory::allocate_clip_id` → `Clip::with_id` → `AddClip`
through `session::structural`. `move_to`: `MoveClip`; `trim`: `TrimClip`;
`gain`, `fade_in`, `fade_out`: `set_clip_gain_db` / `set_clip_fade_in` /
`set_clip_fade_out`; `archive` / `restore`: `ArchiveClip`; all through
`session::structural`. `mute`: `ClipState::mute` through
`session::set_state`, then a rebuild.

## Notes

* `ClipSource` is the primitive behind `content`: `&str`, `String`, a path
  and `Vec<MidiNote>` convert into it. Pass a `ClipSource::Audio` with an
  `offset` yourself to start partway into the audio.
* A key is a path relative to the session folder (`audio/vox 1.wav`).
  `session::save` writes any audio a clip refers to that is not on disk
  yet, and `session::open` reads the WAV files back; audio you insert
  under a key yourself travels with the session the same way.
* A clip's settable fields live in `Clip.state: ClipState { gain_db, mute,
  fade_in, fade_out, archived }`.
* Not yet in the flow: splitting a clip and moving it to another track
  (the primitive `MoveClip::to_track` exists).
