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
| `monitor(rig, track, on)` | switch monitoring on a MIDI track (saved, not undoable, needs sync); audio tracks are refused |
| `is_monitoring(rig, track)` | |
| `play_note(rig, channel, note, velocity)` / `release_note(rig, channel, note)` | queue a note for the next block |
| `send(rig, MidiMessage)` | queue any message |
| `inputs()` (feature `midi-io`) | hardware input ports |
| `connect_input(rig, port_index)` (feature `midi-io`) | route a port into the engine; the connection lives in `rig.midi_inputs` |

## Usage

```rust
use superb::{midi, play, project};
use superb::prelude::*;

let keys = project::add_track(&mut rig, "keys", TrackKind::Midi)?;
midi::monitor(&mut rig, keys, true)?;
play::start(&mut rig)?;
midi::play_note(&mut rig, 0, 60, 100)?;
let audio = play::advance(&mut rig, 8)?;        // middle C on the keys strip
midi::release_note(&mut rig, 0, 60)?;
```

## Made of

`monitor`: `Track::armed` through `project::switch` (a MIDI track's
"armed" flag means monitoring), `needs_sync`. `play_note` / `release_note`:
`MidiMessage::note_on` / `note_off` → `send`. `send`:
`LiveEngine::push_midi` with a zero timestamp. `connect_input`:
`MidiInputBridge::connect` → `LiveEngine::send(EngineCommand::AddMidiInput)`.

## Notes

* The live instrument is `MonoSynthNode`: a simple monophonic synth meant
  for hearing that MIDI arrives. An instrument plugin on the track
  (`plugins::insert`) is the real thing.
* Software notes land at the start of the next block. Hardware notes are
  stamped on the MIDI clock and land at block resolution until the clocks
  are aligned (see ARCHITECTURE.md, "MIDI").
* MIDI clips on the timeline play regardless of monitoring; monitoring is
  about live input.
