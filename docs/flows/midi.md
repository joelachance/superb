# midi

Sending notes, connecting a keyboard.

## Purpose

MIDI that reaches the engine sounds on every MIDI track that is
monitoring (`track::monitor`), on the MIDI channel the track listens to
(`track::midi_channel`, all by default). Notes can come from the program
(`play_note`, `send`) or, with the `midi-io` feature, from a hardware port
(`connect_input`). While a recording runs, the same MIDI lands as notes on
every armed MIDI track (`record`).

## Verbs

| verb | does |
|---|---|
| `play_note(session, channel, note, velocity)` / `release_note(session, channel, note)` | queue a note for the next block |
| `send(session, MidiMessage)` | queue any message |
| `connect_input(session, port_index)` (feature `midi-io`) | route a port (listed by `devices::midi_inputs`) into the engine; the connection lives in `session.midi_inputs` |

## Reference

Every function of the `midi` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `play_note`

```rust
pub fn play_note(session: &mut Session, channel: u8, note: u8, velocity: u8) -> Result<()>
```

Plays a note on the engine's software input. It sounds at the start of the
next block on every monitoring track. `channel` is `0..=15`, `note` and
`velocity` are `0..=127`; a velocity of `0` releases the note.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `channel: u8` — the MIDI channel, `0..=15`
- `note: u8` — the MIDI note number, `0..=127` (60 is middle C)
- `velocity: u8` — `1..=127`; `0` releases the note

**Returns** `Result<()>` — nothing

**Errors** a channel above 15, a note or velocity above 127; the MIDI queue is full.

**Undo** none: not an edit.

**Made of** `MidiMessage::note_on` → [`send`].

**Example**

```rust
midi::play_note(&mut session, 0, 60, 100)?;   // middle C on channel 1
```

### `release_note`

```rust
pub fn release_note(session: &mut Session, channel: u8, note: u8) -> Result<()>
```

Releases a note started with [`play_note`].

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `channel: u8` — the MIDI channel, `0..=15`
- `note: u8` — the MIDI note number

**Returns** `Result<()>` — nothing

**Errors** a channel above 15 or a note above 127; the MIDI queue is full.

**Undo** none: not an edit.

**Made of** `MidiMessage::note_off` → [`send`].

**Example**

```rust
midi::release_note(&mut session, 0, 60)?;
```

### `send`

```rust
pub fn send(session: &mut Session, message: MidiMessage) -> Result<()>
```

Queues any MIDI message for the start of the engine's next block.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `message: MidiMessage` — any `MidiMessage`

**Returns** `Result<()>` — nothing

**Errors** the MIDI queue is full.

**Undo** none: not an edit.

**Made of** `EngineHandle::push_midi` with a zero timestamp.

**Example**

```rust
midi::send(&mut session, MidiMessage::control_change(MidiChannel::new(0)?, 1, 64)?)?;
```

### `connect_input`

```rust
pub fn connect_input(session: &mut Session, port_index: usize) -> Result<()>
```

*Feature `midi-io`.*

Connects a hardware MIDI input (an index from
[`devices::midi_inputs`](crate::devices::midi_inputs)) to the engine. Its
notes land at the start of the block they arrive in and play on every
monitoring track. The connection lives in [`Session::midi_inputs`] and
closes when removed from there.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `port_index: usize` — the port's `index` from `devices::midi_inputs()`, a snapshot that changes when devices are plugged in

**Returns** `Result<()>` — nothing

**Errors** no port at that index; the port cannot be opened.

**Undo** none: not an edit.

**Made of** `MidiInputBridge::connect` → `EngineHandle::send(AddMidiInput)`.

**Example**

```rust
for port in devices::midi_inputs()? { midi::connect_input(&mut session, port.index)?; }
```

## Usage

```rust
use superb::prelude::*;
use superb::{midi, session, track, transport};

let keys = track::add(&mut session, "keys", TrackKind::Midi)?;
track::monitor(&mut session, keys, true)?;
transport::start(&mut session)?;
midi::play_note(&mut session, 0, 60, 100)?;
let audio = session::steps::render_blocks(&mut session, 8)?;   // middle C on the keys strip
midi::release_note(&mut session, 0, 60)?;
```

## Made of

`play_note` / `release_note`: `MidiMessage::note_on` / `note_off` →
`send`. `send`: `EngineHandle::push_midi` with a zero timestamp.
`connect_input`: `MidiInputBridge::connect` →
`EngineHandle::send(EngineCommand::AddMidiInput)`.

## Notes

* Software notes land at the start of the next block. Hardware notes are
  stamped on the MIDI clock and land at block resolution until the clocks
  are aligned (see ARCHITECTURE.md, "MIDI").
* MIDI clips on the timeline play regardless of monitoring; monitoring is
  about live input.
