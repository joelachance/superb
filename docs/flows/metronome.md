# metronome

A click while the transport rolls; a count-in before a recording.

## Example

```rust
use superb::primitives::session::TrackKind;
use superb::primitives::{Result, SampleRate, TimePosition};
use superb::{metronome, record, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("click"), rate, 256)?;
    session::set_tempo(&mut session, 120.0)?;                 // a beat is half a second
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    track::arm(&mut session, vox, true)?;

    metronome::enable(&mut session, true)?;                   // state
    metronome::level(&mut session, -6.0)?;                    // heard at once
    transport::start(&mut session)?;
    let audio = session::steps::render_blocks(&mut session, 94)?;   // one beat
    println!("click peak {:.3}", audio.peak());
    transport::stop(&mut session)?;

    metronome::count_in(&mut session, 1)?;                    // one bar of click before a recording
    transport::seek(&mut session, TimePosition::from_seconds(2.0, rate))?;
    record::start(&mut session, record::Source::Device(None))?;   // rolls from 0 s, records from 2 s
    Ok(())
}
```

`cargo run --example metronome_count_in` shows the count-in dropping into record.

## Verbs

| verb | does | undo |
|---|---|---|
| `enable(session, on)` | the click while rolling | state |
| `level(session, db)` | heard at once | drag |
| `count_in(session, bars)` | bars of click before a recording that starts at the playhead; `0` for none | state |

Every signature, argument, error and undo step: [metronome reference](metronome.ref.md).

## Notes

* The click follows tempo and meter changes, ticks the notated beat
  (quarters in 4/4, eighths in 6/8) and accents the first beat of a bar.
* The count-in clicks even when the metronome is off, and stops at the
  punch-in. With a punch-in ahead of the playhead there is no count-in.
* The click is never in a bounce.
