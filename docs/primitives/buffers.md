# primitives::buffers

Audio block buffers for the Aura DAW SDK.

## Purpose

`primitives::buffers` sits just above the shared types: it depends only on the
`primitives` root and defines the two block layouts every other audio module
exchanges data in: `PlanarBuffer` (native to the process graph — DSP code
operates on plain `f32` slices) and `InterleavedBuffer` (the layout most
device APIs, e.g. cpal, expose), plus conversion and gain/metering helpers
between them. Every buffer is allocated once with a frame *capacity* and can
then be resized *down* without allocating, which is what a variable-size
real-time callback needs.

## Key types

* `PlanarBuffer` — one contiguous `f32` slice per channel; supports
  `set_frames`, gain/ramp, mix (`add_from`/`add_channel_from`), and metering
  (`peak`, `rms`, `channel_peaks`).
* `InterleavedBuffer` — frame-major `[ch0, ch1, ...]` storage used at the
  device boundary.
* `ChannelLayout` — `Mono` / `Stereo` / `Multi(u16)`, normalised via
  `from_count`.
* `interleave` / `deinterleave` — copy between a `PlanarBuffer` and an
  `InterleavedBuffer` of matching shape.
* `interleave_slices` / `deinterleave_slice` — the same conversions over raw
  `&[f32]`, for callback code that doesn't own a `PlanarBuffer` yet.
* `convert` module — `to_f32`/`from_f32` (via `dasp_sample`) and
  `f32_to_i16_saturating` for integer PCM interop.
* `gain` module — `db_to_linear` / `linear_to_db`, floored at `SILENCE_DB`
  (-120 dB) instead of `-inf`.

## Usage

```rust
use aura::primitives::buffers::{PlanarBuffer, InterleavedBuffer, interleave};

let mut block = PlanarBuffer::new(2, 4);
block.channel_mut(0).copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);
block.channel_mut(1).copy_from_slice(&[-1.0, -2.0, -3.0, -4.0]);

let mut out = InterleavedBuffer::new(2, 4);
interleave(&block, &mut out);
assert_eq!(out.samples(), &[1.0, -1.0, 2.0, -2.0, 3.0, -3.0, 4.0, -4.0]);

block.apply_gain(0.5);
assert_eq!(block.channel(0)[3], 2.0);
assert!(!block.is_silent());
block.clear();
assert!(block.is_silent());
```

## Real-time notes

`PlanarBuffer::new`/`InterleavedBuffer::new`/`resize_capacity` allocate (they
call `vec![0.0; ...]`) and must run off the audio thread during setup.
Everything used per block — `set_frames`, `channel`/`channel_mut`, `clear`,
`fill`, `apply_gain`, `apply_gain_ramp`, `copy_from`, `add_from`,
`add_channel_from`, `peak`, `rms` — operates on the pre-allocated `Vec<f32>`
in place and never grows it, so it is real-time safe; `set_frames` only
*shrinks* the active view and returns `AuraError::CapacityExceeded` rather
than reallocating if asked to grow past `capacity`
(`src/primitives/buffers/planar.rs`, `shrinking_frames_keeps_capacity_and_plane_stride`).
This module has no direct no-alloc test of its own; its allocation-free
methods are exercised inside `tests/graph_no_alloc.rs`,
since `AudioGraph::process` calls them every block.

## Limitations / next

* `f32` only — no `f64` or fixed-point buffer variant.
* No SIMD-specialised paths; loops are written for the compiler to
  auto-vectorise.
* No buffer pooling/arena across nodes — `primitives::graph` allocates one input
  and one output `PlanarBuffer` per node in `prepare`.
* `channel_pair_mut` only splits two *distinct* channels; there is no
  general multi-channel split helper.
