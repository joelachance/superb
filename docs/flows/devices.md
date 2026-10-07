# devices

Which audio and MIDI devices exist; playing through an output; opening the
input that monitoring hears and recordings capture.

## Purpose

Everything here needs a feature: `devices` for audio, `midi-io` for MIDI
ports. Without them the module is empty and the session renders through
`session::steps::render_blocks` or `export`.

## Verbs

| verb | feature | does |
|---|---|---|
| `outputs()` / `inputs()` | `devices` | device listings with the default marked and the channel count when known |
| `output(session, Option<name>)` | `devices` | play the session through that device (`None` = default): the audio thread moves into a stream matched to the session, and structural edits rebuild at once |
| `input(session, Option<name>)` | `devices` | open the session's audio input on that device, with every channel it has; monitoring audio tracks hear it and `record::start(Source::Device)` records from it; one per session, kept in `session.input` |
| `close_output(session)` | `devices` | stop playing through the device and take the audio thread back: engine, position, input and MIDI ports intact; `output` can then start another device. Refused while recording |
| `close_input(session)` | `devices` | close it; refused while a device recording runs. Open again to change the device |
| `midi_inputs()` | `midi-io` | MIDI input ports |

## Reference

Every function of the `devices` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `outputs`

```rust
pub fn outputs() -> Result<Vec<DeviceInfo>>
```

*Feature `devices`.*

Audio output devices of the default host.

**Returns** `Result<Vec<DeviceInfo>>` — every audio output device: name, whether it is the default, channels when known

**Errors** the audio host cannot be reached.

**Undo** none.

**Made of** `device::output_devices`.

**Example**

```rust
for d in devices::outputs()? { println!("{}{}", d.name, if d.is_default { " (default)" } else { "" }); }
```

### `inputs`

```rust
pub fn inputs() -> Result<Vec<DeviceInfo>>
```

*Feature `devices`.*

Audio input devices of the default host.

**Returns** `Result<Vec<DeviceInfo>>` — every audio input device, the same way

**Errors** the audio host cannot be reached.

**Undo** none.

**Made of** `device::input_devices`.

**Example**

```rust
let inputs = devices::inputs()?;
```

### `output`

```rust
pub fn output(session: &mut Session, device: Option<&str>) -> Result<()>
```

*Feature `devices`.*

Plays the session through an output device (`None` for the default, or a
name from [`outputs`]): moves the audio thread into a real output stream at
the session's sample rate, block size and channel count, and starts it. From
then on the device drives the engine, structural edits rebuild the mixer at
once, and
[`session::steps::render_blocks`](crate::session::steps::render_blocks) is
unavailable.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `device: Option<&str>` — `Some(name)` from `outputs()`, or `None` for the default

**Returns** `Result<()>` — nothing; `session.output` holds the stream and the audio thread inside it

**Errors** the audio thread is already on a device (`close_output` first); the device refuses the session's rate, block size or channel count; the mixer cannot be built.

**Undo** none.

**Made of** an `OutputConfig` matching the session → [`session::steps::rebuild`](crate::session::steps::rebuild) when needed → `AudioThread::run_on_device`; the result is kept in [`Session::output`] until [`close_output`].

**Example**

```rust
devices::output(&mut session, None)?;                  // the default output drives the engine from here
devices::output(&mut session, Some("Scarlett 2i2"))?;
```

### `close_output`

```rust
pub fn close_output(session: &mut Session) -> Result<()>
```

*Feature `devices`.*

Stops playing through the device and takes the audio thread back into the
session: the engine, the transport's position, the live input and the MIDI
ports stay as they were,
[`session::steps::render_blocks`](crate::session::steps::render_blocks)
works again, and [`output`] can start another device. Refused while a
recording runs.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<()>` — nothing; `session.audio_thread` has the thread back

**Errors** no device output open; a recording running.

**Undo** none.

**Made of** `LiveOutput::stop` → [`Session::audio_thread`].

**Example**

```rust
devices::close_output(&mut session)?;
devices::output(&mut session, Some("Headphones"))?;   // move to another device
```

### `input`

```rust
pub fn input(session: &mut Session, device: Option<&str>) -> Result<()>
```

*Feature `devices`.*

Opens the session's audio input (`None` for the default device, or a name
from [`inputs`]): a stream at the session's sample rate and block size with
every channel the device has, so any of them can feed a track (the tracks'
inputs' width, at least two, when the device does not say). From then on a
monitoring audio track ([`track::monitor`](crate::track::monitor)) hears it
through its strip, and `record::start` with `Source::Device` records from
it. One input per session, until [`close_input`].

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `device: Option<&str>` — `Some(name)` from `inputs()`, or `None` for the default

**Returns** `Result<()>` — nothing; `session.input` holds the stream

**Errors** an input already open; the device cannot be opened at the session's rate and block size.

**Undo** none.

**Made of** an `InputConfig` matching the session → two capture queues (one the engine reads as its live input, one a recording drains) → `InputStream::open_timed` pushing every block to both → `EngineHandle::send(LiveInput)`; the stream is kept in [`Session::input`].

**Example**

```rust
devices::input(&mut session, None)?;
track::monitor(&mut session, vox, true)?;   // hear it
```

### `close_input`

```rust
pub fn close_input(session: &mut Session) -> Result<()>
```

*Feature `devices`.*

Closes the session's audio input: monitoring audio tracks fall silent and
the next `record::start` with `Source::Device` opens it again, which is also
how to change the device or the channel count. Refused while a device
recording is drawing from it.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably

**Returns** `Result<()>` — nothing

**Errors** no input open; a device recording running.

**Undo** none.

**Made of** `EngineHandle::send(LiveInput(None))` → drop the stream; the engine hands the live input back and `reclaim` frees it at the next rebuild or render.

**Example**

```rust
devices::close_input(&mut session)?;
```

### `midi_inputs`

```rust
pub fn midi_inputs() -> Result<Vec<crate::primitives::midi::input::MidiPortInfo>>
```

*Feature `midi-io`.*

Hardware MIDI input ports.

**Returns** `Result<Vec<crate::primitives::midi::input::MidiPortInfo>>` — every MIDI input port: index and name

**Errors** the MIDI backend cannot be reached.

**Undo** none.

**Made of** `midi::input::list_input_ports`.

**Example**

```rust
for p in devices::midi_inputs()? { println!("{}: {}", p.index, p.name); }
```

## Types

### `AudioInput` *(feature `devices`)*

The session's audio input, once [`input`] opened it: the running stream and
what it feeds. Monitoring audio tracks hear it through the engine's live
input; a device recording drains its own queue of the same blocks.

| method | meaning |
|---|---|
| `fn channels(&self) -> usize` | Channels in every captured block. |
| `fn device_name(&self) -> &str` | Name of the device in use. |

## Usage

```rust
use superb::{devices, record, transport};

for d in devices::outputs()? {
    println!("{}{}", d.name, if d.is_default { " *" } else { "" });
}
devices::output(&mut session, None)?;          // the default output
devices::input(&mut session, None)?;           // the default input
track::monitor(&mut session, vox, true)?;      // hear it on the vox strip

record::start(&mut session, record::Source::Device(None))?;   // records from the same input
```

## Made of

`io::device::output_devices` / `input_devices`, `OutputConfig` /
`InputConfig` filled from `session.memory.sample_rate()`, `session.channels` and
`session.engine.max_frames()`; `input`: two capture queues,
`InputStream::open_timed` pushing every block to both (one the engine
reads as its `LiveInput`, one a recording drains while it runs) →
`EngineHandle::send(LiveInput(Some(..)))`; `close_input`:
`EngineHandle::send(LiveInput(None))` and drop the stream;
`midi::input::list_input_ports`.

## Notes

* A stream must match the engine's rate and block size: every published
  graph is prepared for them, and a stream opened with different values is
  refused rather than re-prepared on the audio thread. `output` and
  `record::start` build the configuration from the session, so it cannot
  mismatch.
* Device indices and names are a snapshot; re-list after plugging
  something in.
* The input opens with every channel the device has, so a track can pick
  any of them; when the device does not say how many, the tracks' inputs
  decide, at least two.
* Switching outputs is `close_output` then `output`: the engine keeps
  playing position, live input and MIDI ports across the move.
* A device take is lined up with the transport by the stream clock, so
  the buffers in and out never shift it.
