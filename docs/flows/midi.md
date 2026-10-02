# midi

Hearing what you play, sending notes, connecting a keyboard.

## Purpose

A MIDI track that is *monitoring* gets a live instrument when the console
is built, so MIDI reaching the engine sounds on that track's strip. Notes
can come from the program or, with the `midi-io` feature, from a hardware
port.

## Verbs

| verb | does |
|---|---|
| `monitor(session, track, on)` | switch monitoring on a MIDI track (saved, not undoable, needs rebuild); audio tracks are refused |
| `is_monitoring(session, track)` | |
| `play_note(session, channel, note, velocity)` / `release_note(session, channel, note)` | queue a note for the next block |
| `send(session, MidiMessage)` | queue any message |
| `connect_input(session, port_index)` (feature `midi-io`) | route a port (listed by `devices::midi_inputs`) into the engine; the connection lives in `session.midi_inputs` |

## Usage

```rust
use superb::{midi, session, transport};
use superb::prelude::*;

let keys = track::add(&mut session, "keys", TrackKind::Midi)?;
track::monitor(&mut session, keys, true)?;
transport::start(&mut session)?;
midi::play_note(&mut session, 0, 60, 100)?;
let audio = session::steps::render_blocks(&mut session, 8)?;        // middle C on the keys strip
midi::release_note(&mut session, 0, 60)?;
```

## Made of

`monitor`: `Track::armed` through `session::switch` (a MIDI track's
"armed" flag means monitoring), `needs_rebuild`. `play_note` / `release_note`:
`MidiMessage::note_on` / `note_off` → `send`. `send`:
`EngineHandle::push_midi` with a zero timestamp. `connect_input`:
`MidiInputBridge::connect` → `EngineHandle::send(EngineCommand::AddMidiInput)`.

## Notes

* The live instrument is `MonoSynthNode`: a simple monophonic synth meant
  for hearing that MIDI arrives. An instrument plugin on the track
  (`plugins::insert`) is the real thing.
* Software notes land at the start of the next block. Hardware notes are
  stamped on the MIDI clock and land at block resolution until the clocks
  are aligned (see ARCHITECTURE.md, "MIDI").
* MIDI clips on the timeline play regardless of monitoring; monitoring is
  about live input.
