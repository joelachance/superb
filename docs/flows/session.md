# session

The session: the one thing you open, edit and play.

## Purpose

`Session` is the struct every flow takes. It holds the `Memory` (tracks,
clips, routing, tempo), the undo history, the audio clips play and the
running engine. This module creates, opens and saves it, edits the
memory with undo, and in `session::steps` holds the engine side: how the
memory becomes the mixer the engine runs.

## Verbs

| verb | does | undo step |
|---|---|---|
| `create(name, sample_rate, block_size) -> Session` | a new session around an empty memory | — |
| `open(path, block_size)` / `save(session, path)` | JSON file in and out; `session.history.is_clean()` says whether anything changed since (state changes count) | — |
| `add_marker(session, name, at)` | | one |
| `set_tempo(session, bpm)` | initial tempo of the tempo map | one |
| `set_loop(session, Option<LoopRange>)` | the loop range; also told to the engine | one |
| `undo(session)` / `redo(session)` | return `false` when there is nothing to do; rebuild the mixer | |

Tracks are added and set up in [`track`](track.md); clips are placed in
[`clip`](clip.md).

## Steps (`session::steps`)

| step | does |
|---|---|
| `rebuild(session)` | build a fresh mixer from the memory and swap it into the engine; the structural flows call it for you |
| `render_blocks(session, blocks) -> PlanarBuffer` | tick the audio thread for `blocks` blocks when no sound card is attached, and return the audio |
| `build_mixer(&memory, &audio, channels) -> (Mixer, MixerHandles)` | strips carrying each track's state, routing, sends, a node per clip, a live instrument per monitoring MIDI track; and the live controls, keyed by memory id |
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

let mut session = session::create("demo", SampleRate::HZ_48K, 256)?;
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
session::set_tempo(&mut session, 100.0)?;
session::add_marker(&mut session, "verse", TimePosition::from_samples(48_000))?;
session::undo(&mut session)?;   // the marker is gone
session::save(&mut session, "demo.superb.json")?;
let again = session::open("demo.superb.json", 256)?;
```

Audio is not embedded in the file: clips name keys into the session's
audio, and a program that opens a session puts the audio back under those
keys (`AudioCache::load` with the `files` feature, or `session.audio.insert`).

## Made of

`Memory::allocate_marker_id` → `Marker::with_id` → the matching command
in `primitives::session::commands` pushed through `UndoStack::push`; `MemorySnapshot` and `serde_json` for files.
`set_loop` additionally sends
`EngineCommand::SetLoop`. `rebuild`: `EngineHandle::reclaim` →
`build_mixer` → `publish`, then
`LiveGraph::apply_pending` while the audio thread is still in the session.
`render_blocks`: `AudioThread::tick` per block, plus
`record::collect` while a recording runs.

The crate-private helpers `edit`, `structural`, `changed_structure` and
`set_state` are how the other flows change the memory; see
[flows.md](../flows.md).

## Limitations

* Tempo is a single initial value here; `session.memory.tempo_map_mut()`
  gives tempo and meter changes, which bypass undo.
