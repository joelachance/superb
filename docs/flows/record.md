# record

Start a recording, stop it, get a clip on every armed track.

## Purpose

A recording flows through a capture queue: the input side fills it with
blocks, a collector thread (device recording) or `collect` (your own
source) moves them into memory, and `stop` stores the audio and places one
clip per armed track as a single undo step. Each clip is named after its
track (`vox 1`, `vox 2`, ...), and the clips of one recording share one
buffer.

## Verbs

| verb | does |
|---|---|
| `start(session, Option<device name>, channels)` (feature `devices`) | open a hardware input matched to the session, spawn a collector thread, begin at the playhead |
| `start_from(session, CaptureConsumer, channels)` | begin from a queue you fill yourself; blocks wait for `collect` |
| `collect(session) -> frames` | move waiting blocks into a manual recording (`session::steps::render_blocks` does this); nothing to do for a threaded one |
| `stop(session) -> Vec<ClipId>` | stop the transport, finish the recording, place clips ("Record") |
| `is_recording(session)` | |

Arming is track state: `track::arm` and `track::armed`.

## Steps (`record::steps`)

| step | does |
|---|---|
| `open_input(&InputConfig, slots)` (feature `devices`) | a capture queue fed by hardware; returns the stream and the consumer |
| `spawn_collector(CaptureConsumer, channels) -> Collector` | a thread that empties the queue as it fills; `finish` returns the planes |
| `collect(&mut Capture) -> frames appended` | drain a manual capture's queue into its planes |

`Recording` is the recording in progress: its `Capture` (`Manual { blocks,
planes }` or `Threaded(Collector)`), punch-in position, armed tracks,
channel count, and (with `devices`) the input stream. `frames()` and
`dropped()` report progress and lost blocks.

## Usage

Offline, from a producer you fill yourself:

```rust
use superb::primitives::io::capture::capture_queue;
use superb::{record, session, track, transport};

track::arm(&mut session, vox, true)?;
transport::start(&mut session)?;
let (mut mic, blocks) = capture_queue(1, 256, 64)?;
record::start_from(&mut session, blocks, 1)?;
for block in my_audio_blocks() {
    mic.push(&block, timestamp_us);
    session::steps::render_blocks(&mut session, 1)?;   // renders and collects
}
let clips = record::stop(&mut session)?;                 // one clip per armed track
```

From a real input (feature `devices`):

```rust
record::start(&mut session, None, 1)?;   // the default input, one channel; a thread gathers the audio
// ... play ...
let clips = record::stop(&mut session)?;
```

## Made of

`start_from` (and `start` after opening the input and spawning the
collector): `track::armed` → `session::steps::rebuild` if needed →
`EngineHandle::send(Record)` → `transport::playhead` for the punch-in.
`stop`: `EngineHandle::send(Stop)` → drop the stream → `collect` or
`Collector::finish` → one buffer from the planes → per track a name no
cache entry uses → `AudioCache::insert_shared` → `session::add_audio_clip`,
inside one undo group → rebuild.

## Notes

* The queue holds only a fraction of a second (`start` uses 64 blocks),
  which is why a device recording gets a collector thread. A manual
  recording relies on `render_blocks` collecting after every block.
  `Recording::dropped` tells you if blocks were lost either way.
* Every armed track gets the same audio at the punch-in position. Per-track
  inputs (`track::input`) are the next step; input monitoring, punch-out,
  loop recording and pre-roll are not there yet.
