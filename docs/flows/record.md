# record

Start a recording, stop it, get a clip on every armed track.

## Purpose

`start` records from a `Source` onto every armed track from the playhead;
`stop` writes each track's audio as a WAV file into the session folder
(`audio/vox 1.wav`), keeps it in memory, and places one clip per armed
track as a single undo step. Each clip is named after its file (`vox 1`,
`vox 2`, ...) and holds the channels the track's input selects. The audio arrives
through a capture queue that holds only a fraction of a second, so
something empties it as it fills: a hardware source gets a thread; a queue
you fill yourself waits for `collect`, which `render_blocks` calls after
every block.

## Verbs

| verb | does |
|---|---|
| `start(session, Source)` | begin at the playhead on every armed track; the transport rolls |
| `collect(session) -> frames` | move waiting blocks into a queue recording (`session::steps::render_blocks` does this); nothing to do for a device recording |
| `stop(session) -> Vec<ClipId>` | stop the transport, finish the recording, write the files, place clips ("Record") |

`Source` says where the audio comes from:

| source | does |
|---|---|
| `Source::Device(Option<name>)` (feature `devices`) | a hardware input matched to the session with as many channels as the armed tracks' inputs need, gathered by a thread |
| `Source::Queue { blocks, channels }` | a capture queue you fill yourself (`capture_queue` gives both ends) |

`Recording` is the recording in progress, held in `session.recording`:
`frames()`, `dropped()` (blocks lost to a full queue), `punch_in()`,
`tracks()`, `channels()`.

Arming and the input a track records from are track state: `track::arm`,
`track::input`.

## Usage

Offline, from a queue you fill yourself:

```rust
use superb::primitives::io::capture::capture_queue;
use superb::{record, session, track, transport};

track::arm(&mut session, vox, true)?;
transport::start(&mut session)?;
let (mut mic, blocks) = capture_queue(1, 256, 64)?;
record::start(&mut session, record::Source::Queue { blocks, channels: 1 })?;
for block in my_audio_blocks() {
    mic.push(&block, timestamp_us);
    session::steps::render_blocks(&mut session, 1)?;   // renders and collects
}
let clips = record::stop(&mut session)?;                 // one clip per armed track
```

From a real input (feature `devices`):

```rust
record::start(&mut session, record::Source::Device(None))?;   // the default input
// ... play ...
let clips = record::stop(&mut session)?;
```

## Made of

`start`: the armed tracks → for a device, an `InputConfig` at the
session's rate and block size with the channels the inputs need,
`device::run_capture` with 64 queued blocks, and a collector thread →
`session::steps::rebuild` if needed → `EngineHandle::send(Record)` →
`transport::playhead` for the punch-in. `stop`: `EngineHandle::send(Stop)`
→ drop the stream → drain the queue or finish the thread → one buffer from
the planes → per track a file name nothing uses yet and the channels its
`Input` selects (the whole capture, shared, when none is set) →
`wav::write` into the session folder → `AudioCache::insert_shared` →
`clip::add` (the clip takes the audio's length), inside one undo group →
rebuild.

## Notes

* The queue holds only a fraction of a second (a device source uses 64
  blocks), which is why a device recording gets a thread. A queue
  recording relies on `render_blocks` collecting after every block; with a
  device driving the engine, call `collect` from a timer.
  `Recording::dropped` tells you if blocks were lost either way.
* Every armed track gets a clip at the punch-in position holding the
  channels its input selects, and its own file (two tracks without an
  input get two copies of the capture on disk).
* A session that has never been saved has no folder: the audio stays in
  memory until the first `session::save`, which writes it. Undo removes a
  clip, never its file; a reopened session takes the next free name.
* Input monitoring, punch-out, loop recording and pre-roll are not there
  yet.
