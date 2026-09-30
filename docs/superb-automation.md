# superb-automation

Parameter envelopes and DAW-style automation modes.

## Purpose

`superb-automation` is the first Phase 2 crate. It sits beside `superb-mixer`
in the dependency graph (depends only on `superb-core` and `superb-rt`) and
drives the same `Arc<AtomicF32>` handles that strip/node parameters already
expose — no new parameter system required.

## Key types

* `Envelope` / `EnvelopePoint` / `CurveShape::{Hold, Linear, Curve}` —
  sorted breakpoints; sample-accurate `evaluate` / `fill_block`.
* `AutomationMode::{Off, Read, Touch, Latch, Write}` plus
  `AutomationState` — touch/latch state machine.
* `AutomationLane` — one param: envelope + mode + bound `Arc<AtomicF32>`.
  Call `apply_block(block_start, frames, playing, scratch)` once per audio
  block.
* `AutomationBank` — several lanes applied together.
* `ParamTarget` — implemented for `AtomicF32` and `Arc<AtomicF32>`.

## Usage

```rust
use std::sync::Arc;
use superb_automation::{AutomationLane, AutomationMode, EnvelopePoint};
use superb_core::{IdGenerator, TimePosition};
use superb_rt::AtomicF32;

let ids = IdGenerator::new();
let gain = Arc::new(AtomicF32::new(1.0));
let mut lane = AutomationLane::new(ids.next(), Arc::clone(&gain));
lane.envelope_mut()
    .upsert(EnvelopePoint::linear(TimePosition::ZERO, 0.0));
lane.envelope_mut()
    .upsert(EnvelopePoint::linear(TimePosition::from_samples(100), 1.0));
lane.set_mode(AutomationMode::Read);

lane.apply_block(TimePosition::from_samples(50), 16, true, None);
assert!((gain.get() - 0.5).abs() < 1e-5);
```

Write / touch / latch:

```rust
# use std::sync::Arc;
# use superb_automation::{AutomationLane, AutomationMode};
# use superb_core::{IdGenerator, TimePosition};
# use superb_rt::AtomicF32;
# let ids = IdGenerator::new();
# let gain = Arc::new(AtomicF32::new(1.0));
# let mut lane = AutomationLane::new(ids.next(), Arc::clone(&gain));
lane.set_mode(AutomationMode::Touch);
lane.begin_touch();          // UI grabbed the fader
gain.set(0.25);              // UI writes the live value
lane.apply_block(TimePosition::from_samples(0), 64, true, None);
lane.end_touch();            // back to reading the envelope
```

## Real-time notes

* Envelope **edits** (`upsert`, `clear`, `record` inserts) allocate — keep
  them off the audio thread, or throttle write-mode with
  `set_write_interval_samples` and accept that recording happens on a
  non-RT apply path / control thread.
* The **read** path of `apply_block` only evaluates math and stores into
  atomics / a caller-provided scratch slice — no heap traffic.
* Nodes that read their param once per block see the value at
  `block_start`. For true per-sample automation, pass a scratch buffer and
  have the node consume it.

## Modes (while playing)

| Mode | Envelope → param | Param → envelope |
|---|---|---|
| Off | no | no |
| Read | yes | no |
| Touch | yes until touched; write while held | while touched |
| Latch | yes until first touch; then write until stop | after first touch |
| Write | no | continuously |

When not playing, every mode is a no-op for both directions.

See `examples/automation_gain_ramp` for an offline ramp printed to stdout.
