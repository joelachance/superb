# clip

Clips: put audio or MIDI on a track, move it, archive it.

## Purpose

A clip is a region placed on a track. It plays audio the session holds in
memory, named by the key it was stored under, or MIDI notes. `add` works
out which from what you hand it and how long the clip is: the length of
the audio, or the end of the last note. Nothing here deletes: `archive`
takes a clip off the timeline and `restore` brings it back. Every change
alters what the mixer plays, so it rebuilds the mixer.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, content, at) -> ClipId` | `content` is a key into the session's audio (`&str`, `String`, path) or `Vec<MidiNote>`; an audio clip is as long as its audio and named after the key's stem, a MIDI clip ends with its last note and is named after the track; fails when the audio is not in memory, there are no notes, or the track is a bus | one |
| `move_to(session, clip, at)` | | moves within half a second are one step |
| `archive(session, clip)` | kept with its place and audio, silent, not counted toward the session's end | one |
| `restore(session, clip)` | back on the timeline | one |

## Usage

```rust
use superb::prelude::*;
use superb::{clip, session, track};

let mut session = session::create("demo", SampleRate::HZ_48K, 256)?;
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;

let mut audio = PlanarBuffer::new(2, 48_000);
audio.fill(0.2);
session.audio.insert("vox 1", audio);
let take = clip::add(&mut session, vox, "vox 1", TimePosition::ZERO)?;   // one second long
clip::move_to(&mut session, take, TimePosition::from_samples(24_000))?;
clip::archive(&mut session, take)?;      // silent, still in the session
session::undo(&mut session)?;            // playing again
```

## Made of

`add`: `AudioCache::get` (or `TempoMap::beat_to_samples` for notes) for
the length → `Memory::allocate_clip_id` → `Clip::with_id` → `AddClip`
through `session::structural`. `move_to`: `MoveClip` through
`session::structural`. `archive` / `restore`: `ArchiveClip` through
`session::structural`.

## Notes

* `ClipSource` is the primitive behind `content`: `&str`, `String`, a path
  and `Vec<MidiNote>` convert into it. Pass a `ClipSource::Audio` with an
  `offset` yourself to start partway into the audio.
* Audio is not embedded in the saved file: a clip names a key, and a
  program that opens a session puts the audio back in memory under that
  key (`AudioCache::load` with the `files` feature, or `insert`).
* Not yet in the flow: trimming, splitting, fades and moving a clip to
  another track (the primitives have `ResizeClip`, fades and
  `MoveClip::to_track`).
