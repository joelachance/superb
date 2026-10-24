# devices

Audio and MIDI hardware. Feature `devices` for audio, `midi-io` for MIDI ports.

## Example

```rust
use superb::primitives::session::TrackKind;
use superb::primitives::{Result, SampleRate};
use superb::{devices, record, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("live"), rate, 256)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;

    for d in devices::outputs()? {
        println!("{}{} ({} ch)", d.name, if d.is_default { " *" } else { "" }, d.max_channels);
    }
    for d in devices::inputs()? {
        println!("{}{}", d.name, if d.is_default { " *" } else { "" });
    }

    devices::output(&mut session, None)?;                 // the default output drives the engine
    devices::input(&mut session, None)?;                  // the default input, every channel
    track::monitor(&mut session, vox, true)?;             // hear it on the vox strip
    transport::start(&mut session)?;

    track::arm(&mut session, vox, true)?;
    record::start(&mut session, record::Source::Device(None))?;   // records from the same input
    std::thread::sleep(std::time::Duration::from_secs(5));
    record::stop(&mut session)?;

    devices::close_output(&mut session)?;                 // the engine keeps its position
    devices::output(&mut session, Some("Headphones"))?;   // and moves to another device
    devices::close_input(&mut session)?;
    Ok(())
}
```

MIDI ports, with `midi-io`:

```rust
for p in devices::midi_inputs()? {
    println!("{}: {}", p.index, p.name);
}
```

`cargo run --example list_devices --features hardware` lists everything.

## Verbs

| verb | feature | does |
|---|---|---|
| `outputs()` / `inputs()` | `devices` | `DeviceInfo` list: `name`, `is_default`, `max_channels` (0 when unknown) |
| `output(session, Option<name>)` | `devices` | play through that device; `None` for the default |
| `close_output(session)` | `devices` | take the audio thread back; refused while recording |
| `input(session, Option<name>)` | `devices` | open the input monitoring hears and `Source::Device` records from |
| `close_input(session)` | `devices` | refused while a device recording runs |
| `midi_inputs()` | `midi-io` | MIDI input ports: `index`, `name` |

Every signature, argument, error and undo step: [devices reference](devices.ref.md).

## Notes

* Streams are opened at the session's rate and block size for you; a
  device that cannot do them is refused.
* One input per session. Close it and open again to change device.
* Device lists are a snapshot; list again after plugging something in.
* While a device drives the engine, `render_blocks` is not available.
