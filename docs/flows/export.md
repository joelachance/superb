# export

Getting the session out: bounce it, or a range of it, to a WAV file; or
bounce every strip to its own file, a stem set.

## Purpose

A bounce builds its own copy of the mixer and runs it offline as fast as
the machine allows. The running engine is untouched, so you can bounce
while listening. A stem set does the same once, keeping each strip's
output in its own file beside the mix, so a mix can travel to another
DAW, a mastering engineer or an editor as parts that line up.

## Verbs

| verb | does |
|---|---|
| `bounce(session, path, Bounce) -> PlanarBuffer` | render and write WAV, making the folder if needed (`layout.bounce(name)` is `bounces/<name>.wav`); returns the audio written |
| `stems(session, name, Stems) -> StemSet` | every unmuted track and bus to its own file and the mix beside them, in one pass, into `stems/<name>/` with a `manifest.json`; returns what the manifest says |

`Bounce` says what and how; `Bounce::default()` is the whole session:

| field | default | does |
|---|---|---|
| `start: TimePosition` | zero | where to begin |
| `length: Option<TimeDuration>` | `None`: to the end of the last clip | how much; a given length may run past the clips |
| `sample_rate: Option<SampleRate>` | `None`: the session's rate | convert on the way out (feature `files`; refused without it) |
| `format: WavFormat` | `Pcm16` | or `Pcm24`, `Float32` |

`Stems` is the same without the rate (stems stay at the session's rate),
and 24-bit PCM by default. The `StemSet` that comes back, and the
`manifest.json` beside the files, say the rate, bit depth, start and
length every file shares and, per file, what it is (`audio`, `midi`,
`bus`, `mix`), where its strip goes (a bus's name, or none for the
master), and its peak, true peak and loudness.

## Reference

Every function of the `export` flow: what it takes, what it returns, what it refuses, and how it undoes. A `Result<T>` is `Ok(T)` or a `AuraError`; the errors listed are the ones you can cause.

### `bounce`

```rust
pub fn bounce(session: &mut Session, path: impl AsRef<Path>, what: Bounce) -> Result<PlanarBuffer>
```

Renders part or all of the session offline and writes it as a WAV file at
`path`, making its folder if needed
(`session::layout(&session).bounce("mix")` is `bounces/mix.wav`). Returns
the audio written. The metronome is left out. Fails when there is nothing to
render: no clips after `start` and no `length` given.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `path: impl AsRef<Path>` — the WAV file to write
- `what: Bounce` — a `Bounce`: where to start, how long (or to the end), which sample rate (or the session's), 16-bit PCM or 32-bit float

**Returns** `Result<PlanarBuffer>` — the rendered audio, also written to `path`

**Errors** nothing to bounce (no clips after `start` and no length given); the file cannot be written; resampling fails.

**Undo** none: not an edit.

**Made of** `Memory::end_position` for the length when none is given → `session::steps::build_mixer`, the click off → a `Transport` on the session's tempo map, sought to `start` and playing → `Engine::new` → `offline::render` → `files::resample` when a rate is given → `create_dir_all` → `wav::write`.

**Example**

```rust
let audio = export::bounce(&mut session, "mix.wav", export::Bounce::default())?;                 // the whole session, 16-bit, session rate
let range = export::bounce(&mut session, "verse.wav", export::Bounce { start: TimePosition::from_seconds(8.0, rate), length: Some(TimeDuration::from_seconds(16.0, rate)), sample_rate: Some(SampleRate::CD), format: WavFormat::Float32 })?;
```

### `stems`

```rust
pub fn stems(session: &mut Session, name: &str, what: Stems) -> Result<StemSet>
```

Bounces every strip of the session to its own file and the mix beside them,
in one offline pass, into `stems/<name>/` of the session folder, with a
`manifest.json` that describes them (`StemSet`). Every file starts at the
same timeline sample and runs the same length, so they line up in any DAW.
Each holds its strip's output after its inserts, fader and pan and before
the master, so the stems of the tracks that feed the master and of the buses
sum to the mix before the master (a track routed into a bus is in that bus's
stem as well as its own). A muted track or bus gets no stem; solo is a
listening aid and is ignored; the metronome is left out. Nothing is
normalised: the stems must still sum to the mix. A name already used is
refused, so a set is never overwritten; a failed bounce removes the folder
it made.

**Takes**
- `session: &mut Session` — the open session, borrowed mutably
- `name: &str` — the set's name and folder under `stems/`; characters a file name cannot hold become `-`
- `what: Stems` — a `Stems`: where to start, how long (or to the end of the last clip), 24-bit PCM by default

**Returns** `Result<StemSet>` — the `StemSet`: name, rate, bit depth, start, frames, and per file its source, output, peak, true peak and loudness; the same as `manifest.json` beside the files

**Errors** nothing to bounce (no clips after `start` and no length given); the name is already used; a file cannot be written.

**Undo** none: not an edit.

**Made of** `Memory::end_position` for the length when none is given → `session::steps::build_mixer` → `Mixer::clear_solo`, the click off → a `Transport` sought to `start` and playing → `Engine::new` → per block `Engine::render`, then `AudioGraph::node_output` of every unmuted strip into a `wav::Writer` and the engine's output into the mix's, each measured by `analysis::meter` → `wav::Writer::finish` → `manifest.json`.

**Example**

```rust
let set = export::stems(&mut session, "final", Stems::default())?;   // stems/final/: one file per strip, mix.wav, manifest.json
for stem in &set.stems { println!("{} {:?} LUFS", stem.file, stem.loudness_lufs); }
```

## Types

### `Bounce`

What to bounce and how. `Bounce::default()` is the whole session, from zero
to the end of the last clip, at the session's sample rate as 16-bit PCM.

**Fields**
- `start: TimePosition` — Where to start. Zero by default.
- `length: Option<TimeDuration>` — How much to render. `None` (the default) runs to the end of the last clip.
- `sample_rate: Option<SampleRate>` — Convert to this sample rate before writing. `None` (the default) keeps the session's rate. Converting needs the `files` feature; without it any other rate is an error.
- `format: WavFormat` — 16-bit PCM by default.

### `Stems`

What a stem set holds and how. `Stems::default()` is the whole session, from
zero to the end of the last clip, as 24-bit PCM at the session's sample
rate.

**Fields**
- `start: TimePosition` — Where every file starts on the timeline. Zero by default.
- `length: Option<TimeDuration>` — How much to render. `None` (the default) runs to the end of the last clip.
- `format: WavFormat` — 24-bit PCM by default.

### `StemSet`

A stem set as `stems` wrote it: what `manifest.json` beside the files says.
Everything a receiving studio, an uploader or a later import needs to know
about the files without opening them.

**Fields**
- `format_version: u32` — The manifest's format, for readers to check.
- `name: String` — The set's name, which is also its folder: `stems/<name>/`.
- `sample_rate: u32` — Sample rate of every file, in Hz.
- `bits: u32` — Bits per sample of every file: 16 or 24 (PCM) or 32 (float).
- `start: i64` — The timeline sample every file's first frame is at.
- `frames: u64` — Frames in every file.
- `stems: Vec<Stem>` — One per strip: every track and bus that is not muted, in the session's order.
- `mix: Stem` — The mix through the master, as `bounce` renders it.

### `Stem`

One file of a stem set.

**Fields**
- `file: String` — The file, inside the set's folder (`vox.wav`).
- `name: String` — The track or bus it is the output of; `mix` for the mix.
- `source: StemSource` — What it is.
- `output: Option<String>` — Where the strip's output goes: the bus's name, or `None` for the master (and for the mix itself).
- `channels: usize` — Channels in the file.
- `peak_dbfs: Option<f32>` — The highest sample, in dBFS; `None` when the file is silent.
- `true_peak_dbtp: Option<f32>` — The highest point between the samples too, in dBTP; `None` when silent.
- `loudness_lufs: Option<f32>` — Integrated loudness, in LUFS; `None` when too quiet or short to measure.

### `StemSource`

What a stem is the output of.

**Variants**
- `Audio` — An audio track.
- `Midi` — A MIDI track, through its instrument.
- `Bus` — A bus: what was routed and sent to it, after its own strip.
- `Mix` — The mix through the master.

## Usage

```rust
use aura::export::{self, Bounce};
use aura::primitives::io::wav::WavFormat;

let audio = export::bounce(&mut session, "session.wav", Bounce::default())?;
println!("{} frames, peak {:.2}", audio.frames(), audio.peak());

export::bounce(&mut session, "chorus.wav", Bounce {
    start: TimePosition::from_seconds(30.0, rate),
    length: Some(TimeDuration::from_seconds(15.0, rate)),
    format: WavFormat::Float32,
    ..Bounce::default()
})?;

// Every strip and the mix, into stems/final/.
let set = export::stems(&mut session, "final", export::Stems::default())?;
for stem in &set.stems {
    println!("{} to {}: {:?} LUFS", stem.file, stem.output.as_deref().unwrap_or("master"), stem.loudness_lufs);
}
```

## Made of

`Memory::end_position` for the length when none is given →
`session::steps::build_mixer` → `Transport::with_tempo_map(session tempo
map)`, `seek`, `play` → `Engine::new` → `io::offline::render` →
`io::files::resample` when a rate is given → `wav::write`. `stems`: the
same mixer with solo cleared and the click off → per block
`Engine::render`, then `AudioGraph::node_output` of every unmuted strip
into a `wav::Writer` and the engine's output into the mix's, each
measured by `analysis::meter` → `manifest.json`.

## Notes

* A bounce with no clips after `start` and no `length` is an error rather
  than an empty file.
* Live MIDI input and monitoring instruments are not part of a bounce;
  MIDI clips on the timeline are.
* WAV is the only encoder; formats are 16-bit and 24-bit PCM and 32-bit
  float.
* The metronome is never in a bounce or a stem set.
* A stem is a strip's output after its inserts, fader and pan, before the
  master. The stems of tracks that feed the master and of the buses sum
  to the mix before the master; a track routed into a bus is in that
  bus's stem as well as its own, so summing every file counts it twice.
  The manifest's `output` says which is which.
* Strips are stereo, so every stem is two channels, a mono recording
  panned like any other.
* Muted tracks get no stem. Solo is a listening aid and is ignored, so a
  stem set is the same whatever was soloed when it was made.
* Stems are never normalised, or they would stop summing to the mix;
  `analysis::meter::normalize` on the mix is the place for a loudness
  target.
* A stem set's name is used once: a second `stems` with the same name is
  refused rather than writing over the first. A bounce that fails removes
  the folder it made; it never touches one it did not make.
* Each file skips the latency of the path to its strip, so stems line up
  with each other and with the mix even when an insert adds delay.
