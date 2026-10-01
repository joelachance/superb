# project

Starting a song, putting things in it, saving it, undoing.

## Purpose

`project` owns the document side of a `Rig`: it creates and opens rigs,
and every edit to the song's *structure* goes through it. Each edit is an
undoable command on `rig.history`; structural ones also mark the rig for a
sync so the console follows.

## Verbs

| verb | does | undo step |
|---|---|---|
| `create(name, sample_rate, block_size)` | a new `Rig` around an empty song | — |
| `open(path, block_size)` / `save(rig, path)` / `is_saved(rig)` | JSON file in, out, and the clean flag | — |
| `add_track(rig, name, kind)` / `remove_track` / `rename_track` | tracks of kind Audio, Midi or Bus | one each (rename coalesces) |
| `add_audio_clip(rig, track, take, start, length)` | a clip of cached audio (`take` is the cache key) | one |
| `add_midi_clip(rig, track, name, start, length, notes)` | a clip of inline MIDI notes | one |
| `move_clip` / `remove_clip` | | move coalesces until `finish_gesture` |
| `add_marker(rig, name, at)` | | one |
| `set_tempo(rig, bpm)` | initial tempo of the tempo map | one |
| `set_loop(rig, Option<LoopRange>)` | the song's loop range; also told to the engine | one |
| `undo(rig)` / `redo(rig)` | return `false` when there is nothing to do; mark the rig for a sync | |
| `finish_gesture(rig)` | ends the current coalescing run ("mouse up") | |
| `import_audio(rig, track, path, at)` (feature `files`) | decode, resample to the song's rate, cache, place a clip of the full length | one |

The crate-private helpers `edit`, `structural` and `switch` are how the
other flows change the session; see [flows.md](../flows.md).

## Usage

```rust
use superb::prelude::*;
use superb::project;

let mut rig = project::create("song", SampleRate::HZ_48K, 256)?;
let vox = project::add_track(&mut rig, "vox", TrackKind::Audio)?;

let mut take = PlanarBuffer::new(2, 48_000);
take.fill(0.2);
rig.cache.insert("take-1.wav", take);
let clip = project::add_audio_clip(
    &mut rig, vox, "take-1.wav", TimePosition::ZERO, TimeDuration::from_samples(48_000),
)?;
project::move_clip(&mut rig, clip, TimePosition::from_samples(24_000))?;
project::finish_gesture(&mut rig);
project::undo(&mut rig)?;   // the clip is back at zero
project::save(&mut rig, "song.superb.json")?;
```

Audio is not embedded in the file: clips refer to cache keys, and a program
that opens a song re-caches the audio (`AudioCache::load` with the `files`
feature, or `rig.cache.insert`).

## Made of

`Session::allocate_*_id` → `Track::with_id` / `Clip::with_id` /
`Marker::with_id` → the matching command in `primitives::session::commands`
pushed through `UndoStack::push`; `SessionSnapshot` and `serde_json` for
files; `AudioCache::load` for import. `set_loop` additionally sends
`EngineCommand::SetLoop` so the transport knows the range at once.

## Limitations

* Clips are placed whole; there is no trimming, splitting or fades yet
  (the session primitives have `offset` on audio clips; the flow does not
  expose it).
* Tempo is a single initial value here; use `rig.session.tempo_map_mut()`
  for tempo and meter changes, then `finish_gesture` is not involved (they
  bypass undo).
