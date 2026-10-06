# primitives::io

Where audio leaves and enters the Superb DAW SDK.

## Purpose

`primitives::io` is the last stop in the engine: it depends on every
other engine module and is generic over any
`superb::primitives::graph::BlockProcessor` — an `AudioGraph` or a `superb::primitives::mixer::Mixer`
both qualify. It owns the block loop that turns a prepared processor plus a
`Transport` into rendered audio: driving the transport, splitting each
block at loop points, delivering queued MIDI, running the processor per
segment, and recording DSP load. This is the layer that drives offline
bounces, WAV files, and (behind the `devices` feature) real cpal output
streams and (also behind `devices`) input/capture streams. Behind the
`files` feature it also owns the file layer: symphonia decoding, rubato
resampling and bounce/export helpers.

## Key types

* `Engine<P: BlockProcessor>` — owns a processor and a `Transport`.
  `new`/`prepare`/`render` (the allocation-free block loop),
  `transport_mut`, `processor_mut`, `midi_mut` (this block's `MidiEventBuffer`),
  `set_midi_input` (attaches an SPSC `Consumer<TimestampedMessage>`),
  `set_midi_timing`/`midi_timing`, `set_midi_block_start_us`,
  `frames_rendered`/`stream_clock_us` (MIDI placement, below),
  `load` (shared `LoadMeter`), `stats`, `into_parts`.
* `MidiTiming` — `Stream` (default: timestamps → frames via
  `FrameMapper`) or `BlockStart` (everything at frame 0).
* `EngineStats` — `blocks`, `segments`, `midi_dropped` counters from `render`.
* `Engine::live` → `(EngineHandle, AudioThread)`, with `EngineCommand` — a
  running engine split in two: `EngineHandle` is the control side
  (transport commands over a queue, `publish` of prepared graphs, software
  MIDI, position and state atomics, `reclaim` of retired graphs) and
  `AudioThread` is the audio side (`tick` renders one block; with
  `devices`, `run_on_device` moves it into an output callback). This is
  what the `transport` flow drives.
* `LiveInput` — captured audio on its way into a running engine:
  `Engine::set_live_input` (or `EngineCommand::LiveInput(Some(..))` on a
  live engine, `None` to detach; a replaced or detached input comes back
  through `reclaim`). Before each block
  the engine pulls the queued blocks into a ring four blocks deep and hands
  the frames to every node as `ProcessContext::input`; `LiveInputNode`
  plays chosen channels of it on a strip. Overruns drop the oldest frames,
  underruns are silence, both counted.
* `capture::{capture_queue, CaptureProducer, CaptureConsumer, CaptureBlock}`
  — a recycle queue of pre-allocated planar blocks for recording; always
  compiled, fed by `device::run_capture` or any producer.
* `offline::render` / `offline::render_prepared` — drive an `Engine` as fast
  as possible into a returned `PlanarBuffer` (bounce / test rendering).
* `wav::{write, write_to, read, read_from, WavFormat}` — minimal RIFF/WAVE
  I/O; `WavFormat::{Pcm16, Float32}`.
* `device` (feature `devices`) — `output_devices` / `input_devices`,
  `OutputConfig` / `InputConfig`, `OutputStream` (`open`, `open_timed`) /
  `InputStream` (`open`, `open_timed`, `open_to_queue`), `run_engine` /
  `run_capture`, `capture_queue` (recycle SPSC of planar `CaptureBlock`s),
  built on `cpal` 0.18 (Apache-2.0).
* `files` (feature `files`) — `AudioFile { buffer, sample_rate }`,
  `decode`/`decode_from`/`decode_bytes`, `decode_at_rate`, `resample`,
  `resampled_frames`, `bounce`, `bounce_wav`, `export_wav`. See "File I/O"
  below. `superb::primitives::io::FILES_FEATURE` reports whether the build has it.

## Usage

```rust
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::SineNode;
use superb::primitives::graph::AudioGraph;
use superb::primitives::io::{offline, Engine};
use superb::primitives::transport::Transport;

let mut graph = AudioGraph::new();
let sine = graph.add_node(Box::new(SineNode::new(440.0, 0.25, 2)));
graph.set_output(sine).unwrap();

let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
engine.transport_mut().play();
let audio = offline::render(&mut engine, SampleRate::HZ_48K, 256, 4_800).unwrap();
assert_eq!(audio.channels(), 2);
assert_eq!(audio.frames(), 4_800);
assert!((audio.peak() - 0.25).abs() < 1e-3);
```

`examples/sine_through_graph` (`cargo run --example sine_through_graph`) builds on
this: a `Mixer` with one track feeds `offline::render`, and the result is
written with `wav::write`. `examples/play_sine_device`
(`cargo run --example play_sine_device --features devices`) instead hands the
engine to `device::run_engine` and plays it on the default output.

## File I/O (feature `files`)

```sh
cargo test --features files                       # decode / resample / bounce tests and crate wiring
cargo run --example decode_bounce                 # example: bounce → decode → resample → export
```

`files` is **off by default**, so `cargo test` (no extra features) never compiles the codec
stack. `examples/decode_bounce` enables `superb/files`, and Cargo feature
unification therefore turns `files` on for
`cargo run --example decode_bounce --features files`. Plain tests still need
`--features files`. Pure Rust: no hardware, ALSA or ffmpeg at test time.

### Licence boundary (MPL-2.0)

The decoder is [symphonia](https://crates.io/crates/symphonia) 0.6, which
is **MPL-2.0** — a file-level copyleft. Superb keeps it contained:

* `src/primitives/io/files/decode.rs` is the **only** source file in the
  crate that imports symphonia. Its header says so, as do the headers
  of `files/mod.rs`, `resample.rs` and `bounce.rs`.
* Everything `decode.rs` exports is expressed in Superb types
  (`PlanarBuffer`, `SampleRate`, `SuperbError::Message`), so no other file
  names an MPL type.
* The default-feature test `mpl_dependency_is_confined_to_decode_rs`
  scans every `.rs` file under `src/`, `tests/` and `examples/` and fails if
  any other file mentions the crate outside a comment. It runs on every
  `cargo test`.
* symphonia is built with `default-features = false` and only `wav`,
  `flac` and `pcm`, so no patent-encumbered or extra-licence codecs are
  pulled in. Transitive crates (`symphonia-core`, `-format-riff`,
  `-bundle-flac`, `-codec-pcm`, `-metadata`, `-common`) are MPL-2.0 too and
  only reachable from `decode.rs`.

Resampling uses [rubato](https://crates.io/crates/rubato) 4 (MIT OR
Apache-2.0; 5.x needs rustc 1.87, above the crate's 1.86 floor) and
its `audioadapter` buffers (MIT OR Apache-2.0).

### Decoding

```rust
use superb::primitives::SampleRate;
use superb::primitives::io::files::{decode, decode_at_rate, decode_bytes};

let file = decode("take.flac")?;            // container/codec probed from content
println!("{} ch, {} frames @ {}", file.channels(), file.frames(), file.sample_rate);
let at_48k = decode_at_rate("take.flac", SampleRate::HZ_48K)?; // decode + resample if needed
let in_memory = decode_bytes(std::fs::read("take.wav")?, Some("wav"))?;
```

* `decode(path)` opens the file (seekable, so trailing metadata is read),
  `decode_bytes(Vec<u8>, hint)` decodes from memory, `decode_from(reader)`
  consumes any `Read + Send + Rebuild` stream without seeking.
* Formats with the default `files` build: WAV (PCM 8/16/24/32-bit, float)
  and FLAC. More containers/codecs are a matter of adding symphonia
  features in `Cargo.toml` — nothing else changes.
* The first audio track is decoded in full into a planar `f32` buffer in
  file channel order; samples are converted from the codec's native format
  by symphonia (`i16` → `f32` is `/32768`, matching `wav::read`).
* Robustness rules: bytes that are not a recognised container fail with
  `SuperbError::Message`; a stream truncated after at least one decoded
  packet returns the frames decoded so far; a corrupt packet is skipped; a
  channel-count change mid-stream is an error; none of these panic.
* Sample rate comes from the codec parameters (fallback: the first decoded
  buffer) and must satisfy `SampleRate::new`.

### Resampling

```rust
use superb::primitives::SampleRate;
use superb::primitives::io::files::{resample, resampled_frames};

let converted = resample(&file.buffer, file.sample_rate, SampleRate::CD)?;
assert_eq!(converted.frames(), resampled_frames(file.frames(), file.sample_rate, SampleRate::CD));
```

* Band-limited FFT resampler (`rubato::Fft`, fixed 1024-frame input chunks).
  Superb drives the chunk loop itself: it feeds the whole clip, pads the tail
  with silence, and trims the resampler's start-up delay so output sample 0
  aligns with input sample 0 (residual sub-sample phase < 0.1 rad at 1 kHz
  in the tests).
* **Frame count rule:** `ceil(frames * to / from)`, exposed as
  `resampled_frames` so callers can pre-compute clip lengths. Equal rates
  return a copy without touching rubato; zero frames in → zero frames out.
* Allocates and is CPU-heavy: call it from a worker thread, never the
  audio callback. `AudioFile::resampled_to` is the same operation on a
  decoded file.

### Bounce / export

```rust
use superb::primitives::io::files::{bounce, bounce_wav, export_wav};
use superb::primitives::io::wav::WavFormat;

// render 10 s at 48 kHz in 256-frame blocks straight to disk; returns the buffer too
let rendered = bounce_wav(&mut engine, SampleRate::HZ_48K, 256, 480_000, "mix.wav", WavFormat::Float32)?;

// or keep it in memory as an AudioFile and write later (possibly resampled)
let take = bounce(&mut engine, SampleRate::HZ_48K, 256, 480_000)?;
export_wav("mix_cd.wav", &take.resampled_to(SampleRate::CD)?, WavFormat::Pcm16)?;
```

`bounce` is `offline::render` with the sample rate attached; `bounce_wav`
adds a `wav::write`. Both prepare the engine at the given rate/block size;
call `transport_mut().play()` first if the transport should move.
`bounce_wav` returns the rendered `PlanarBuffer` so callers can meter it
without reading the file back. Output is always WAV (PCM16 or float32);
encoding to FLAC or other formats is not provided.

## Sample-accurate external MIDI

`Engine::render` drains the `Consumer<TimestampedMessage>` attached with
`set_midi_input` into the block's `MidiEventBuffer`. Each message's
`timestamp_us` becomes a block-relative frame through a
`superb::primitives::midi::timing::FrameMapper` the engine owns:

```rust
use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::SampleRate;
use superb::primitives::graph::nodes::MonoSynthNode;
use superb::primitives::graph::AudioGraph;
use superb::primitives::io::{Engine, MidiTiming};
use superb::primitives::midi::{MidiChannel, MidiMessage, TimestampedMessage};
use superb::primitives::transport::Transport;

let mut graph = AudioGraph::new();
let synth = graph.add_node(Box::new(MonoSynthNode::new(1)));
graph.set_output(synth).unwrap();
let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
engine.prepare(SampleRate::HZ_48K, 480).unwrap(); // 480 frames = 10 ms
assert_eq!(engine.midi_timing(), MidiTiming::Stream);

let (mut tx, rx) = superb::primitives::realtime::spsc::channel::<TimestampedMessage>(16);
engine.set_midi_input(Some(rx));

// Stamp on the engine's stream clock: block 0 spans 0..10_000 µs.
let ch = MidiChannel::new(0).unwrap();
let at = engine.stream_clock_us() + 5_000; // frame 240
tx.push(TimestampedMessage { timestamp_us: at, message: MidiMessage::NoteOn { channel: ch, note: 69, velocity: 127 } }).unwrap();

let mut out = PlanarBuffer::new(1, 480);
engine.render(&mut out).unwrap();
assert!(out.channel(0)[..240].iter().all(|&s| s == 0.0));
assert!(out.channel(0)[240..].iter().any(|&s| s != 0.0));
assert_eq!(engine.stream_clock_us(), 10_000); // next block's start
```

How the block start is chosen, per `render`:

1. If `set_midi_block_start_us(Some(us))` was called since the last render,
   that value is the block start. It is **one-shot**: consumed by that render
   and cleared by `prepare`. `None` cancels a pending override.
2. Otherwise the **stream clock** is used: `frames_rendered * 1_000_000 /
   sample_rate`, rounded up so that `us_to_frames` inverts it exactly.
   `frames_rendered` is reset by `prepare` and advanced by every `render`
   that reached the processor (also on processor error, since the stream
   moved on). `stream_clock_us()` returns the next block's start so offline
   producers can stamp messages.

Then `FrameMapper::set_block(block_start_us, frames)` runs and each message
is placed at `mapper.frame_for(timestamp_us)` (`MidiTiming::Stream`) or at
`0` (`MidiTiming::BlockStart`). Timestamps before the
block clamp to frame 0 and timestamps after it clamp to the last frame, so
nothing is ever lost or deferred to a later block. After the drain the buffer
is `sort_by_frame`d, so explicit `midi_mut().push(MidiEvent { frame, .. })`
still works alongside the queue. Nothing here allocates.

**cpal wiring (feature `devices`).** `device::build`'s data callback reads
`OutputCallbackInfo::timestamp().callback` (a `StreamInstant`), converts it
to microseconds and passes it to the render closure; `OutputStream::open_timed`
exposes that `(block, block_start_us)` closure publicly and `open` wraps it
for callers that don't care. `run_engine` calls
`engine.set_midi_block_start_us(Some(block_start_us))` before every
`engine.render`. Callbacks larger than `max_block_frames` are rendered in
chunks and each chunk's start is advanced by `chunk_offset * 1e6 / hz`.

**Clock limitation (intentional).** cpal's `StreamInstant` counts from the
stream's own epoch; midir's `TimestampedMessage::timestamp_us` counts from
midir's connection clock. They are **not** the same clock, so feeding
`superb::primitives::midi::input::MidiInputBridge` straight into an engine driven by
`run_engine` yields clamped placement (frame 0 or last frame), not
sample-accurate hardware MIDI. Sample-accurate placement is exact whenever
producer and engine share a clock — offline/`stream_clock_us()` producers,
sequencers, tests, or a bridge that re-stamps midir timestamps onto the cpal
clock (measure the offset once and add it). If you want the old
deterministic behaviour for hardware input, set
`engine.set_midi_timing(MidiTiming::BlockStart)`.

## Real-time notes

`Engine::render` is allocation-free: it reuses the processor's own output
buffer and copies into `out` with `PlanarBuffer::copy_block_from`, and MIDI
is drained from the SPSC consumer into the pre-allocated `MidiEventBuffer`
rather than collected into a new container. `Engine::prepare` (which calls
`processor.prepare`) and `Engine::new` are the off-thread setup path and may
allocate. On the `devices` feature, output and input cpal data callbacks call
`superb::primitives::realtime::thread::enter_rt()` once on first invocation to mark the audio
thread real-time. Output then calls `Engine::render`; input de-interleaves
into a scratch `PlanarBuffer` (and optionally `CaptureProducer::push`).
Sample format conversion uses cpal's `FromSample` / `Sample::to_sample`,
also allocation-free. MIDI from `set_midi_input`'s `Consumer` is drained at
the start of `render` and placed inside that block by the engine's
`FrameMapper` (see "Sample-accurate external MIDI" above); the mapper is a
`Copy` struct with no heap data. Hardware-dependent tests
(`plays_a_sine_on_the_default_device`, `captures_from_the_default_device`)
are `#[ignore]` since CI has no audio device.


## Capture / input streams (feature `devices`)

```sh
cargo test --features devices   # unit + ignored hardware smoke
```

Mirror of the output API for microphones / line inputs:

```rust
use superb::primitives::buffers::PlanarBuffer;
use superb::primitives::io::device::{capture_queue, input_devices, run_capture, InputConfig, InputStream};

// List inputs (empty or Err without hardware — never panics).
let _ = input_devices();

// Callback style: planar scratch reused every callback (no alloc).
let config = InputConfig { channels: 2, max_block_frames: 512, ..InputConfig::default() };
// let stream = InputStream::open_timed(&config, |block, capture_us| { /* meter / record */ })?;

// Engine-friendly path: pre-allocated recycle SPSC of planar blocks.
let (mut tx, mut rx) = capture_queue(2, 512, 8)?;
// let stream = InputStream::open_to_queue(&config, tx)?;
// …or shorthand:
// let (stream, mut rx) = run_capture(&config, 8)?;
```

* Device samples arrive interleaved from cpal; the callback de-interleaves
  into a pre-allocated `PlanarBuffer` (same scratch pattern as output, in
  reverse) and never allocates beyond what `OutputStream` already does.
* `open_timed` stamps each chunk with `InputCallbackInfo::timestamp().capture`
  (ADC instant) in microseconds on cpal's stream clock; callbacks larger
  than `max_block_frames` are split and the timestamp is advanced per chunk.
* `capture_queue(channels, max_block_frames, capacity)` pre-allocates
  `CaptureBlock`s on a free/filled SPSC pair. The producer (`push`) copies
  into a free slot; overruns increment `dropped` and discard the device
  block. The consumer `pop`s filled blocks and `recycle`s them. Dropping a
  `CaptureBlock` on the audio thread never happens under normal protocol.
* Hardware smoke test `captures_from_the_default_device` is `#[ignore]`.

## Limitations / next

* **Writing is WAV only** (PCM16 and float32, plain 44-byte header). The
  `wav` module's own *reader* is equally minimal; for 24-bit, extensible
  headers, FLAC and friends use `files::decode` (feature `files`).
* **Decoding covers WAV + FLAC** in the default `files` build; MP3/AAC/
  Vorbis/OGG/MP4 need extra symphonia features (and a licence review for
  the patent-encumbered ones). Only the first audio track is read; no
  seeking, streaming or partial reads — the whole file is decoded into
  memory. No metadata (tags, cue points, loop markers) is surfaced yet.
* **No encoders** other than WAV; no FLAC/OGG export.
* **Resampling is offline only**: `resample` allocates and works on whole
  clips. A real-time (`process_into_buffer`-based, pre-allocated) variant
  for varispeed / device-clock drift is not implemented.
* **Capture is planar-only at the API boundary** — interleaved delivery is
  not exposed (cpal's interleaved buffer is converted in the callback).
  Duplex (shared input+output stream) and device hot-plug are not wrapped.
* **Hardware MIDI timing across midir + cpal is still block-coarse** because
  the two libraries use different clocks (see above). Placement is exact for
  producers on the engine's stream clock or the cpal callback clock.
* `device` requires the `devices` feature (and ALSA headers on Linux) and
  is entirely absent from the default build.
* `OutputStream::pause` depends on backend support in cpal and is not
  guaranteed to work on every host.
