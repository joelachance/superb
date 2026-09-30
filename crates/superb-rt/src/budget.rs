//! Process-budget timing and DSP load metering.
//!
//! A callback for `frames` samples at `rate` Hz has a hard deadline of
//! `frames / rate` seconds. [`ProcessBudget`] captures that deadline;
//! [`ProcessBudget::start`] returns a timer whose [`finish`](Timer::finish)
//! reports elapsed time, load ratio and whether the deadline was missed.
//! [`LoadMeter`] accumulates those reports into lock-free atomics that a UI
//! thread can read.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use superb_core::SampleRate;

use crate::atomic::AtomicF32;

/// The time available for one process callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessBudget {
    budget: Duration,
}

impl ProcessBudget {
    /// Budget for `frames` samples at `rate`.
    pub fn for_block(frames: usize, rate: SampleRate) -> Self {
        Self {
            budget: Duration::from_secs_f64(frames as f64 / rate.as_f64()),
        }
    }

    /// Budget from an explicit duration.
    pub const fn from_duration(budget: Duration) -> Self {
        Self { budget }
    }

    /// The deadline duration.
    pub const fn duration(&self) -> Duration {
        self.budget
    }

    /// Starts timing a callback.
    #[inline]
    pub fn start(&self) -> Timer {
        Timer {
            budget: self.budget,
            started: Instant::now(),
        }
    }

    /// Builds a report from an already-measured duration (useful in tests).
    pub fn report(&self, elapsed: Duration) -> ProcessReport {
        let load = if self.budget.is_zero() {
            f64::INFINITY
        } else {
            elapsed.as_secs_f64() / self.budget.as_secs_f64()
        };
        ProcessReport {
            elapsed,
            budget: self.budget,
            load,
        }
    }
}

/// A running measurement of one callback.
#[derive(Debug)]
pub struct Timer {
    budget: Duration,
    started: Instant,
}

impl Timer {
    /// Stops the timer and returns the report.
    #[inline]
    pub fn finish(self) -> ProcessReport {
        ProcessBudget {
            budget: self.budget,
        }
        .report(self.started.elapsed())
    }
}

/// Result of timing one callback.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessReport {
    /// Wall-clock time the callback took.
    pub elapsed: Duration,
    /// The deadline it had.
    pub budget: Duration,
    /// `elapsed / budget`; `>= 1.0` means the deadline was missed.
    pub load: f64,
}

impl ProcessReport {
    /// `true` if the callback took longer than its budget.
    pub fn overran(&self) -> bool {
        self.elapsed > self.budget
    }
}

/// Lock-free accumulator of [`ProcessReport`]s.
///
/// The audio thread calls [`LoadMeter::record`]; any other thread can read
/// the smoothed load, the peak load and the overrun count at any time.
#[derive(Debug, Default)]
pub struct LoadMeter {
    smoothed: AtomicF32,
    peak: AtomicF32,
    overruns: AtomicUsize,
    callbacks: AtomicUsize,
    /// Smoothing coefficient in 1/1000 units (e.g. 100 = 10 % new sample per callback).
    smoothing_permille: AtomicU32,
}

impl LoadMeter {
    /// Creates a meter with 10 % smoothing per callback.
    pub const fn new() -> Self {
        Self {
            smoothed: AtomicF32::new(0.0),
            peak: AtomicF32::new(0.0),
            overruns: AtomicUsize::new(0),
            callbacks: AtomicUsize::new(0),
            smoothing_permille: AtomicU32::new(100),
        }
    }

    /// Sets how much of each new report is mixed into the smoothed value (`0.0..=1.0`).
    pub fn set_smoothing(&self, alpha: f32) {
        let permille = (alpha.clamp(0.0, 1.0) * 1000.0) as u32;
        self.smoothing_permille.store(permille, Ordering::Relaxed);
    }

    /// Records one callback report. Audio-thread safe.
    pub fn record(&self, report: &ProcessReport) {
        let load = report.load as f32;
        let alpha = self.smoothing_permille.load(Ordering::Relaxed) as f32 / 1000.0;
        let prev = self.smoothed.get();
        self.smoothed.set(prev + alpha * (load - prev));
        self.peak.fetch_max(load, Ordering::Relaxed);
        self.callbacks.fetch_add(1, Ordering::Relaxed);
        if report.overran() {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Exponentially smoothed load ratio.
    pub fn load(&self) -> f32 {
        self.smoothed.get()
    }

    /// Highest load ratio seen since the last [`LoadMeter::reset_peak`].
    pub fn peak(&self) -> f32 {
        self.peak.get()
    }

    /// Number of callbacks that missed their deadline.
    pub fn overruns(&self) -> usize {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Total callbacks recorded.
    pub fn callbacks(&self) -> usize {
        self.callbacks.load(Ordering::Relaxed)
    }

    /// Resets the peak hold to zero.
    pub fn reset_peak(&self) {
        self.peak.set(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_matches_block_duration() {
        let b = ProcessBudget::for_block(480, SampleRate::HZ_48K);
        assert_eq!(b.duration(), Duration::from_millis(10));
        let b = ProcessBudget::for_block(441, SampleRate::CD);
        assert!((b.duration().as_secs_f64() - 0.01).abs() < 1e-9);
    }

    #[test]
    fn report_flags_overrun_and_load() {
        let b = ProcessBudget::from_duration(Duration::from_millis(10));
        let ok = b.report(Duration::from_millis(5));
        assert!(!ok.overran());
        assert!((ok.load - 0.5).abs() < 1e-9);
        let late = b.report(Duration::from_millis(15));
        assert!(late.overran());
        assert!((late.load - 1.5).abs() < 1e-9);
        let zero = ProcessBudget::from_duration(Duration::ZERO).report(Duration::from_nanos(1));
        assert!(zero.load.is_infinite());
    }

    #[test]
    fn timer_measures_something_positive() {
        let b = ProcessBudget::from_duration(Duration::from_secs(10));
        let t = b.start();
        std::hint::black_box((0..1000).sum::<u64>());
        let r = t.finish();
        assert!(r.elapsed <= Duration::from_secs(10));
        assert!(!r.overran());
    }

    #[test]
    fn load_meter_smooths_and_counts() {
        let m = LoadMeter::new();
        m.set_smoothing(1.0); // no smoothing: track exactly
        let b = ProcessBudget::from_duration(Duration::from_millis(10));
        m.record(&b.report(Duration::from_millis(2)));
        m.record(&b.report(Duration::from_millis(12)));
        m.record(&b.report(Duration::from_millis(4)));
        assert!((m.load() - 0.4).abs() < 1e-6);
        assert!((m.peak() - 1.2).abs() < 1e-6);
        assert_eq!(m.overruns(), 1);
        assert_eq!(m.callbacks(), 3);
        m.reset_peak();
        assert_eq!(m.peak(), 0.0);

        let m = LoadMeter::new(); // default 10 %
        m.record(&b.report(Duration::from_millis(10)));
        assert!((m.load() - 0.1).abs() < 1e-6);
    }
}
