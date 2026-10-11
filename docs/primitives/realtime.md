# primitives::realtime

Real-time safety primitives for the Aura DAW SDK.

## Purpose

`primitives::realtime` depends only on the `primitives` root and
provides the cross-thread plumbing and safety-proving tools every other
real-time module in the SDK is built with. The audio thread must never block,
allocate, or take a lock; this module supplies the wait-free queue for moving
data onto/off of that thread, lock-free atomics for parameter sharing,
allocation-counting to *prove* a hot path is heap-free, and a thread marker
plus load meter for diagnosing when it isn't fast enough.

## Key types

* `spsc::{channel, Producer, Consumer}` — a wait-free single-producer /
  single-consumer ring buffer, allocated once at `channel(capacity)`; `push`
  and `pop` never allocate or block.
* `AtomicF32` / `AtomicF64` — lock-free float atomics for parameter sharing
  (UI thread writes, audio thread reads), with `fetch_max` for peak holds.
* `CountingAllocator` / `assert_no_alloc` / `count_allocations` — a
  `#[global_allocator]` wrapper that counts heap ops per thread so tests can
  prove a closure allocates zero times.
* `thread::{enter_rt, is_rt_thread, debug_assert_not_rt}` — marks the current
  thread as real-time and panics (in debug builds) if non-RT-safe code runs
  on it.
* `ProcessBudget` / `Timer` / `ProcessReport` / `LoadMeter` — measures a
  callback's elapsed time against its sample-rate-derived deadline and
  accumulates a lock-free, UI-readable load figure.

## Usage

```rust
use aura::primitives::realtime::spsc;

let (mut tx, mut rx) = spsc::channel::<u32>(4);
tx.push(1).unwrap();
tx.push(2).unwrap();
assert_eq!(rx.pop(), Some(1));
assert_eq!(rx.pop(), Some(2));
assert_eq!(rx.pop(), None);
```

```rust,ignore
#[global_allocator]
static ALLOC: aura::primitives::realtime::CountingAllocator = aura::primitives::realtime::CountingAllocator::system();

aura::primitives::realtime::assert_no_alloc(|| {
    // audio-thread work here; panics if it touches the heap
});
```

## Real-time notes

`spsc::channel` allocates its slot array once at construction (non-RT
setup); after that, `Producer::push` and `Consumer::pop`/`peek`/`drain` are
wait-free and allocation-free, proven by
`src/primitives/realtime/spsc.rs::cross_thread_transfer_is_lossless_and_ordered`
running the queue between two real OS threads. `AtomicF32`/`AtomicF64`
`get`/`set`/`load`/`store`/`swap`/`fetch_max` compile to single lock-free
instructions on all supported targets. `assert_no_alloc` itself needs
`CountingAllocator` installed as the binary's `#[global_allocator]` — without
it, it panics rather than silently passing
(`src/primitives/realtime/alloc.rs::allocation_is_counted_and_absence_is_provable`);
`tests/graph_no_alloc.rs` is the SDK's
canonical consumer of this pattern for a real `process()` call.
`thread::debug_assert_not_rt` compiles to nothing in release builds, so it is
a debug-only guard, not an enforced runtime invariant.

## Limitations / next

* `spsc` is single-producer/single-consumer only — no MPSC/MPMC variant yet.
* No triple-buffer (single-writer, single-reader, wait-free *latest value*)
  primitive yet; `AtomicF32`/`AtomicF64` cover single scalars, not larger
  snapshots.
* No lock-free command queue with typed messages — callers build their own
  command enum on top of `spsc::channel<T>`.
* `ProcessBudget`/`LoadMeter` measure wall-clock time via `std::time::Instant`;
  no CPU-cycle or `RDTSC`-based timing.
