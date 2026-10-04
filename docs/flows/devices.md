# devices

Which audio and MIDI devices exist, and configurations that match the session.

## Purpose

Everything here needs a feature: `devices` for audio, `midi-io` for MIDI
ports. Without them the module is empty and the session renders through
`session::steps::render_blocks` or `export`.

## Verbs

| verb | feature | does |
|---|---|---|
| `outputs()` / `inputs()` | `devices` | device listings with the default marked and the channel count when known |
| `output(session, Option<name>)` | `devices` | play the session through that device (`None` = default): the engine loop moves into a stream matched to the session, and structural edits rebuild at once |
| `midi_inputs()` | `midi-io` | MIDI input ports |

## Usage

```rust
use superb::{devices, record, transport};

for d in devices::outputs()? {
    println!("{}{}", d.name, if d.is_default { " *" } else { "" });
}
devices::output(&mut session, None)?;          // the default output

record::start(&mut session, None, 1)?;         // the default input, one channel
```

## Made of

`io::device::output_devices` / `input_devices`, `OutputConfig` /
`InputConfig` filled from `session.session.sample_rate()`, `session.channels` and
`session.engine.max_frames()`; `midi::input::list_input_ports`.

## Notes

* A stream must match the engine's rate and block size: every published
  graph is prepared for them, and a stream opened with different values is
  refused rather than re-prepared on the audio thread. `output` and
  `record::start` build the configuration from the session, so it cannot
  mismatch.
* Device indices and names are a snapshot; re-list after plugging
  something in.
