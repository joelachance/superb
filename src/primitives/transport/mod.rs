//! # primitives::transport
//!
//! The playhead and musical clock of the Aura DAW SDK.
//!
//! * [`TempoMap`] — piecewise-constant tempo changes plus time-signature
//!   changes, with exact conversions between samples, seconds, beats and
//!   bars/beats/ticks ([`Bbt`]).
//! * [`Transport`] — stopped / playing / recording state, sample-accurate
//!   position, optional loop range.
//! * [`Transport::advance`] — the audio-thread entry point. It returns an
//!   allocation-free iterator of [`Segment`]s that splits a process block at
//!   loop boundaries so every node sees a contiguous timeline slice.
//!
//! ```
//! use aura::primitives::{SampleRate, TimePosition};
//! use aura::primitives::transport::{LoopRange, Transport};
//!
//! let mut t = Transport::new(SampleRate::HZ_48K, 120.0);
//! t.set_loop(Some(LoopRange::new(
//!     TimePosition::from_samples(0),
//!     TimePosition::from_samples(1_000),
//! ).unwrap()));
//! t.set_loop_enabled(true);
//! t.seek(TimePosition::from_samples(900));
//! t.play();
//!
//! // A 256-frame block starting at 900 wraps: 100 frames to the loop end,
//! // then 156 frames from the loop start.
//! let segs: Vec<_> = t.advance(256).collect();
//! assert_eq!(segs.len(), 2);
//! assert_eq!((segs[0].start.samples(), segs[0].frames), (900, 100));
//! assert_eq!((segs[1].start.samples(), segs[1].frames), (0, 156));
//! assert_eq!(t.position().samples(), 156);
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod playhead;
mod tempo_map;

pub use playhead::{LoopRange, Segment, Segments, Transport, TransportSnapshot, TransportState};
pub use tempo_map::{Bbt, MeterPoint, TempoMap, TempoPoint, TICKS_PER_QUARTER};
