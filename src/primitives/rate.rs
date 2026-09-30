//! Sample rate newtype.

use core::fmt;

use crate::primitives::{Result, SuperbError};

/// Audio sample rate in Hz.
///
/// Stored as an integer because every rate a DAW encounters in practice is
/// integral (44.1k, 48k, 88.2k, 96k, 176.4k, 192k, ...). Use
/// [`SampleRate::as_f64`] for conversions.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SampleRate(u32);

impl SampleRate {
    /// 44 100 Hz.
    pub const CD: SampleRate = SampleRate(44_100);
    /// 48 000 Hz.
    pub const HZ_48K: SampleRate = SampleRate(48_000);
    /// 96 000 Hz.
    pub const HZ_96K: SampleRate = SampleRate(96_000);

    /// Lowest rate accepted by [`SampleRate::new`].
    pub const MIN_HZ: u32 = 1_000;
    /// Highest rate accepted by [`SampleRate::new`].
    pub const MAX_HZ: u32 = 1_536_000;

    /// Creates a validated sample rate.
    ///
    /// Returns [`SuperbError::InvalidArgument`] outside `MIN_HZ..=MAX_HZ`.
    pub const fn new(hz: u32) -> Result<Self> {
        if hz < Self::MIN_HZ || hz > Self::MAX_HZ {
            Err(SuperbError::InvalidArgument {
                what: "sample_rate",
            })
        } else {
            Ok(Self(hz))
        }
    }

    /// Creates a sample rate without validation. Intended for constants.
    pub const fn new_unchecked(hz: u32) -> Self {
        Self(hz)
    }

    /// The rate in Hz as an integer.
    pub const fn hz(self) -> u32 {
        self.0
    }

    /// The rate in Hz as `f64`.
    pub const fn as_f64(self) -> f64 {
        self.0 as f64
    }

    /// Duration of a single sample period in seconds.
    pub fn period_seconds(self) -> f64 {
        1.0 / self.as_f64()
    }

    /// Converts a duration in seconds to a (rounded) number of samples.
    pub fn seconds_to_samples(self, seconds: f64) -> i64 {
        (seconds * self.as_f64()).round() as i64
    }

    /// Converts a number of samples to seconds.
    pub fn samples_to_seconds(self, samples: i64) -> f64 {
        samples as f64 / self.as_f64()
    }
}

impl Default for SampleRate {
    fn default() -> Self {
        Self::HZ_48K
    }
}

impl fmt::Debug for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SampleRate({} Hz)", self.0)
    }
}

impl fmt::Display for SampleRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} Hz", self.0)
    }
}

impl TryFrom<u32> for SampleRate {
    type Error = SuperbError;
    fn try_from(hz: u32) -> Result<Self> {
        Self::new(hz)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_bounds() {
        assert!(SampleRate::new(0).is_err());
        assert!(SampleRate::new(999).is_err());
        assert!(SampleRate::new(1_000).is_ok());
        assert!(SampleRate::new(192_000).is_ok());
        assert!(SampleRate::new(2_000_000).is_err());
    }

    #[test]
    fn conversions_round_trip_for_common_rates() {
        for rate in [SampleRate::CD, SampleRate::HZ_48K, SampleRate::HZ_96K] {
            let samples = rate.seconds_to_samples(1.0);
            assert_eq!(samples as u32, rate.hz());
            assert_eq!(rate.samples_to_seconds(samples), 1.0);
        }
        // 44.1k * 0.5 s = 22 050 exactly
        assert_eq!(SampleRate::CD.seconds_to_samples(0.5), 22_050);
        // rounding, not truncation
        assert_eq!(
            SampleRate::HZ_48K.seconds_to_samples(1.0 / 48_000.0 * 2.6),
            3
        );
    }

    #[test]
    fn display_and_default() {
        assert_eq!(SampleRate::default(), SampleRate::HZ_48K);
        assert_eq!(SampleRate::CD.to_string(), "44100 Hz");
        assert!((SampleRate::HZ_48K.period_seconds() - 1.0 / 48_000.0).abs() < 1e-15);
    }
}
