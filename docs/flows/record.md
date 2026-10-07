# record

Start a recording, stop it, get a clip on every armed track.

## Purpose

`start` records from a `Source` onto every armed track, from the playhead
or from the punch-in point ahead of it, until `stop` or the punch-out
point; the transport rolls from where it is and drops into and out of
record at those points. `stop` writes each track's audio as a WAV file into the session folder
(`audio/vox 1.wav`), keeps it in memory, and places one clip per armed
track as a single undo step. Each clip is named after its file (`vox 1`,
`vox 2`, ...) and holds the channels the track's input selects. An armed
MIDI track gets a clip of the notes that reached the engine while the
recording ran, from any connected port or `midi::play_note`, stamped
where they arrived and named after the track. The audio arrives
through a capture queue that holds only a fraction of a second, so
something empties it as it fills: a hardware source gets a thread; a queue
you fill yourself waits for `collect`, which `render_blocks` calls after
every block.

A device take lands where it was played. What you hear leaves late and
what you play arrives late, by the buffers in and out; the recording lines
its first frame up with where the transport began rolling by comparing the
input's capture instants with the output's playback instants on the device
clock (`RollStart`), so those buffers are taken back by themselves and the
capture's own start never shifts the take. Nothing to set by hand.

## Verbs

| verb | does |
|---|---|
| `start(session, Source)` | begin on every armed track, audio and MIDI, at the playhead, or at the punch-in point when it lies ahead; the transport rolls from where it is, or from `metronome::count_in` bars before the playhead with the click sounding |
| `punch_in(session, Option<TimePosition>)` / `punch_out(session, Option<TimePosition>)` | where the next recording drops in and out; saved; edits that coalesce |
| `collect(session) -> frames` | move waiting blocks into a queue recording (`session::steps::render_blocks` does this); nothing to do for a device recording |
| `stop(session) -> Vec<ClipId>` | stop the transport, finish the recording, write the files, place clips ("Record") |

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

## Reference

Every function of the `record` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `start`

```rust
pub fn start(session: &mut Session, source: Source) -> Result<()>
```

Starts recording from `source` onto every armed track. Recording begins at
the playhead, or at the punch-in point when one is set ahead of it
([`punch_in`]): the transport rolls from where it is and drops into record
there. With a punch-out point ([`punch_out`]) it drops out of record there
and keeps playing; [`stop`] finishes either way. A recording that starts at
the playhead gets the metronome's count-in (`metronome::count_in`): the
transport rolls that many bars early with the click sounding.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `source: Source` — `Source::Device(Some(name))` or `Source::Device(None)` for the default input (feature `devices`); `Source::Queue { blocks, channels }` for audio you push yourself

**Returns** `Result<()>` — nothing; `session.recording` holds the recording until `stop`

**Errors** a recording already running; no armed track; the device input cannot be opened; the command queue is full.

**Undo** none: not an edit until `stop`.

**Made of** [`track::arm`](crate::track::arm)ed tracks → for a device, `devices::input` if needed, its recording queue and a collector thread → [`session::steps::rebuild`] when needed → `EngineHandle::send(SetPunch)` → `EngineHandle::send(Record)`, or `Play` when the punch-in is ahead → [`transport::playhead`] for where it all started.

**Example**

```rust
record::start(&mut session, record::Source::Device(None))?;   // the default input
// ... play ...
let clips = record::stop(&mut session)?;
```

### `punch_in`

```rust
pub fn punch_in(session: &mut Session, at: Option<TimePosition>) -> Result<()>
```

Sets or clears the punch-in point: where the next recording begins when it
lies ahead of the playhead. Saved with the session; an edit that coalesces
like a drag.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `at: Option<TimePosition>` — `Some(position)` where the next recording drops in; `None` clears it

**Returns** `Result<()>` — nothing

**Errors** never in practice.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** `SetPunch` through `session::edit`.

**Example**

```rust
record::punch_in(&mut session, Some(TimePosition::from_seconds(16.0, rate)))?;
```

### `punch_out`

```rust
pub fn punch_out(session: &mut Session, at: Option<TimePosition>) -> Result<()>
```

Sets or clears the punch-out point: where a recording drops out of record on
its own and keeps playing. Saved with the session; an edit that coalesces
like a drag.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `at: Option<TimePosition>` — `Some(position)` where the recording drops out; `None` clears it

**Returns** `Result<()>` — nothing

**Errors** never in practice.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** `SetPunch` through `session::edit`.

**Example**

```rust
record::punch_out(&mut session, Some(TimePosition::from_seconds(24.0, rate)))?;
```

### `collect`

```rust
pub fn collect(session: &mut Session) -> usize
```

Moves waiting audio blocks into the recording when the source is a queue
([`Source::Queue`]), and the MIDI that arrived since the last call either
way; a device recording's audio has its own thread. Returns the frames
gathered in total, or zero when nothing is recording.
[`session::steps::render_blocks`] calls this after every block; with a
device driving the engine, call it from a timer during long takes, so the
MIDI queue (`MIDI_CAPTURE_CAPACITY` messages) never fills.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `usize` — the audio frames gathered so far, or zero when nothing is recording

**Errors** never.

**Undo** none.

**Example**

```rust
let frames = record::collect(&mut session);   // call from a timer while a device drives the engine
```

### `stop`

```rust
pub fn stop(session: &mut Session) -> Result<Vec<ClipId>>
```

Stops the recording and the transport and places one clip per armed track
starting at the punch-in point. Audio captured before the punch-in (while
rolling towards it) and after the punch-out is left out. Each clip holds the
channels its track's [`Input`] selects (tracks without one share the whole
capture) and refers to `audio/<track> <n>.wav` (`audio/vox 1.wav`), so it is
named `vox 1`; the file is written into [`Session::folder`] before the clip
is placed. Returns the new clips (none if nothing was captured). The whole
placement is one undo step called "Record": undoing it removes the clips and
deletes the files it wrote, redo writes them again; the audio stays in
memory either way.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<Vec<ClipId>>` — the ids of the clips placed, one per armed track that got something (audio captured, or notes that arrived)

**Errors** nothing recording; a WAV cannot be written; the collector thread failed.

**Undo** one step labelled "Record": undo removes the clips and deletes the files they wrote; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `EngineHandle::send(Stop)` → drain the queue or `Collector::finish` (and hand the audio input its queue back) → frame 0 lined up with the `RollStart` when the device clock gives one → trimmed to the punch points → the planes become one buffer → per track, a file name nothing uses yet and the channels its `Input` selects → `wav::write` into the session folder → `AudioCache::insert_shared` → [`clip::add`](crate::clip::add), inside one undo group; rebuilds the mixer so the clips play back on their strips.

**Example**

```rust
let clips = record::stop(&mut session)?;
```

## Types

### `Source`

Where a recording's audio comes from.

| variant | type | meaning |
|---|---|---|
| `Device` | `(Option<String>)` | The session's audio input (`devices::input`). If it is not open yet, this opens it on the named device (`None` for the default). Every channel the input carries is captured, and a thread gathers the audio until [`stop`]; each track keeps the channel(s) its [`track::input`](crate::track::input) names. |
| `Queue` | `{ blocks: CaptureConsumer, channels: usize }` | A capture queue you fill yourself (a file, a generator, a test) with blocks of `channels` channels. Blocks wait on the queue until [`collect`] runs; [`session::steps::render_blocks`] does that after every block, so an offline program never calls it. |

### `Recording`

A recording in progress: what [`Session::recording`] holds between [`start`]
and [`stop`].

| method | meaning |
|---|---|
| `fn frames(&self) -> usize` | Frames gathered so far. |
| `fn dropped(&self) -> u64` | Blocks the input side could not queue because the queue was full. Anything above zero means a gap in the recording. |
| `fn started_at(&self) -> TimePosition` | Where the transport started rolling: at or before the punch-in. |
| `fn punch_in(&self) -> TimePosition` | Where the recording begins and the clips will start. |
| `fn punch_out(&self) -> Option<TimePosition>` | Where the recording ends on its own, if a punch-out point was set. |
| `fn tracks(&self) -> &[TrackId]` | The tracks that were armed when it started; each gets a clip. |
| `fn channels(&self) -> usize` | Audio channels being captured; zero when no audio track is armed. |
| `fn midi_messages(&self) -> usize` | Incoming MIDI messages gathered so far. |

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
`RollStart` when the device clock gives one → drop what came before the punch-in and after the
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
  direct monitoring; the alignment fixes where the take lands.
* Lining frame 0 up with the roll start needs a device clock on both
  sides: blocks stamped by the input stream and the audio thread driven
  by `devices::output`. Otherwise frame 0 is where the recording started.
  What the clock cannot know (converter and USB latency the driver does
  not report) is not taken back; on a good interface that is a few
  samples.
* Every armed MIDI track gets the notes of the MIDI channel it listens to
  (`track::midi_channel`), or all of them by default, from every port and
  from `midi::play_note`. A note still down when the recording stops ends
  there. Telling ports apart comes later.
* Incoming MIDI waits on a queue of `MIDI_CAPTURE_CAPACITY` messages until
  `collect` or `stop` drains it; `render_blocks` collects after every
  block, and with a device driving the engine `collect` from a timer keeps
  a long take safe.
* Loop recording is not there yet.
