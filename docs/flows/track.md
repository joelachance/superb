# track

Tracks: add, name, fader, pan, mute, solo, arm, monitor, route, meter, archive.

## Example

```rust
use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::session::{Input, TrackKind};
use superb::primitives::{Result, SampleRate, TimePosition};
use superb::{clip, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("tracks"), rate, 256)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let keys = track::add(&mut session, "keys", TrackKind::Midi)?;
    let room = track::add(&mut session, "room", TrackKind::Bus)?;

    let mut tone = PlanarBuffer::new(2, rate.hz() as usize);
    tone.fill(0.25);
    session.audio.insert("audio/tone.wav", tone);
    clip::add(&mut session, vox, "audio/tone.wav", TimePosition::ZERO)?;

    for db in [-2.0, -4.0, -6.0] {
        track::volume(&mut session, vox, db)?;     // one undo step: moves within half a second merge
    }
    track::pan(&mut session, vox, -0.3)?;          // a little left
    track::route(&mut session, vox, Some(room))?;  // vox → room bus → master
    track::volume(&mut session, room, -3.0)?;

    track::mute(&mut session, keys, true)?;        // state: saved, not an undo step
    track::arm(&mut session, vox, true)?;
    track::input(&mut session, vox, Some(Input::Mono(0)))?;
    track::monitor(&mut session, keys, true)?;     // keys sounds when MIDI arrives
    track::midi_channel(&mut session, keys, Some(0))?;

    transport::start(&mut session)?;
    session::steps::render_blocks(&mut session, 8)?;
    if let Some([l, r]) = track::peak(&session, room) {
        println!("room: L {l:.3} R {r:.3}");
    }

    track::rename(&mut session, vox, "lead vocal")?;
    track::color(&mut session, vox, Some(0xff8800))?;
    track::move_to(&mut session, keys, 0)?;        // first in the list
    track::archive(&mut session, keys)?;           // off the mixer, kept with its clips
    track::restore(&mut session, keys)?;
    Ok(())
}
```

`cargo run --example mix_session` does the same with the numbers printed.

## Verbs

| verb | does | undo |
|---|---|---|
| `add(session, name, kind) -> TrackId` | `Audio`, `Midi` or `Bus` | one |
| `archive(session, track)` / `restore(session, track)` | off the mixer and back; nothing is deleted | one |
| `rename(session, track, name)` | | merges |
| `color(session, track, Option<0xRRGGBB>)` | | merges |
| `move_to(session, track, index)` | position in the track list, 0 first | one |
| `volume(session, track, db)` | the fader; heard at once | drag |
| `pan(session, track, pan)` | `-1.0` left to `1.0` right | drag |
| `route(session, track, Option<bus>)` | into a bus, or `None` for the master | one |
| `mute` / `solo(session, track, on)` | heard at once | state |
| `arm(session, track, on)` | gets a clip when a recording stops | state |
| `monitor(session, track, on)` | MIDI track: a live instrument; audio track: the audio input | state |
| `input(session, track, Option<Input>)` | `Input::Mono(ch)`, `Input::Stereo(l, r)`, or `None` for everything captured | state |
| `midi_channel(session, track, Option<0..=15>)` | which channel the track listens to; `None` for all | state |
| `peak(session, track) -> Option<[f32; 2]>` | the strip's peak since the last read | |

Every signature, argument, error and undo step: [track reference](track.ref.md).

## Notes

* The master is a track: `session.memory.master()`. `volume`, `pan`, `mute`,
  `rename` and `peak` work on it; archive, solo, arm, monitor, input,
  routing and sends are refused.
* Solo silences every other track; buses ignore solo.
* A monitoring MIDI track gets `MonoSynthNode`, a simple synth for hearing
  that MIDI arrives. Instrument plugins come later.
* Monitoring an audio track needs the input open (`devices::input`).
* Pan law is a live-strip setting, `StripParams::set_pan_law`: tracks
  default to constant power, buses to balance.
