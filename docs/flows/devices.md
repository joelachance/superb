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
