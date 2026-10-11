# record

Start a recording, stop it, get a clip on every armed track.

## Purpose

`start` records from a `Source` onto every armed track, from the playhead
or from the punch-in point ahead of it, until `stop` or the punch-out
point; the transport rolls from where it is and drops into and out of
record at those points. Every armed audio track gets its own WAV file in
the session folder the moment the recording starts (`audio/vox 1.wav`),
and the audio streams into it block by block, flushed to disk about once a
second: a crash costs a second of audio, never the take, and a three-hour
recording never has to fit in memory. `stop` finishes the files, keeps the
audio in memory, and places one clip per armed track as a single undo
step. Each clip is named after its file (`vox 1`, `vox 2`, ...) and holds
the channels the track's input selects. An armed
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
| `stop(session) -> Vec<ClipId>` | stop the transport, finish the files, place clips ("Record") |
| `unfinished(session) -> Vec<PathBuf>` | the files a crash left unfinished: still tagged `.recording`, not yet clips |
| `recover(session) -> Vec<ClipId>` | finish those files' headers and place their clips where the tags say ("Recover"); deletes nothing |

`Source` says where the audio comes from:

| source | does |
|---|---|
| `Source::Device(Option<name>)` (feature `devices`) | the session's audio input (`devices::input`), opened on that device if it is not open yet; every channel it carries, gathered by a thread |
| `Source::Queue { blocks, channels }` | a capture queue you fill yourself (`capture_queue` gives both ends) |

`Recording` is the recording in progress, held in `session.recording`.
What an application shows while it records: `frames()` captured,
`sealed_frames()` on disk for good, `dropped()` blocks (a full queue),
`disk_errors()` (a full disk, a file gone), `peaks()` of the latest block
per channel, `since_last_block()` (a source that went quiet), and
`files()`, the files being written. Where it is: `started_at()`,
`punch_in()`, `punch_out()`, `tracks()`, `channels()`.

Arming and the input a track records from are track state: `track::arm`,
`track::input`.

## Reference

Every function of the `record` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `AuraError`; the errors listed are the ones you can cause.

### `start`

```rust
pub fn start(session: &mut Session, source: Source) -> Result<()>
```

Starts recording from `source` onto every armed track. Recording begins at
the playhead, or at the punch-in point when one is set ahead of it
(`punch_in`): the transport rolls from where it is and drops into record
there. With a punch-out point (`punch_out`) it drops out of record there and
keeps playing; `stop` finishes either way. A recording that starts at the
playhead gets the metronome's count-in (`metronome::count_in`): the
transport rolls that many bars early with the click sounding. Every armed
audio track gets its file in the session folder now (`audio/vox 1.wav`, with
a `.recording` tag beside it) and the audio streams into it from the first
block.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `source: Source` — `Source::Device(Some(name))` or `Source::Device(None)` for the default input (feature `devices`); `Source::Queue { blocks, channels }` for audio you push yourself

**Returns** `Result<()>` — nothing; `session.recording` holds the recording until `stop`

**Errors** a recording already running; no armed track; the device input cannot be opened; the command queue is full.

**Undo** none: not an edit until `stop`.

**Made of** `track::arm`ed tracks → for a device, `devices::input` if needed and its recording queue → a `wav::Writer` and a tag per armed audio track → a collector thread for a device → `session::steps::rebuild` when needed → `EngineHandle::send(SetPunch)` → `EngineHandle::send(Record)`, or `Play` when the punch-in is ahead → `transport::playhead` for where it all started.

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

Writes waiting audio blocks to the recording's files when the source is a
queue (`Source::Queue`), and takes the MIDI that arrived since the last call
either way; a device recording's audio has its own thread. Returns the
frames captured in total, or zero when nothing is recording.
`session::steps::render_blocks` calls this after every block; with a device
driving the engine, call it from a timer during long takes, so the MIDI
queue (`MIDI_CAPTURE_CAPACITY` messages) never fills.

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

Stops the recording and the transport, finishes the files and places one
clip per armed track. An audio clip refers to its track's file (`audio/vox
1.wav`, so it is named `vox 1`) from the punch-in point: what was captured
before it (the roll towards it, a count-in) stays in the file ahead of the
clip's offset, and what came after the punch-out stays past its end, so the
clip can be trimmed out over it later like any other. The file holds the
channels the track's `Input` selects (tracks without one get the whole
capture). A device take lands where it was played: the file's first frame is
placed by the device clock, and a capture that began after the transport
rolled gives a clip that starts that much later. Returns the new clips (none
if nothing was captured). The whole placement is one undo step called
"Record": undoing it removes the clips and deletes the files it wrote, redo
writes them again from the audio kept in memory.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<Vec<ClipId>>` — the ids of the clips placed, one per armed track that got something (audio inside the punch points, or notes that arrived)

**Errors** nothing recording; a file cannot be finished or read back; the collector thread failed.

**Undo** one step labelled "Record": undo removes the clips and deletes the files they wrote, redo writes them again; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `EngineHandle::send(Stop)` → drain the queue or `Collector::finish` (and hand the audio input its queue back) → `wav::Writer::finish` per file, tags removed → the file's first frame lined up with the `RollStart` when the device clock gives one → per track, the clip's start, offset into the file and length from the punch points → `wav::read` into `AudioCache::insert_shared` → `clip::add`, inside one undo group; rebuilds the mixer so the clips play back on their strips.

**Example**

```rust
let clips = record::stop(&mut session)?;
```

### `unfinished`

```rust
pub fn unfinished(session: &Session) -> Vec<PathBuf>
```

Recordings a crash left unfinished in the session folder: files that still
have their `.recording` tag beside them, as keys (`audio/vox 1.wav`).
`recover` places them; until then they are not clips and `session::open`
only notes them in the log.

**Takes**
- `session: &Session` — the open session, borrowed

**Returns** `Vec<PathBuf>` — the files a crash left unfinished, as keys into the session folder (`audio/vox 1.wav`); empty when there are none

**Errors** never; a query.

**Undo** none.

**Example**

```rust
for key in record::unfinished(&session) { println!("unfinished: {}", key.display()); }
```

### `recover`

```rust
pub fn recover(session: &mut Session) -> Result<Vec<ClipId>>
```

Places the recordings a crash left unfinished (`unfinished`): each file's
header is finished in place (`wav::repair`), its audio loaded, and a clip
placed on the track it was recording for, from the punch-in point the tag
remembers, lined up as if the capture began where the transport rolled (the
device clock that would say otherwise is gone with the crash). The tags are
removed; the files stay, and the placement is one undo step called "Recover"
whose undo removes the clips but deletes nothing. A tag whose track is gone,
or whose file cannot be read, is left where it is and noted in the log.
Refused while a recording runs.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<Vec<ClipId>>` — the ids of the clips placed, one per unfinished file whose track still exists

**Errors** a recording running; a clip cannot be placed.

**Undo** one step labelled "Recover": undo removes the clips and deletes nothing; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** the tags in `audio/` → `wav::repair` → `wav::read` into `AudioCache::insert_shared` → `AddClip` through `session::structural` per file, inside one undo group; rebuilds the mixer.

**Example**

```rust
let placed = record::recover(&mut session)?;   // after session::open noted unfinished recordings
```

## Types

### `Source`

Where a recording's audio comes from.

**Variants**
- `Device(Option<String>)` — The session's audio input (`devices::input`). If it is not open yet, this opens it on the named device (`None` for the default). Every channel the input carries is captured, and a thread gathers the audio until `stop`; each track keeps the channel(s) its `track::input` names.
- `Queue { blocks: CaptureConsumer, channels: usize }` — A capture queue you fill yourself (a file, a generator, a test) with blocks of `channels` channels. Blocks wait on the queue until `collect` runs; `session::steps::render_blocks` does that after every block, so an offline program never calls it.

### `Recording`

A recording in progress: what `Session::recording` holds between `start` and
`stop`. Its audio is on disk from the first block: one WAV file per armed
audio track in the session folder (`audio/vox 1.wav`), written as the blocks
arrive and flushed about once a second, so a crash costs a second of audio,
not the take, and the take never has to fit in memory. The counters here are
what an application shows while it records: frames captured and sealed on
disk, blocks dropped, disk errors, the level of the latest block, and how
long ago it came.

**Methods**
- `fn frames(&self) -> usize` — Frames captured so far: handed to the files, not all on disk yet.
- `fn sealed_frames(&self) -> u64` — Frames on disk for good in every file of the recording: written, synced and covered by a header a reader accepts. Everything before this frame survives a crash and will never change, so an uploader can ship the byte range it covers. Advances about once a second.
- `fn disk_errors(&self) -> u64` — Writes that failed across the files (disk full, file gone, I/O error). Anything above zero means a gap in a file; the recording keeps draining its queue so the audio thread is never held up, and the application decides whether to stop.
- `fn dropped(&self) -> u64` — Blocks the input side could not queue because the queue was full. Anything above zero means a gap in the recording.
- `fn peaks(&self) -> Vec<f32>` — Peak per captured channel of the latest block, 0.0 to 1.0 and above when clipping; all zero before the first block.
- `fn since_last_block(&self) -> Option<core::time::Duration>` — How long ago the last block arrived; `None` before the first. A value that keeps growing while the transport rolls means the source went quiet: the device was unplugged, or nothing feeds the queue.
- `fn files(&self) -> &[PathBuf]` — The files being written, one per armed audio track, as keys into the session folder (`audio/vox 1.wav`). They exist from `start` on, grow as the recording runs, and are the clips' audio after `stop`.
- `fn started_at(&self) -> TimePosition` — Where the transport started rolling: at or before the punch-in.
- `fn punch_in(&self) -> TimePosition` — Where the recording begins and the clips will start.
- `fn punch_out(&self) -> Option<TimePosition>` — Where the recording ends on its own, if a punch-out point was set.
- `fn tracks(&self) -> &[TrackId]` — The tracks that were armed when it started; each gets a clip.
- `fn channels(&self) -> usize` — Audio channels being captured; zero when no audio track is armed.
- `fn midi_messages(&self) -> usize` — Incoming MIDI messages gathered so far.

## Usage

Offline, from a queue you fill yourself:

```rust
use aura::primitives::io::capture::capture_queue;
use aura::{record, session, track, transport};

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
`transport::playhead` for where it started; before any of that, per armed
audio track, a file name nothing uses yet, a `wav::Writer` on it with the
channels its `Input` selects (the whole capture when none is set) and a
`.recording` tag beside it. `punch_in` / `punch_out`: `SetPunch` through
`session::edit`. `collect`: the queue's blocks into the writers, a flush
every second's worth of frames. `stop`: `EngineHandle::send(SetPunch`
cleared`)` → `EngineHandle::send(Stop)` → drain the queue or finish the
thread → `wav::Writer::finish` per file, tags removed → the file's first
frame lined up with the `RollStart` when the device clock gives one → per
track the clip's start, its offset into the file and its length from the
punch points → `wav::read` into `AudioCache::insert_shared` → `clip::add`,
inside one undo group → rebuild. `unfinished`: the tags in `audio/`.
`recover`: per tag `wav::repair` → `wav::read` → `AddClip` at the tag's
punch-in, inside one undo group → rebuild.

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
* The file holds everything captured, the roll towards the punch-in and
  the count-in included; the clip starts at its offset into the file and
  ends at the punch-out, so it can be trimmed out over that audio later
  like any other clip. A capture that began after the transport rolled
  (the device clock says so) gives a clip that starts that much later,
  not one padded with silence.
* The files are 32-bit float WAV, finished at `stop`; past 4 GiB they are
  RF64, which every DAW reads. While the recording runs their headers say
  how much was flushed, and `wav::read` and `wav::repair` take the file's
  length as the truth anyway.
* `sealed_frames()` is for an uploader: the frames before it are on disk
  and will never change, so their byte range can be read and shipped while
  the recording goes on (`files()` names the files; the header is the
  first 80 bytes, then `channels × 4` bytes per frame). The SDK never
  knows a network exists.
* After a crash the files are there with their `.recording` tags.
  `session::open` notes them in the log, `unfinished` lists them, and
  `recover` finishes their headers and places their clips where the tags
  say, lined up as if the capture began where the transport rolled. It
  deletes nothing: undoing a recovery removes the clips and leaves the
  files.
* Undoing a recording removes its clips and deletes the files it wrote;
  redo writes them again from the audio in memory. Nothing else deletes a
  file.
* Writing happens on the thread that drains the queue (the collector for
  a device, the caller of `collect` otherwise), never on the audio thread,
  which only copies blocks into the queue. A failed write is counted in
  `disk_errors()` and the queue keeps draining, so the audio thread is
  never held up; stopping is the application's decision.
* The transport reports `Recording` from the punch-in to the punch-out,
  at block granularity; the clip is cut at the exact samples.
* A recording that starts at the playhead gets the metronome's count-in:
  the transport rolls that many bars early with the click, which stops at
  the punch-in; what was captured during the count-in is in the file
  ahead of the clip, not in the clip. The count-in doubles as a check:
  read `peaks()`, `frames()` and `disk_errors()` during it to know the mic
  is live and the disk takes writes before a wanted note is at stake.
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
