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

## Reference

Every function of the `export` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `SuperbError`; the errors listed are the ones you can cause.

### `bounce`

```rust
pub fn bounce(session: &mut Session, path: impl AsRef<Path>, what: Bounce) -> Result<PlanarBuffer>
```

Renders part or all of the session offline and writes it as a WAV file at
`path`. Returns the audio written. Fails when there is nothing to render: no
clips after `start` and no `length` given.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `path: impl AsRef<Path>` — the WAV file to write
- `what: Bounce` — a `Bounce`: where to start, how long (or to the end), which sample rate (or the session's), 16-bit PCM or 32-bit float

**Returns** `Result<PlanarBuffer>` — the rendered audio, also written to `path`

**Errors** nothing to bounce (no clips after `start` and no length given); the file cannot be written; resampling fails.

**Undo** none: not an edit.

**Made of** `Memory::end_position` for the length when none is given → [`session::steps::build_mixer`] → a `Transport` on the session's tempo map, sought to `start` and playing → `Engine::new` → `offline::render` → `files::resample` when a rate is given → `wav::write`.

**Example**

```rust
let audio = export::bounce(&mut session, "mix.wav", export::Bounce::default())?;                 // the whole session, 16-bit, session rate
let range = export::bounce(&mut session, "verse.wav", export::Bounce { start: TimePosition::from_seconds(8.0, rate), length: Some(TimeDuration::from_seconds(16.0, rate)), sample_rate: Some(SampleRate::CD), format: WavFormat::Float32 })?;
```

## Types

### `Bounce`

What to bounce and how. [`Bounce::default()`] is the whole session, from
zero to the end of the last clip, at the session's sample rate as 16-bit
PCM.

| field | type | meaning |
|---|---|---|
| `start` | `TimePosition` | Where to start. Zero by default. |
| `length` | `Option<TimeDuration>` | How much to render. `None` (the default) runs to the end of the last clip. |
| `sample_rate` | `Option<SampleRate>` | Convert to this sample rate before writing. `None` (the default) keeps the session's rate. Converting needs the `files` feature; without it any other rate is an error. |
| `format` | `WavFormat` | 16-bit PCM by default. |

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
