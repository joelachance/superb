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
| `output_config(session, Option<name>) -> OutputConfig` | `devices` | the session's sample rate, block size and channel count on the named or default device |
| `input_config(session, Option<name>, channels) -> InputConfig` | `devices` | likewise for an input |
| `midi_inputs()` | `midi-io` | MIDI input ports |

## Usage

```rust
use superb::{devices, record, transport};

for d in devices::outputs()? {
    println!("{}{}", d.name, if d.is_default { " *" } else { "" });
}
let speakers = devices::output_config(&session, None);
devices::output(&mut session, &speakers)?;

let mic = devices::input_config(&session, None, 1);
record::start(&mut session, &mic)?;
```

## Made of

`io::device::output_devices` / `input_devices`, `OutputConfig` /
`InputConfig` filled from `session.session.sample_rate()`, `session.channels` and
`session.engine.max_frames()`; `midi::input::list_input_ports`.

## Notes

* The configurations must match the engine's rate and block size: every
  published graph is prepared for them, and a stream opened with different
  values is refused rather than re-prepared on the audio thread. Using
  these helpers guarantees the match.
* Device indices and names are a snapshot; re-list after plugging
  something in.
