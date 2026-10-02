# record

Arm tracks, capture a take, turn it into clips.

## Purpose

A take flows through a capture queue: the input side fills it with blocks,
a collector thread (device capture) or `collect` (your own source) moves
them into the growing take on the control thread, and
`stop` puts the take in the audio cache and places one clip per armed
track as a single undo step. The steps are public so a program can record
from any source.

## Verbs

| verb | does |
|---|---|
| `arm(session, track, on)` | switch an audio track's arm (saved, not undoable); MIDI tracks are refused |
| `armed_tracks(session)` / `is_recording(session)` | |
| `start(session, &InputConfig)` (feature `devices`) | open a hardware input, spawn a collector thread, begin at the playhead |
| `start_from(session, CaptureConsumer, channels)` | begin from a queue you fill yourself; blocks wait for `collect` |
| `collect(session) -> frames` | move waiting blocks into a manual take (`session::steps::render_blocks` does this); nothing to do for a threaded one |
| `stop(session) -> Vec<ClipId>` | stop the transport, collect, store the take, place clips ("Record take") |

## Steps (`record::steps`)

| step | does |
|---|---|
| `open_input(&InputConfig, slots)` (feature `devices`) | a capture queue fed by hardware; returns the stream and the consumer |
| `spawn_collector(CaptureConsumer, channels) -> Collector` | a thread that empties the queue as it fills; `finish` returns the planes |
| `collect(&mut Capture) -> frames appended` | drain a manual capture's queue into its planes |
| `append_blocks(&mut CaptureConsumer, &mut [Vec<f32>]) -> frames` | the drain both of the above use |
| `finish_take(planes) -> PlanarBuffer` | |
| `take_name(session)` | the next unused `take N` cache key |
| `store_take(&mut cache, name, audio)` | |
| `place_clip(session, track, take, start, length)` | `session::add_audio_clip` |

`Recording` is the take in progress: its `Capture` (`Manual { blocks,
planes }` or `Threaded(Collector)`), punch-in position, armed tracks,
channel count, and (with `devices`) the input stream. `frames()` and
`dropped()` report progress and lost blocks.

## Usage

Offline, from a producer you fill yourself:

```rust
use superb::primitives::io::capture::capture_queue;
use superb::{record, session, transport};

track::arm(&mut session, vox, true)?;
transport::start(&mut session)?;
let (mut mic, blocks) = capture_queue(1, 256, 64)?;
record::start_from(&mut session, blocks, 1)?;
for block in my_audio_blocks() {
    mic.push(&block, timestamp_us);
    session::steps::render_blocks(&mut session, 1)?;          // renders and collects
}
let clips = record::stop(&mut session)?;      // one clip per armed track
```

From a real input (feature `devices`):

```rust
let mic = superb::devices::input_config(&session, None, 1);
record::start(&mut session, &mic)?;        // a collector thread gathers the take
// ... play ...
let clips = record::stop(&mut session)?;
```

## Made of

`start_from` (and `start` after opening the input and spawning the
collector): `armed_tracks` → `session::steps::rebuild` if needed →
`EngineHandle::send(Record)` → `transport::playhead` for the punch-in. `stop`:
`EngineHandle::send(Stop)` → drop the stream → `collect` or
`Collector::finish` → `finish_take` →
`take_name` → `AudioCache::insert` → `UndoStack::begin_group` →
`add_audio_clip` per track → `end_group`, `needs_rebuild`.

## Notes

* The queue holds only a fraction of a second (`start` uses 64 blocks),
  which is why a device capture gets a collector thread. A manual capture
  relies on `session::steps::render_blocks` collecting after every block.
  `Recording::dropped` tells you if blocks were lost either way.
* The take is placed at the punch-in position on every track that was
  armed when the take began, with the same audio. Per-track inputs are not
  routed yet.
* There is no input monitoring through a strip while recording, and no
  punch-out, loop recording or pre-roll yet.
