# session

Starting a session, putting things in it, saving it, undoing.

## Purpose

`session` owns the document side of a `Session`: it creates and opens rigs,
and every edit to the session's *structure* goes through it. Each edit is an
undoable command on `session.history`; structural ones also mark the session for a
rebuild so the console follows.

## Verbs

| verb | does | undo step |
|---|---|---|
| `create(name, sample_rate, block_size)` | a new `Session` around an empty session | — |
| `open(path, block_size)` / `save(session, path)` / `is_saved(session)` | JSON file in, out, and the clean flag | — |
| `add_track(session, name, kind)` / `remove_track` / `rename_track` | tracks of kind Audio, Midi or Bus | one each (quick renames merge) |
| `add_audio_clip(session, track, take, start, length)` | a clip of cached audio (`take` is the cache key) | one |
| `add_midi_clip(session, track, name, start, length, notes)` | a clip of inline MIDI notes | one |
| `move_clip` / `remove_clip` | | moves within half a second are one step |
| `add_marker(session, name, at)` | | one |
| `set_tempo(session, bpm)` | initial tempo of the tempo map | one |
| `set_loop(session, Option<LoopRange>)` | the session's loop range; also told to the engine | one |
| `undo(session)` / `redo(session)` | return `false` when there is nothing to do; mark the session for a rebuild | |
| `import_audio(session, track, path, at)` (feature `files`) | decode, resample to the session's rate, cache, place a clip of the full length | one |

The crate-private helpers `edit`, `structural` and `switch` are how the
other flows change the session; see [flows.md](../flows.md).

## Usage

```rust
use superb::prelude::*;
use superb::session;

let mut session = session::create("session", SampleRate::HZ_48K, 256)?;
let vox = session::add_track(&mut session, "vox", TrackKind::Audio)?;

let mut take = PlanarBuffer::new(2, 48_000);
take.fill(0.2);
session.cache.insert("take-1.wav", take);
let clip = session::add_audio_clip(
    &mut session, vox, "take-1.wav", TimePosition::ZERO, TimeDuration::from_samples(48_000),
)?;
session::move_clip(&mut session, clip, TimePosition::from_samples(24_000))?;
session::undo(&mut session)?;   // the clip is back at zero
session::save(&mut session, "session.superb.json")?;
```

Audio is not embedded in the file: clips refer to cache keys, and a program
that opens a session re-caches the audio (`AudioCache::load` with the `files`
feature, or `session.cache.insert`).

## Made of

`Document::allocate_*_id` → `Track::with_id` / `Clip::with_id` /
`Marker::with_id` → the matching command in `primitives::session::commands`
pushed through `UndoStack::push`; `DocumentSnapshot` and `serde_json` for
files; `AudioCache::load` for import. `set_loop` additionally sends
`EngineCommand::SetLoop` so the transport knows the range at once.

## Limitations

* Clips are placed whole; there is no trimming, splitting or fades yet
  (the session primitives have `offset` on audio clips; the flow does not
  expose it).
* Tempo is a single initial value here; `session.session.tempo_map_mut()`
  gives tempo and meter changes, which bypass undo.
