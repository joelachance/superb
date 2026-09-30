//! # superb-automation
//!
//! Parameter envelopes and DAW-style automation modes for the Superb SDK.
//!
//! * [`Envelope`] / [`EnvelopePoint`] / [`CurveShape`] — breakpoint curves
//!   with hold, linear and tensioned segments; sample-accurate evaluation.
//! * [`AutomationMode`] / [`AutomationState`] — Off / Read / Touch / Latch /
//!   Write state machine.
//! * [`AutomationLane`] / [`AutomationBank`] — bind an envelope to an
//!   `Arc<AtomicF32>` (the same handles mixer strips and graph nodes use)
//!   and apply one block at a time.
//! * [`ParamTarget`] — trait implemented for [`AtomicF32`] and
//!   `Arc<AtomicF32>`.
//!
//! ```
//! use std::sync::Arc;
//! use superb_automation::{AutomationLane, AutomationMode, CurveShape, EnvelopePoint};
//! use superb_core::{IdGenerator, TimePosition};
//! use superb_rt::AtomicF32;
//!
//! let ids = IdGenerator::new();
//! let gain = Arc::new(AtomicF32::new(1.0));
//! let mut lane = AutomationLane::new(ids.next(), Arc::clone(&gain));
//! lane.envelope_mut().upsert(EnvelopePoint::linear(TimePosition::ZERO, 0.0));
//! lane.envelope_mut().upsert(EnvelopePoint {
//!     time: TimePosition::from_samples(100),
//!     value: 1.0,
//!     shape: CurveShape::Linear,
//! });
//! lane.set_mode(AutomationMode::Read);
//!
//! lane.apply_block(TimePosition::from_samples(50), 16, true, None);
//! assert!((gain.get() - 0.5).abs() < 1e-5);
//! ```
//!
//! Real-time notes: envelope **edits** (`upsert`, `clear`, …) allocate and
//! belong off the audio thread. [`AutomationLane::apply_block`] on the read
//! path only evaluates and stores into atomics / a caller-provided scratch
//! buffer — no heap traffic. Write-mode `record` may insert points (control
//! or non-RT apply path); throttle with
//! [`AutomationLane::set_write_interval_samples`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod envelope;
mod lane;
mod mode;
mod target;

pub use envelope::{CurveShape, Envelope, EnvelopePoint};
pub use lane::{advance_block, AutomationBank, AutomationLane, RECORD_EPSILON};
pub use mode::{AutomationMode, AutomationState};
pub use target::ParamTarget;
