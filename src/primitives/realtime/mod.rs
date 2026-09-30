//! # superb-rt
//!
//! Real-time safety primitives for the Superb DAW SDK.
//!
//! The audio thread must never block, allocate, or take a lock. This crate
//! provides the building blocks that make that discipline practical:
//!
//! * [`spsc`] — a wait-free single-producer / single-consumer ring buffer for
//!   moving commands, MIDI events and meter data between threads.
//! * [`alloc`] — a counting global allocator plus [`alloc::assert_no_alloc`],
//!   so tests can prove a `process()` call never touches the heap.
//! * [`atomic`] — [`AtomicF32`] / [`AtomicF64`]
//!   for lock-free parameter sharing.
//! * [`budget`] — measure a callback against its time budget and expose a
//!   lock-free DSP load figure.
//! * [`thread`] — mark the current thread as real-time and assert that
//!   non-RT code is not called from it.
//!
//! ```
//! use superb::primitives::realtime::spsc;
//!
//! let (mut tx, mut rx) = spsc::channel::<u32>(4);
//! tx.push(1).unwrap();
//! tx.push(2).unwrap();
//! assert_eq!(rx.pop(), Some(1));
//! assert_eq!(rx.pop(), Some(2));
//! assert_eq!(rx.pop(), None);
//! ```

#![warn(missing_docs)]

#[allow(unsafe_code)]
pub mod alloc;
pub mod atomic;
pub mod budget;
#[allow(unsafe_code)]
pub mod spsc;
pub mod thread;

pub use alloc::{assert_no_alloc, count_allocations, CountingAllocator};
pub use atomic::{AtomicF32, AtomicF64};
pub use budget::{LoadMeter, ProcessBudget, ProcessReport};
pub use spsc::{channel, Consumer, Producer};
