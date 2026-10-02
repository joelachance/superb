# midi

Sending notes, connecting a keyboard.

## Purpose

MIDI that reaches the engine sounds on every MIDI track that is
monitoring (`track::monitor`). Notes can come from the program
(`play_note`, `send`) or, with the `midi-io` feature, from a hardware port
(`connect_input`).

## Verbs

| verb | does |
|---|---|
| `play_note(session, channel, note, velocity)` / `release_note(session, channel, note)` | queue a note for the next block |
| `send(session, MidiMessage)` | queue any message |
| `connect_input(session, port_index)` (feature `midi-io`) | route a port (listed by `devices::midi_inputs`) into the engine; the connection lives in `session.midi_inputs` |

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
