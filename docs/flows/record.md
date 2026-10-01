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
| `arm(rig, track, on)` | switch an audio track's arm (saved, not undoable); MIDI tracks are refused |
| `armed_tracks(rig)` / `is_recording(rig)` | |
| `start(rig, &InputConfig)` (feature `devices`) | open a hardware input, spawn a collector thread, begin at the playhead |
| `start_from(rig, CaptureConsumer, channels)` | begin from a queue you fill yourself; blocks wait for `collect` |
| `collect(rig) -> frames` | move waiting blocks into a manual take (`play::advance` does this); nothing to do for a threaded one |
| `stop(rig) -> Vec<ClipId>` | stop the transport, collect, store the take, place clips ("Record take") |

## Steps (`record::steps`)

| step | does |
|---|---|
| `open_input(&InputConfig, slots)` (feature `devices`) | a capture queue fed by hardware; returns the stream and the consumer |
| `spawn_collector(CaptureConsumer, channels) -> Collector` | a thread that empties the queue as it fills; `finish` returns the planes |
| `collect(&mut Capture) -> frames appended` | drain a manual capture's queue into its planes |
| `append_blocks(&mut CaptureConsumer, &mut [Vec<f32>]) -> frames` | the drain both of the above use |
| `finish_take(planes) -> PlanarBuffer` | |
| `take_name(rig)` | the next unused `take N` cache key |
| `store_take(&mut cache, name, audio)` | |
| `place_clip(rig, track, take, start, length)` | `project::add_audio_clip` |

`Recording` is the take in progress: its `Capture` (`Manual { blocks,
planes }` or `Threaded(Collector)`), punch-in position, armed tracks,
channel count, and (with `devices`) the input stream. `frames()` and
`dropped()` report progress and lost blocks.

## Usage

Offline, from a producer you fill yourself:

```rust
use superb::primitives::io::capture::capture_queue;
use superb::{play, project, record};

record::arm(&mut rig, vox, true)?;
play::start(&mut rig)?;
let (mut mic, blocks) = capture_queue(1, 256, 64)?;
record::start_from(&mut rig, blocks, 1)?;
for block in my_audio_blocks() {
    mic.push(&block, timestamp_us);
    play::advance(&mut rig, 1)?;          // renders and collects
}
let clips = record::stop(&mut rig)?;      // one clip per armed track
```

From a real input (feature `devices`):

```rust
let mic = superb::devices::input_config(&rig, None, 1);
record::start(&mut rig, &mic)?;        // a collector thread gathers the take
// ... play ...
let clips = record::stop(&mut rig)?;
```

## Made of

`start_from` (and `start` after opening the input and spawning the
collector): `armed_tracks` → `play::sync` if needed →
`LiveEngine::send(Record)` → `play::position` for the punch-in. `stop`:
`LiveEngine::send(Stop)` → drop the stream → `collect` or
`Collector::finish` → `finish_take` →
`take_name` → `AudioCache::insert` → `UndoStack::begin_group` →
`add_audio_clip` per track → `end_group`, `needs_sync`.

## Notes

* The queue holds only a fraction of a second (`start` uses 64 blocks),
  which is why a device capture gets a collector thread. A manual capture
  relies on `play::advance` collecting after every block.
  `Recording::dropped` tells you if blocks were lost either way.
* The take is placed at the punch-in position on every track that was
  armed when the take began, with the same audio. Per-track inputs are not
  routed yet.
* There is no input monitoring through a strip while recording, and no
  punch-out, loop recording or pre-roll yet.
