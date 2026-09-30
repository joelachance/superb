# superb-transport

The playhead and musical clock of the Superb DAW SDK.

## Purpose

`superb-transport` sits above `superb-buffers`/`superb-rt` in the dependency
chain (core → buffers/rt → **transport** → graph) and depends on
`superb-core`. It owns the mapping between audio time (samples) and musical
time (beats/bars), and the playhead state machine that walks a process block
across that timeline, including loop wraps. `superb-graph`'s `BlockTime`
(`crates/superb-graph/src/context.rs`) is built from a `Transport` and one of
its `Segment`s, so every node downstream sees a consistent, sample-accurate
position and tempo.

## Key types

* `TempoMap` — piecewise-constant tempo with optional linear ramps between
  points, plus meter changes; converts beats <-> seconds <-> samples and
  beats <-> `Bbt` (bars/beats/ticks). Editing: `add_tempo` (constant),
  `add_tempo_ramp` (ramp to the next point), `set_tempo_ramp`,
  `remove_tempo`, `add_meter`, `remove_meter`.
* `TempoPoint` / `MeterPoint` — a tempo (bpm from a beat, `ramp` flag) or
  meter (time signature from a bar) change point.
* `Bbt` — 1-based bar/beat, 0-based tick (`TICKS_PER_QUARTER` = 960 ticks per
  quarter note) position for display and grid snapping.
* `Transport` — stopped/playing/recording state, sample-accurate position,
  optional `LoopRange`; owns the `TempoMap`.
* `LoopRange` — a half-open `[start, end)` sample range.
* `Segment` / `Segments` — the allocation-free iterator `Transport::advance`
  returns, splitting a block at loop boundaries.

## Usage

```rust
use superb::primitives::{SampleRate, TimePosition};
use superb::primitives::transport::{LoopRange, Transport};

let mut t = Transport::new(SampleRate::HZ_48K, 120.0);
t.set_loop(Some(LoopRange::new(
    TimePosition::from_samples(0),
    TimePosition::from_samples(1_000),
).unwrap()));
t.set_loop_enabled(true);
t.seek(TimePosition::from_samples(900));
t.play();

// A 256-frame block starting at 900 wraps: 100 frames to the loop end,
// then 156 frames from the loop start.
let segs: Vec<_> = t.advance(256).collect();
assert_eq!(segs.len(), 2);
assert_eq!((segs[0].start.samples(), segs[0].frames), (900, 100));
assert_eq!((segs[1].start.samples(), segs[1].frames), (0, 156));
assert_eq!(t.position().samples(), 156);
```

```rust
use superb::primitives::BeatPosition;
use superb::primitives::transport::TempoMap;
use superb::primitives::SampleRate;

let mut map = TempoMap::new(SampleRate::HZ_48K, 120.0);
map.add_tempo(BeatPosition::new(4.0), 60.0).unwrap();
assert_eq!(map.tempo_at(BeatPosition::new(3.999)), 120.0);
assert_eq!(map.tempo_at(BeatPosition::new(4.0)), 60.0);

// Ramp instead of step: tempo falls linearly from 120 at beat 0 to 60 at beat 4.
map.set_tempo_ramp(BeatPosition::ZERO, true).unwrap();
assert_eq!(map.tempo_at(BeatPosition::new(2.0)), 90.0);
let s = map.beat_to_seconds(BeatPosition::new(4.0));   // 4·ln 2 ≈ 2.77 s
assert!(s > 2.0 && s < 4.0);
assert!((map.seconds_to_beat(s).beats() - 4.0).abs() < 1e-9);
```

## Tempo ramps

A `TempoPoint` with `ramp == true` interpolates bpm **linearly in beats** up
to the next point (`bpm(β) = b0 + (b1 − b0)(β − β0)/(β1 − β0)`); the last
point and anything before beat zero are held constant. Conversions use the
closed-form integral, so they are exact and invertible rather than sampled:

* beats → seconds: `60 (β1 − β0)/(b1 − b0) · ln(bpm(β)/b0)`
* seconds → beats: `bpm(β) = b0 · exp(Δs (b1 − b0)/(60 (β1 − β0)))`, then
  solve the line for β.

Equal tempos on both ends degrade to the constant case, so a ramp flag never
divides by zero. `rebuild` caches seconds at each point with the same
integral, keeping `seconds_to_beat`'s binary search valid.
`superb-session` persists the flag (`TempoPointSnapshot::ramp`, defaulting
to `false` for older files) and `commands::SetTempo::ramped` edits it
undoably.

## Real-time notes

`Transport::advance` is the audio-thread entry point and is allocation-free:
`Segments` is a plain iterator over fields already on the stack, not a `Vec`.
`Transport::seek`/`play`/`stop`/`set_loop`/etc. only touch `Copy` fields.
`TempoMap::add_tempo`/`add_meter`/`remove_tempo`/`remove_meter` mutate
`Vec<TempoPoint>`/`Vec<MeterPoint>` (via `insert`/`remove`/`rebuild`) and so
*do* allocate/reallocate — they are edit-time operations meant to run off the
audio thread, mirroring how `AudioGraph`'s editing methods
(`add_node`/`connect`) are separate from its `process`. Per-block reads such
as `tempo_at`, `samples_to_beat`, `beat_to_samples` and `beat_to_bbt` only
binary-search the existing `Vec`s and are safe to call every block.

## Limitations / next

* Tempo ramps are **linear in beats** only — no exponential/S-curves and no
  ramps defined over seconds. `samples_per_beat_at` reports the instantaneous
  value at a beat, not an average over a block.
* No tap-tempo or tempo-detection helpers.
* `TempoMap`/`Transport` are plain single-owner values with no built-in
  cross-thread command queue; a live engine is expected to pair them with
  `superb::primitives::realtime::spsc` for control messages, per the crate doc comment.
* Loop points are sample-accurate but there is no crossfade/click-avoidance
  at the loop boundary — that's a node/mixer concern, not the transport's.
