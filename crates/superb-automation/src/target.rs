//! Targets that automation can drive (typically `Arc<AtomicF32>`).

use std::sync::Arc;

use superb_rt::AtomicF32;

/// Something automation can read from and write to.
///
/// Implemented for [`AtomicF32`] and [`Arc<AtomicF32>`] so lanes can bind
/// directly to the same handles that mixer strips and graph nodes already
/// expose.
pub trait ParamTarget {
    /// Current value (UI or last automation write).
    fn get(&self) -> f32;
    /// Stores a new value (automation read path, or UI).
    fn set(&self, value: f32);
}

impl ParamTarget for AtomicF32 {
    #[inline]
    fn get(&self) -> f32 {
        AtomicF32::get(self)
    }

    #[inline]
    fn set(&self, value: f32) {
        AtomicF32::set(self, value);
    }
}

impl ParamTarget for Arc<AtomicF32> {
    #[inline]
    fn get(&self) -> f32 {
        AtomicF32::get(self)
    }

    #[inline]
    fn set(&self, value: f32) {
        AtomicF32::set(self, value);
    }
}

impl<T: ParamTarget + ?Sized> ParamTarget for &T {
    #[inline]
    fn get(&self) -> f32 {
        (*self).get()
    }

    #[inline]
    fn set(&self, value: f32) {
        (*self).set(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arc_atomic_round_trip() {
        let p = Arc::new(AtomicF32::new(0.25));
        assert_eq!(ParamTarget::get(&p), 0.25);
        ParamTarget::set(&p, 0.75);
        assert_eq!(p.get(), 0.75);
    }
}
