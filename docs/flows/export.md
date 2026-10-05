# export

Getting the session out: bounce it, or a range of it, to a WAV file.

## Purpose

A bounce builds its own copy of the mixer and runs it offline as fast as
the machine allows. The running engine is untouched, so you can bounce
while listening.

## Verbs

| verb | does |
|---|---|
| `bounce(session, path, Bounce) -> PlanarBuffer` | render and write WAV; returns the audio written |

`Bounce` says what and how; `Bounce::default()` is the whole session:

| field | default | does |
|---|---|---|
| `start: TimePosition` | zero | where to begin |
| `length: Option<TimeDuration>` | `None`: to the end of the last clip | how much; a given length may run past the clips |
| `sample_rate: Option<SampleRate>` | `None`: the session's rate | convert on the way out (feature `files`; refused without it) |
| `format: WavFormat` | `Pcm16` | or `Float32` |

## Usage

```rust
use superb::export::{self, Bounce};
use superb::primitives::io::wav::WavFormat;

let audio = export::bounce(&mut session, "session.wav", Bounce::default())?;
println!("{} frames, peak {:.2}", audio.frames(), audio.peak());

export::bounce(&mut session, "chorus.wav", Bounce {
    start: TimePosition::from_seconds(30.0, rate),
    length: Some(TimeDuration::from_seconds(15.0, rate)),
    format: WavFormat::Float32,
    ..Bounce::default()
})?;
```

## Made of

`Memory::end_position` for the length when none is given →
`session::steps::build_mixer` → `Transport::with_tempo_map(session tempo
map)`, `seek`, `play` → `Engine::new` → `io::offline::render` →
`io::files::resample` when a rate is given → `wav::write`.

## Notes

* A bounce with no clips after `start` and no `length` is an error rather
  than an empty file.
* Live MIDI input and monitoring instruments are not part of a bounce;
  MIDI clips on the timeline are.
* WAV is the only encoder; formats are 16-bit PCM and 32-bit float.
