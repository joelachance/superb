# export

Bounce the session, or a range of it, to WAV; or every strip to its own file.

## Example

```rust
use superb::export::{self, Bounce, Stems};
use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::io::wav::WavFormat;
use superb::primitives::session::TrackKind;
use superb::primitives::{Result, SampleRate, TimeDuration, TimePosition};
use superb::{clip, session, track};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("bounce"), rate, 256)?;
    let lead = track::add(&mut session, "lead", TrackKind::Audio)?;
    let mut tone = PlanarBuffer::new(2, rate.hz() as usize);
    tone.fill(0.25);
    session.audio.insert("audio/tone.wav", tone);
    clip::add(&mut session, lead, "audio/tone.wav", TimePosition::ZERO)?;
    let layout = session::layout(&session);

    // The whole session, 16-bit, at the session's rate, into bounces/mix.wav.
    let audio = export::bounce(&mut session, layout.bounce("mix"), Bounce::default())?;
    println!("{} frames, peak {:.3}", audio.frames(), audio.peak());

    // A range, as 32-bit float, resampled to CD rate (feature `files`).
    export::bounce(&mut session, layout.bounce("chorus"), Bounce {
        start: TimePosition::from_seconds(0.25, rate),
        length: Some(TimeDuration::from_seconds(0.5, rate)),
        sample_rate: Some(SampleRate::CD),
        format: WavFormat::Float32,
    })?;

    // Every strip and the mix into stems/final/, with manifest.json.
    let set = export::stems(&mut session, "final", Stems::default())?;
    for stem in set.stems.iter().chain(std::iter::once(&set.mix)) {
        println!("{} → {}: {:?} LUFS", stem.file,
            stem.output.as_deref().unwrap_or("master"), stem.loudness_lufs);
    }
    Ok(())
}
```

`cargo run --example export_session` and `cargo run --example bounce_stems` run these.

## Verbs

| verb | does |
|---|---|
| `bounce(session, path, Bounce) -> PlanarBuffer` | render offline and write WAV; returns the audio |
| `stems(session, name, Stems) -> StemSet` | one file per unmuted strip plus the mix, into `stems/<name>/` |

| `Bounce` field | default | does |
|---|---|---|
| `start` | zero | where to begin |
| `length` | to the end of the last clip | how much |
| `sample_rate` | the session's | convert on the way out (feature `files`) |
| `format` | `WavFormat::Pcm16` | or `Pcm24`, `Float32` |

`Stems` has `start`, `length` and `format` (24-bit by default); stems stay
at the session's rate. `StemSet` and `manifest.json` list each file's
source, output bus, peak, true peak and loudness.

Every signature, argument, error and undo step: [export reference](export.ref.md).

## Notes

* A bounce runs its own copy of the mixer; the live engine is untouched.
* Live MIDI input, monitoring instruments and the click are never in a
  bounce. MIDI clips on the timeline are.
* Nothing to bounce (no clips after `start`, no `length`) is an error, not
  an empty file.
* A stem is a strip's output before the master. Muted tracks get no stem;
  solo is ignored. Stems are never normalised, so they sum to the mix.
* A stem set's name is used once; a second `stems("final")` is refused.
