# devices

Which audio and MIDI devices exist, and configurations that match the rig.

## Purpose

Everything here needs a feature: `devices` for audio, `midi-io` for MIDI
ports. Without them the module is empty and the rig renders through
`play::drive` or `export`.

## Verbs

| verb | feature | does |
|---|---|---|
| `outputs()` / `inputs()` | `devices` | device listings with the default marked and the channel count when known |
| `output_config(rig, Option<name>) -> OutputConfig` | `devices` | the rig's sample rate, block size and channel count on the named or default device |
| `input_config(rig, Option<name>, channels) -> InputConfig` | `devices` | likewise for an input |
| `midi_inputs()` | `midi-io` | MIDI input ports |

## Usage

```rust
use superb::{devices, play, record};

for d in devices::outputs()? {
    println!("{}{}", d.name, if d.is_default { " *" } else { "" });
}
let speakers = devices::output_config(&rig, None);
play::start_output(&mut rig, &speakers)?;

let mic = devices::input_config(&rig, None, 1);
record::start(&mut rig, &mic)?;
```

## Made of

`io::device::output_devices` / `input_devices`, `OutputConfig` /
`InputConfig` filled from `rig.session.sample_rate()`, `rig.channels` and
`rig.engine.max_frames()`; `midi::input::list_input_ports`.

## Notes

* The configurations must match the engine's rate and block size: every
  published graph is prepared for them, and a stream opened with different
  values is refused rather than re-prepared on the audio thread. Using
  these helpers guarantees the match.
* Device indices and names are a snapshot; re-list after plugging
  something in.
