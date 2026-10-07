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
| `open(folder, block_size)` / `save(session)` | the folder in and out: `memory.json` plus the WAV files clips refer to; `save` also writes any audio a clip refers to that is not on disk yet and remembers where the playhead is, so `open` starts there; `session.history.is_clean()` says whether anything changed since (state changes count) | — |
| `import_audio(session, file) -> Imported` | a file into the session: with `files`, WAV, AIFF, FLAC, ALAC, MP3, AAC (MP4/M4A), Ogg Vorbis, CAF and Matroska, resampled to the session's rate when they differ (a WAV at the session's rate without `files`); written as 32-bit float `audio/<name>.wav` (`<name> 2.wav` if taken) and kept in memory; `Imported` says the key, the file's rate and bit depth, channels, frames and whether it was resampled; `clip::add` places it | — (the placing is the edit) |
| `state_file(session, Option<PathBuf>)` | where state is kept: read now if the file exists, rewritten after every state change and every `save`, playhead included; `None` stops writing. Until set, state is not saved | — |
| `set_tempo(session, bpm)` | initial tempo of the tempo map; rebuilds, which hands the engine the map | one |
| `tempo_change(session, beat, Option<bpm>)` | add or remove a tempo change at a beat; rebuilds | one per beat (coalesces) |
| `meter_change(session, bar, Option<TimeSignature>)` | set or remove the time signature from a bar on; bar 1 always has one; rebuilds | one |
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

## Reference

Every function of the `session` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `create`

```rust
pub fn create( folder: impl AsRef<Path>, sample_rate: SampleRate, block_size: usize, ) -> Result<Session>
```

Starts a new, empty session at 120 bpm in 4/4 in `folder`, which is created
(with its `audio/` folder) and given its `memory.json` at once, so it can be
opened from here on. The session is named after the folder.

**Takes**
- `folder: impl AsRef<Path>` — where the session lives; created now, with an `audio/` folder inside; the session is named after it
- `sample_rate: SampleRate` — the one sample rate everything in the session runs at
- `block_size: usize` — frames per engine block, the largest block any node sees

**Returns** `Result<Session>` — the new `Session`, running an offline engine; nothing plays until `devices::output` or `session::steps::render_blocks`

**Errors** the folder cannot be created or `memory.json` cannot be written; a block size of zero.

**Undo** none: not an edit.

**Made of** `create_dir_all` → `Memory::new` → `Session::new` → `Memory::save`.

**Example**

```rust
let mut session = session::create("~/songs/demo", SampleRate::HZ_48K, 256)?;
```

### `open`

```rust
pub fn open(folder: impl AsRef<Path>, block_size: usize) -> Result<Session>
```

Opens a session folder written by [`save`]: reads `memory.json` and loads
every WAV file the clips refer to into [`Session::audio`]. A file that is
missing or at another sample rate is left out, and its clip plays as
silence. The undo history starts empty and clean.

**Takes**
- `folder: impl AsRef<Path>` — a session folder, one `create` made
- `block_size: usize` — frames per engine block

**Returns** `Result<Session>` — the `Session` as saved: memory, routing, tempo, and the audio its clips refer to, read from `audio/`; state comes back only once `state_file` is named

**Errors** no `memory.json` in the folder, a file written by a newer version, or malformed JSON; a missing audio file leaves its clip silent rather than failing.

**Undo** none: not an edit.

**Made of** `Memory::load` → `Session::new` → `wav::read` per audio clip.

**Example**

```rust
let mut session = session::open("~/songs/demo", 256)?;
session::state_file(&mut session, Some(session.folder.join("state.json")))?;
```

### `save`

```rust
pub fn save(session: &mut Session) -> Result<()>
```

Saves the session into its folder: the memory as `memory.json`, after
writing any audio a clip refers to that is not on disk yet (recordings
already are). The history remembers this state as saved
(`session.history.is_clean()`).

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<()>` — nothing; `session.history.is_clean()` is `true` afterwards

**Errors** the folder or `memory.json` cannot be written; audio a clip refers to cannot be written.

**Undo** none: not an edit.

**Made of** `wav::write` per audio clip not yet on disk → `Memory::save` → `UndoStack::mark_clean`.

**Example**

```rust
session::save(&mut session)?;
```

### `import_audio`

```rust
pub fn import_audio(session: &mut Session, file: impl AsRef<Path>) -> Result<Imported>
```

Brings an audio file into the session and says what it was. With the `files`
feature any format symphonia decodes is read (WAV, AIFF, FLAC, ALAC, MP3,
AAC in MP4 or M4A, Ogg Vorbis, CAF, Matroska) and resampled to the session's
rate when it differs; without it, a WAV already at the session's rate. The
audio is written into the folder as 32-bit float at the session's rate,
`audio/<name>.wav` (`<name> 2.wav` when that is taken), and kept in memory;
`clip::add` places it by the returned key. Not an edit: the file joins the
folder and stays, placing it is the undo step.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `file: impl AsRef<Path>` — the audio file to bring in; WAV, AIFF, FLAC, ALAC, MP3, AAC (MP4/M4A), Ogg Vorbis, CAF or Matroska with the `files` feature

**Returns** `Result<Imported>` — an `Imported`: the key (`audio/<name>.wav`) a clip refers to the audio by, the file's own sample rate and bit depth, channels, frames, and whether it was resampled

**Errors** the file cannot be read or decoded; without the `files` feature, anything but a WAV at the session's rate.

**Undo** none: not an edit; placing the clip with `clip::add` is the undo step.

**Made of** `io::files::decode` → `AudioFile::resampled_to` (or `io::wav::read_with_format`) → `folder::free_key` → `folder::write_audio` → `AudioCache::insert`.

**Example**

```rust
let take = session::import_audio(&mut session, "/music/take.flac")?;
println!("{take}");   // audio/take.wav: 44100 Hz, 24-bit, 2 channel(s), resampled to 48000 Hz, stored as 32-bit float
clip::add(&mut session, vox, take.key, TimePosition::ZERO)?;
```

### `state_file`

```rust
pub fn state_file(session: &mut Session, path: Option<PathBuf>) -> Result<()>
```

Says where state is kept, or that it is not. State (a track's mute, solo,
arm, monitor and input, a clip's mute, a send's on/off, looping, the click
and its count-in, the playhead) is never written into `memory.json`: until
an application names a file here, a session opens with all of it off. With a
path, the file is read now if it exists (its state lands on the memory, the
mixer and the engine) and rewritten after every state change and every
[`save`], with the playhead. `None` stops writing; the file stays.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `path: Option<PathBuf>` — `Some(path)`: keep state there, reading it now if it exists; `None`: stop keeping state

**Returns** `Result<()>` — nothing

**Errors** the file exists but cannot be read, is malformed or was written by a newer version; the file cannot be written.

**Undo** none: not an edit.

**Made of** `Memory::load_state` → `Memory::apply_state` → `EngineHandle::send(SetLoopEnabled, Seek)` → `session::changed_structure`; then `Memory::save_state` from `session::set_state` and [`save`].

**Example**

```rust
session::state_file(&mut session, Some(session.folder.join("state.json")))?;   // read now, written from here on
session::state_file(&mut session, None)?;                                            // stop writing; the file stays
```

### `set_tempo`

```rust
pub fn set_tempo(session: &mut Session, bpm: f64) -> Result<()>
```

Sets the memory's tempo at the start. Consecutive changes are one undo step.
MIDI clips are scheduled through the tempo map, so the mixer rebuilds, and
the rebuild hands the engine the new map.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `bpm: f64` — beats (quarter notes) per minute

**Returns** `Result<()>` — nothing

**Errors** a tempo at or below zero.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `SetTempo` at beat 0 through `session::structural`.

**Example**

```rust
session::set_tempo(&mut session, 100.0)?;
```

### `tempo_change`

```rust
pub fn tempo_change(session: &mut Session, beat: BeatPosition, bpm: Option<f64>) -> Result<()>
```

Adds a tempo change at `beat` (`Some(bpm)`), or removes the one there
(`None`); the tempo at beat zero is set with [`set_tempo`] and cannot be
removed. Changes to the same beat coalesce. Rebuilds the mixer, which hands
the engine the new map.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `beat: BeatPosition` — where the change takes effect, in quarter notes from the start
- `bpm: Option<f64>` — `Some(bpm)` adds or changes the tempo there; `None` removes the change

**Returns** `Result<()>` — nothing

**Errors** a beat before zero; removing the change at beat zero (use `set_tempo`); removing a change that is not there.

**Undo** one step per beat; changes to the same beat coalesce; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `SetTempo` or `RemoveTempo` through `session::structural`.

**Example**

```rust
session::tempo_change(&mut session, BeatPosition::new(16.0), Some(140.0))?;   // faster from bar 5
session::tempo_change(&mut session, BeatPosition::new(16.0), None)?;          // and back
```

### `meter_change`

```rust
pub fn meter_change( session: &mut Session, bar: u32, signature: Option<TimeSignature>, ) -> Result<()>
```

Sets the meter (time signature) from `bar` on (`Some`), or removes the
change at that bar (`None`); bar 1 always has a meter. Beats stay quarter
notes whatever the signature, as in Logic and Cubase; the click ticks on the
notated beat. Rebuilds the mixer, which hands the engine the new map.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `bar: u32` — the 1-based bar the new meter starts at
- `signature: Option<TimeSignature>` — `Some(signature)` sets the meter from that bar on; `None` removes the change there

**Returns** `Result<()>` — nothing

**Errors** bar 0; removing the meter at bar 1; removing a change that is not there.

**Undo** one undo step; rebuilds the mixer (at once while a device plays, otherwise at the next start).

**Made of** `SetMeter` through `session::structural`.

**Example**

```rust
session::meter_change(&mut session, 9, Some(TimeSignature::new(6, 8)?))?;
```

### `set_loop`

```rust
pub fn set_loop(session: &mut Session, range: Option<LoopRange>) -> Result<()>
```

Sets or clears the loop range in the memory and on the running engine. Turn
looping on with [`transport::looping`](crate::transport::looping).

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `range: Option<LoopRange>` — `Some(range)` sets the loop range, told to the engine at once; `None` clears it

**Returns** `Result<()>` — nothing

**Errors** a range whose end is not after its start.

**Undo** one undo step per drag: calls within `Session::COALESCE_WINDOW` (half a second) merge.

**Made of** push `SetLoopRange` → `EngineHandle::send(SetLoop)`.

**Example**

```rust
session::set_loop(&mut session, Some(LoopRange::new(TimePosition::ZERO, TimePosition::from_seconds(8.0, rate))?))?;
transport::looping(&mut session, true)?;
```

### `undo`

```rust
pub fn undo(session: &mut Session) -> Result<bool>
```

Reverts the last edit. Returns `false` if there was nothing to undo.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<bool>` — `true` when a step was undone, `false` when there was nothing to undo

**Errors** a command fails to revert (should not happen).

**Undo** this is the undo.

**Made of** `UndoStack::undo`; the mixer is marked for a rebuild because the undone edit may have been structural.

**Example**

```rust
if session::undo(&mut session)? { println!("undid {:?}", session.history.redo_label()); }
```

### `redo`

```rust
pub fn redo(session: &mut Session) -> Result<bool>
```

Re-applies the last undone edit.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<bool>` — `true` when a step was redone, `false` when there was nothing to redo

**Errors** a command fails to apply again.

**Undo** this is the redo.

**Made of** `UndoStack::redo`, then the same rebuild marking as [`undo`].

**Example**

```rust
session::redo(&mut session)?;
```

## Types

### `Session`

A session: the memory you edit plus everything needed to play it.  Plain
record, public fields, constructors only. Every flow function takes `&mut
Session`; reach into any field and call a primitive directly when a flow
does not do exactly what you want.

| field | type | meaning |
|---|---|---|
| `memory` | `Memory` | The memory: tracks, clips, markers, routing, tempo. |
| `history` | `UndoStack<Memory>` | Undo history for `memory`. Every flow edit pushes through this stack, so every edit is reversible. Consecutive edits of the same control (a fader, a knob, a clip being dragged) merge into one step while they arrive within [`Session::COALESCE_WINDOW`] of each other; a pause starts a new step. Call `history.seal()` to end a step sooner (on mouse up, say). |
| `audio` | `AudioCache` | Audio held in memory, keyed by the path clips refer to, relative to the session folder (`audio/vox 1.wav`). |
| `folder` | `PathBuf` | The folder the session lives in: `memory.json` and the `audio/` folder of recordings are inside. Set when the session is created or opened; recordings are written there as they stop. |
| `engine` | `EngineHandle` | Control handle to the engine: commands, graph swaps, position. |
| `audio_thread` | `Option<AudioThread>` | The engine's audio side. Present until a device takes it (`devices::output`, feature `devices`); while present, [`steps::render_blocks`] ticks it by hand. |
| `mixer` | `MixerHandles` | The live controls of the mixer built by the last [`steps::rebuild`]: one per strip and send, keyed by the memory's ids. |
| `recording` | `Option<crate::record::Recording>` | A take being recorded, if any. |
| `channels` | `usize` | Channels per strip and at the output: 2, stereo. Change it before the first rebuild if you need another width. |
| `output` | `Option<crate::primitives::io::LiveOutput>` | The device output, once `devices::output` started one: the audio thread lives inside it until `devices::close_output`. |
| `input` | `Option<crate::devices::AudioInput>` | The audio input, once `devices::input` opened it: what monitoring audio tracks hear and device recordings capture. |
| `midi_inputs` | `Vec<crate::primitives::midi::input::MidiInputBridge>` | Hardware MIDI inputs connected so far. Dropping one disconnects it. |
| `state_file` | `Option<PathBuf>` | Where state is kept, once [`state_file`] named a file; `None` means state is not saved. |

| method | meaning |
|---|---|
| `const COALESCE_WINDOW: core::time::Duration` | How long a pause between two edits of the same control turns them into separate undo steps. |
| `fn new(memory: Memory, folder: impl Into<PathBuf>, block_size: usize) -> Result<Session>` | A stereo session around `memory` living in `folder`, with an engine that renders blocks of at most `block_size` frames at the memory's sample rate. Records the folder and touches nothing on disk; [`create`] and [`open`] do. |

### `MixerHandles`

The live controls of the mixer the engine is running, keyed by the track or
send each was built for.  Filled by every [`steps::rebuild`]. The `Mixer`
itself becomes the graph the engine runs; what the control side keeps are
these handles, whose atomics the audio thread reads every block, so a change
made through them is heard at the next block. Handles from before a rebuild
are stale: look them up again.

| field | type | meaning |
|---|---|---|
| `strips` | `Vec<(TrackId, Arc<StripParams>)>` | A track or bus in the memory → its strip: volume, pan, mute, solo, pan law and the peak meter. |
| `sends` | `Vec<(SendId, Arc<SendParams>)>` | A send in the memory → its level and on/off. |
| `click` | `Arc<ClickParams>` | The metronome's click on the master: on/off, level, count-in. |

| method | meaning |
|---|---|
| `fn strip(&self, track: TrackId) -> Option<&Arc<StripParams>>` | The live strip of a track or bus. |
| `fn send(&self, send: SendId) -> Option<&Arc<SendParams>>` | The live controls of a send. |

### `Imported`

What [`import_audio`] brought in: the key a clip refers to the audio by, and
what the file was, so an application can say "44.1 kHz, 24-bit, stereo,
resampled". Whatever the file was, the session stores it as 32-bit float at
its own rate.

| field | type | meaning |
|---|---|---|
| `key` | `PathBuf` | `audio/<name>.wav`: the key a clip refers to the audio by. |
| `source_rate` | `SampleRate` | The file's own sample rate. |
| `source_bits` | `Option<u32>` | The file's own bits per sample when it has a fixed depth (16, 24, 32); `None` for lossy codecs such as MP3 and AAC, which have none. |
| `channels` | `usize` | Channels. |
| `frames` | `usize` | Frames as stored, at the session's rate. |
| `stored_rate` | `SampleRate` | The session's rate, which the stored audio is at. |
| `resampled` | `bool` | Whether the audio was resampled from `source_rate` to `stored_rate`. |

## Usage

```rust
use superb::prelude::*;
use superb::{clip, marker, session, track};

let mut session = session::create("demo.superb", SampleRate::HZ_48K, 256)?;   // a folder
let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
let take = session::import_audio(&mut session, "/music/take.wav")?;   // audio/take.wav
clip::add(&mut session, vox, take, TimePosition::ZERO)?;
session::set_tempo(&mut session, 100.0)?;
marker::add(&mut session, "verse", TimePosition::from_samples(48_000))?;
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
`wav::read` for the audio. `set_tempo`, `tempo_change` and
`meter_change` go through `session::structural` (`SetTempo`,
`RemoveTempo`, `SetMeter`); `set_loop` additionally sends
`EngineCommand::SetLoop`. `rebuild`: `EngineHandle::reclaim` →
`build_mixer` → `publish` (which waits for room in the swap queue while a
device drives the engine), then `LiveGraph::apply_pending` while the audio
thread is still in the session. `render_blocks`: `AudioThread::tick` per
block, plus `record::collect` while a recording runs.

The crate-private helpers `edit`, `structural`, `changed_structure` and
`set_state` are how the other flows change the memory; see
[flows.md](../flows.md).

## Limitations

* Beats are quarter notes whatever the meter, as in Logic and Cubase: a
  6/8 bar is three beats long and the click ticks eighths. Tempo ramps
  (`SetTempo::ramped`) have no verb yet; push the command yourself.
* The SDK speaks through the `log` facade for things that are not
  errors but you would want to know: a session opened without a state
  file, the first state change that will not be saved, a save that left
  state out. Applications pick the logger (`env_logger`, say); without
  one the lines go nowhere.
* An audio file is described by its sample rate (samples per second per
  channel: 44 100, 48 000, 96 000 Hz), its bit depth (how many bits each
  sample is stored in: 16 or 24 for integer PCM, 32 for float; lossy codecs
  such as MP3 and AAC have none), its channel count, and the container and
  codec it uses (WAV and AIFF hold PCM; FLAC and ALAC compress without
  loss; MP3, AAC and Vorbis lose detail). A session has one sample rate, so
  `import_audio` resamples a file at another rate once, on the way in, and
  stores every file as 32-bit float, which holds 24-bit audio exactly. The
  returned `Imported` reports all of this, and the log notes a resample.
