//! Lock-free floating point atomics for parameter sharing.

use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// An `f32` that can be read and written atomically.
///
/// Typical use: a UI thread stores a gain value, the audio thread loads it
/// once per block. Both operations are single instructions on all
/// supported targets.
#[derive(Debug)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    /// Creates a new atomic holding `value`.
    pub const fn new(value: f32) -> Self {
        Self(AtomicU32::new(value.to_bits()))
    }

    /// Loads the value with the given ordering.
    #[inline]
    pub fn load(&self, order: Ordering) -> f32 {
        f32::from_bits(self.0.load(order))
    }

    /// Stores `value` with the given ordering.
    #[inline]
    pub fn store(&self, value: f32, order: Ordering) {
        self.0.store(value.to_bits(), order)
    }

    /// Loads with `Relaxed` ordering (the common case for parameters).
    #[inline]
    pub fn get(&self) -> f32 {
        self.load(Ordering::Relaxed)
    }

    /// Stores with `Relaxed` ordering.
    #[inline]
    pub fn set(&self, value: f32) {
        self.store(value, Ordering::Relaxed)
    }

    /// Swaps in `value`, returning the previous value.
    #[inline]
    pub fn swap(&self, value: f32, order: Ordering) -> f32 {
        f32::from_bits(self.0.swap(value.to_bits(), order))
    }

    /// Atomically stores `max(current, value)`; returns the previous value.
    ///
    /// Used for peak meters: the audio thread raises the peak, the UI reads
    /// and resets it.
    pub fn fetch_max(&self, value: f32, order: Ordering) -> f32 {
        let mut current = self.0.load(Ordering::Relaxed);
        loop {
            let cur_f = f32::from_bits(current);
            // Only a strictly greater, non-NaN value replaces the current one.
            if value.partial_cmp(&cur_f) != Some(core::cmp::Ordering::Greater) {
                return cur_f;
            }
            match self
                .0
                .compare_exchange_weak(current, value.to_bits(), order, Ordering::Relaxed)
            {
                Ok(_) => return cur_f,
                Err(actual) => current = actual,
            }
        }
    }
}

impl Default for AtomicF32 {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// An `f64` that can be read and written atomically.
#[derive(Debug)]
pub struct AtomicF64(AtomicU64);

impl AtomicF64 {
    /// Creates a new atomic holding `value`.
    pub const fn new(value: f64) -> Self {
        Self(AtomicU64::new(value.to_bits()))
    }

    /// Loads the value with the given ordering.
    #[inline]
    pub fn load(&self, order: Ordering) -> f64 {
        f64::from_bits(self.0.load(order))
    }

    /// Stores `value` with the given ordering.
    #[inline]
    pub fn store(&self, value: f64, order: Ordering) {
        self.0.store(value.to_bits(), order)
    }

    /// Loads with `Relaxed` ordering.
    #[inline]
    pub fn get(&self) -> f64 {
        self.load(Ordering::Relaxed)
    }

    /// Stores with `Relaxed` ordering.
    #[inline]
    pub fn set(&self, value: f64) {
        self.store(value, Ordering::Relaxed)
    }
}

impl Default for AtomicF64 {
    fn default() -> Self {
        Self::new(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn round_trips_special_values() {
        let a = AtomicF32::new(0.0);
        for v in [0.0, -0.0, 1.5, -1e-30, f32::MAX, f32::INFINITY] {
            a.set(v);
            assert_eq!(a.get().to_bits(), v.to_bits());
        }
        a.set(f32::NAN);
        assert!(a.get().is_nan());
        let d = AtomicF64::new(2.5);
        assert_eq!(d.get(), 2.5);
        d.set(-7.25);
        assert_eq!(d.load(Ordering::SeqCst), -7.25);
    }

    #[test]
    fn fetch_max_behaves_like_peak_hold() {
        let peak = AtomicF32::new(0.0);
        assert_eq!(peak.fetch_max(0.5, Ordering::Relaxed), 0.0);
        assert_eq!(peak.fetch_max(0.25, Ordering::Relaxed), 0.5);
        assert_eq!(peak.get(), 0.5);
        assert_eq!(peak.fetch_max(f32::NAN, Ordering::Relaxed), 0.5); // NaN never wins
        assert_eq!(peak.swap(0.0, Ordering::Relaxed), 0.5);
        assert_eq!(peak.get(), 0.0);
    }

    #[test]
    fn shared_between_threads() {
        let value = Arc::new(AtomicF32::new(0.0));
        let writer = {
            let value = Arc::clone(&value);
            std::thread::spawn(move || {
                for i in 0..10_000 {
                    value.store(i as f32, Ordering::Release);
                }
            })
        };
        writer.join().unwrap();
        assert_eq!(value.load(Ordering::Acquire), 9_999.0);
    }
}
