# superb-core

Foundation types shared by every Superb SDK crate. No dependencies, no I/O.

## Purpose

`superb-core` is the bottom of the dependency chain (core → buffers/rt →
transport → graph): every other crate in the workspace depends on it, and it
depends on nothing in the workspace. Everything here is `Copy`-friendly
(except the rarely-used `SuperbError::Message(String)` variant) and safe to
touch on the audio thread — it defines the vocabulary (time, sample rates,
IDs, errors) that the rest of the SDK builds on, but does no scheduling,
buffering or I/O itself.

## Key types

* `SampleRate` — validated integer Hz (`1_000..=1_536_000`), with `f64` and
  seconds/samples conversion helpers.
* `TimePosition` / `TimeDuration` — signed, integer sample-accurate audio-domain
  position/duration (signed so pre-roll is representable).
* `BeatPosition` / `BeatDuration` — musical position/duration in quarter notes
  (`f64`); independent of sample rate.
* `TimeSignature` — validated `numerator/denominator` meter with `bar_length`
  and `beat_length` helpers.
* `IdGenerator` / `TrackId`, `BusId`, `SendId`, `NodeId`, `PortId`, `ClipId`,
  `ParamId` — monotonic `u64` newtype IDs, `0` reserved as "invalid".
* `SuperbError` / `Result<T>` — the shared error enum used across the SDK.

## Usage

```rust
use superb_core::{SampleRate, TimePosition, BeatPosition};

let rate = SampleRate::new(48_000).unwrap();
let half_second = TimePosition::from_seconds(0.5, rate);
assert_eq!(half_second.samples(), 24_000);
assert_eq!(half_second.to_seconds(rate), 0.5);

// Beats are independent of sample rate; the tempo map in
// `superb-transport` converts between the two domains.
let two_bars_of_4_4 = BeatPosition::new(8.0);
assert_eq!(two_bars_of_4_4.beats(), 8.0);
```

```rust
use superb_core::{IdGenerator, NodeId};

let gen = IdGenerator::new();
let a: NodeId = gen.next();
let b: NodeId = gen.next();
assert!(a.is_valid());
assert!(b > a);
```

## Real-time notes

Every type in this crate is stack-only (`Vec`/`String`-free except
`SuperbError::Message`, which is only ever constructed off the audio thread)
so arithmetic on `TimePosition`, `BeatPosition`, `SampleRate` etc. never
allocates. `IdGenerator::next_raw` is a single lock-free `AtomicU64::fetch_add`
(`crates/superb-core/src/ids.rs`, proven single-threaded and cross-thread by
`generator_is_monotonic_and_never_yields_invalid` and
`generator_is_unique_across_threads`), so IDs may be minted from any thread,
including the audio thread. There is no allocator dependency to prove
no-alloc against here — that guarantee is established downstream in
`superb-graph`'s `no_alloc.rs` integration test, which exercises these types
as part of a real process call.

## Limitations / next

* `TimePosition`/`TimeDuration` are `i64` samples — no explicit overflow
  checking beyond what `i64` arithmetic gives you.
* `BeatPosition`/`BeatDuration` are `f64`; no rational/tick-exact
  representation.
* Converting between the audio and musical domains requires a tempo map,
  which lives in `superb-transport`, not here.
* `IdGenerator` never reuses IDs and has no persistence/serialization story
  yet — that belongs to `superb-session` in Phase 2.
