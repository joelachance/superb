# export

Getting the session out: render a range or the whole session to a file.

## Purpose

Export builds its own copy of the mixer, inserts included, and runs it
offline as fast as the machine allows. The running engine is untouched, so
you can bounce while listening.

## Verbs

| verb | does |
|---|---|
| `render(session, start, frames) -> PlanarBuffer` | into memory |
| `bounce(session, path, start, frames, WavFormat) -> PlanarBuffer` | render and write WAV at the session's rate |
| `bounce_session(session, path, WavFormat)` | from zero to the end of the last clip; fails on an empty session |
| `bounce_at_rate(session, path, start, frames, rate, WavFormat)` (feature `files`) | render, resample, write |

## Usage

```rust
use superb::export;
use superb::primitives::io::wav::WavFormat;

let audio = export::bounce_session(&mut session, "session.wav", WavFormat::Pcm16)?;
println!("{} frames, peak {:.2}", audio.frames(), audio.peak());
```

## Made of

`render`: `session::steps::build_mixer` → `session::steps::attach_inserts`
(fails if an insert cannot load) → `Transport::with_tempo_map(session
tempo map)`, `seek`, `play` → `Engine::new` → `io::offline::render`.
`bounce`: `render` → `wav::write`. `bounce_at_rate`: `render` →
`io::files::resample` → `wav::write`.

## Notes

* Plugin instances for the export are fresh, so anything that depends on
  a tail from before `start` begins cold.
* Live MIDI input and monitoring instruments are not part of an export;
  MIDI clips on the timeline are.
* WAV is the only encoder; formats are 16-bit PCM and 32-bit float.
