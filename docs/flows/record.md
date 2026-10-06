# record

Start a recording, stop it, get a clip on every armed track.

## Purpose

`start` records from a `Source` onto every armed track, from the playhead
or from the punch-in point ahead of it, until `stop` or the punch-out
point; the transport rolls from where it is and drops into and out of
record at those points. `stop` writes each track's audio as a WAV file into the session folder
(`audio/vox 1.wav`), keeps it in memory, and places one clip per armed
track as a single undo step. Each clip is named after its file (`vox 1`,
`vox 2`, ...) and holds the channels the track's input selects. The audio arrives
through a capture queue that holds only a fraction of a second, so
something empties it as it fills: a hardware source gets a thread; a queue
you fill yourself waits for `collect`, which `render_blocks` calls after
every block.

A take comes back late by the trip through the interface: what you hear
leaves late, what you play arrives late. `offset` says how much earlier to
place every take, and `calibrate` measures it once with a loopback cable
(feature `devices`). A device recording also lines its first frame up with
where the transport began rolling, on the device clock (`RollStart`), so
the capture's own start never shifts the take.

## Verbs

| verb | does |
|---|---|
| `start(session, Source)` | begin on every armed track at the playhead, or at the punch-in point when it lies ahead; the transport rolls from where it is, or from `metronome::count_in` bars before the playhead with the click sounding |
| `punch_in(session, Option<TimePosition>)` / `punch_out(session, Option<TimePosition>)` | where the next recording drops in and out; saved; edits that coalesce |
| `collect(session) -> frames` | move waiting blocks into a queue recording (`session::steps::render_blocks` does this); nothing to do for a device recording |
| `stop(session) -> Vec<ClipId>` | stop the transport, finish the recording, write the files, place clips ("Record") |
| `offset(session, TimeDuration)` | how much earlier than it arrived a take is placed; positive is earlier; state, saved |
| `calibrate(session) -> TimeDuration` (feature `devices`) | with an output looped back into the input, `devices::output` playing and `devices::input` open: play one click, record it, store how late it came back as the offset; about two seconds |

`Source` says where the audio comes from:

| source | does |
|---|---|
| `Source::Device(Option<name>)` (feature `devices`) | the session's audio input (`devices::input`), opened on that device if it is not open yet; every channel it carries, gathered by a thread |
| `Source::Queue { blocks, channels }` | a capture queue you fill yourself (`capture_queue` gives both ends) |

`Recording` is the recording in progress, held in `session.recording`:
`frames()`, `dropped()` (blocks lost to a full queue), `started_at()`,
`punch_in()`, `punch_out()`, `tracks()`, `channels()`.

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

`start`: the armed tracks → for a device, `devices::input` if needed, its
recording queue (64 blocks) and a collector thread →
`session::steps::rebuild` if needed → `EngineHandle::send(SetPunch)` →
`EngineHandle::send(Record)`, or `Play` when the punch-in lies ahead →
`transport::playhead` for where it started. `punch_in` / `punch_out`:
`SetPunch` through `session::edit`. `stop`: `EngineHandle::send(SetPunch` cleared`)` → `EngineHandle::send(Stop)`
→ drain the queue or finish the thread → frame 0 lined up with the
`RollStart` when the device clock gives one → the take moved earlier by
the recording offset → drop what came before the punch-in and after the
punch-out → one buffer from the planes → per track a file name nothing uses yet and the channels its
`Input` selects (the whole capture, shared, when none is set) →
`wav::write` into the session folder → `AudioCache::insert_shared` →
`clip::add` (the clip takes the audio's length), inside one undo group →
rebuild.

## Notes

* To hear yourself, `track::monitor` the armed track: it plays the same
  input through its strip.
* The queue holds only a fraction of a second (a device source uses 64
  blocks), which is why a device recording gets a thread. A queue
  recording relies on `render_blocks` collecting after every block; with a
  device driving the engine, call `collect` from a timer.
  `Recording::dropped` tells you if blocks were lost either way.
* Every armed track gets a clip at the punch-in position holding the
  channels its input selects, and its own file (two tracks without an
  input get two copies of the capture on disk).
* Undoing a recording removes its clips and deletes the files it wrote;
  redo writes them again. Nothing else deletes a file.
* The transport reports `Recording` from the punch-in to the punch-out,
  at block granularity; the clip is cut at the exact samples.
* A recording that starts at the playhead gets the metronome's count-in:
  the transport rolls that many bars early with the click, which stops at
  the punch-in; what was captured during the count-in is left out.
* Hearing yourself late while playing is a different problem from the
  take landing late. Nothing in software can move sound earlier, so the
  cures for monitoring latency are small buffers or the interface's own
  direct monitoring; the offset fixes where the take lands.
* The offset is one number per session. Measure it again after changing
  the interface or its buffer size.
* Lining frame 0 up with the roll start needs a device clock on both
  sides: blocks stamped by the input stream and the audio thread driven
  by `devices::output`. Otherwise frame 0 is where the recording started,
  as before. The offset applies either way, to `Source::Queue` too.
* Loop recording is not there yet.
