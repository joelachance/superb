# send

A copy of a track into a bus, pre- or post-fader.

## Example

```rust
use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::mixer::SendTap;
use superb::primitives::session::TrackKind;
use superb::primitives::{Result, SampleRate, TimePosition};
use superb::{clip, send, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("sends"), rate, 256)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let gtr = track::add(&mut session, "gtr", TrackKind::Audio)?;
    let room = track::add(&mut session, "room", TrackKind::Bus)?;   // the reverb return, minus the reverb

    let mut tone = PlanarBuffer::new(2, rate.hz() as usize);
    tone.fill(0.25);
    session.audio.insert("audio/tone.wav", tone);
    for t in [vox, gtr] {
        clip::add(&mut session, t, "audio/tone.wav", TimePosition::ZERO)?;
    }

    let vox_room = send::add(&mut session, vox, room, -6.0, SendTap::PostFader)?;
    let gtr_room = send::add(&mut session, gtr, room, -6.0, SendTap::PreFader)?;
    send::level(&mut session, vox_room, -12.0)?;      // heard at once; one undo step per drag
    send::enable(&mut session, gtr_room, false)?;     // state: saved, not an undo step
    track::volume(&mut session, room, -3.0)?;         // a bus is a track

    transport::start(&mut session)?;
    let audio = session::steps::render_blocks(&mut session, 8)?;
    println!("peak {:.3}", audio.peak());

    send::remove(&mut session, gtr_room)?;
    track::route(&mut session, gtr, Some(room))?;     // the whole track through the bus instead
    Ok(())
}
```

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, track, bus, level_db, SendTap) -> SendId` | `SendTap::PostFader` or `PreFader` | one |
| `remove(session, send)` | | one |
| `level(session, send, db)` | heard at once | drag |
| `enable(session, send, on)` | heard at once | state |

Every signature, argument, error and undo step: [send reference](send.ref.md).

## Notes

* A bus is a track of kind `Bus`: `track::volume`, `mute`, `route` and
  `peak` work on it. `track::route` sends the whole track through a bus;
  a send keeps the track in the mix and adds a copy.
* A bus routed into itself is refused.
