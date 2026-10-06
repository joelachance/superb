# session

The session: the one thing you open, edit and play.

## Purpose

`Session` is the struct every flow takes. It holds the `Memory` (tracks,
clips, routing, tempo), the undo history, the audio clips play and the
running engine. This module creates, opens and saves it, edits the
memory with undo, and in `session::steps` holds the engine side: how the
memory becomes the mixer the engine runs. A session is a folder from the
moment it is created: `memory.json` beside an `audio/` folder of WAV files.

## Verbs

| verb | does | undo step |
|---|---|---|
| `create(folder, sample_rate, block_size) -> Session` | a new session in `folder`, created now with its `audio/` folder and `memory.json`, named after the folder | — |
| `open(folder, block_size)` / `save(session)` | the folder in and out: `memory.json` plus the WAV files clips refer to; `save` also writes any audio a clip refers to that is not on disk yet; `session.history.is_clean()` says whether anything changed since (state changes count) | — |
| `add_marker(session, name, at)` | | one |
| `set_tempo(session, bpm)` | initial tempo of the tempo map; also told to the engine | one |
| `set_loop(session, Option<LoopRange>)` | the loop range; also told to the engine; `transport::looping` turns it on | one |
| `undo(session)` / `redo(session)` | return `false` when there is nothing to do; rebuild the mixer | |

Tracks are added and set up in [`track`](track.md); clips are placed in
[`clip`](clip.md).

## Steps (`session::steps`)

| step | does |
|---|---|
| `rebuild(session)` | build a fresh mixer from the memory and swap it into the engine; the structural flows call it for you |
| `render_blocks(session, blocks) -> PlanarBuffer` | tick the audio thread for `blocks` blocks when no sound card is attached, and return the audio |
| `build_mixer(&memory, &audio, channels) -> (Mixer, MixerHandles)` | strips carrying each track's state, routing, sends, a node per clip, a live instrument per monitoring MIDI track, an input tap per monitoring audio track; and the live controls, keyed by the memory's ids |
| `publish(&mut EngineHandle, Mixer)` | prepare the graph and hand it to the engine |

`MixerHandles` is what `build_mixer` returns beside the mixer and what
`rebuild` stores in `session.mixer`: `strip(track)` and `send(send)` give
the live `StripParams` / `SendParams` of a track or send, so a
fader can move without a rebuild. The memory's and the mixer's ids are
separate namespaces; the handles hide the mixer's. The `session_steps`
example builds a mixer this way without a `Session`.

## Usage

```rust
use superb::prelude::*;
use superb::{session, track};

let mut session = session::create("demo.superb", SampleRate::HZ_48K, 256)?;   // a folder
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
session::set_tempo(&mut session, 100.0)?;
session::add_marker(&mut session, "verse", TimePosition::from_samples(48_000))?;
session::undo(&mut session)?;   // the marker is gone
session::save(&mut session)?;
let again = session::open("demo.superb", 256)?;
```

## The session folder

```text
demo.superb/
  memory.json        the Memory: tracks, clips, markers, routing, tempo
  audio/
    vox 1.wav        recordings, 32-bit float at the session's rate
    vox 2.wav
```

`create` makes the folder and writes `memory.json` at once, so a session
can be opened from the moment it exists. A clip refers to its audio by a
path relative to the folder, which is also its key in `session.audio`.
`record::stop` writes `audio/<track> <n>.wav` as it stops, before the clip
is placed; `save` writes `memory.json` and any audio a program put in
`session.audio` under a key that is not on disk yet. `open` reads every
referenced WAV back; a missing file leaves its clip silent rather than
failing. The only thing that deletes a file is undoing the recording that
wrote it; redo writes it again. Archived clips keep their files in place;
`memory.json` marks them `"archived": true`.

## Made of

`create`: `create_dir_all` → `Memory::new` → `Session::new` →
`Memory::save`. `Memory::allocate_marker_id` → `Marker::with_id` → the matching command
in `primitives::session::commands` pushed through `UndoStack::push`;
`MemorySnapshot` and `serde_json` for `memory.json`, `wav::write` /
`wav::read` for the audio. `set_tempo` and `set_loop` additionally send
`EngineCommand::SetTempo` / `SetLoop`. `rebuild`: `EngineHandle::reclaim` →
`build_mixer` → `publish`, then `LiveGraph::apply_pending` while the audio
thread is still in the session. `render_blocks`: `AudioThread::tick` per
block, plus `record::collect` while a recording runs.

The crate-private helpers `edit`, `structural`, `changed_structure` and
`set_state` are how the other flows change the memory; see
[flows.md](../flows.md).

## Limitations

* Tempo is a single initial value here; `session.memory.tempo_map_mut()`
  gives tempo and meter changes, which bypass undo.
