//! Sample-accurate and musical time.
//!
//! Two independent domains are modelled:
//!
//! * **Audio time** — [`TimePosition`] (an absolute sample index) and
//!   [`TimeDuration`] (a signed sample count). Both are integers so that
//!   arithmetic on the audio thread is exact and free of rounding drift.
//! * **Musical time** — [`BeatPosition`] and [`BeatDuration`] in quarter
//!   notes. Conversion between the two domains requires a tempo map, which
//!   lives in `superb-transport`.

use core::fmt;
use core::ops::{Add, AddAssign, Neg, Sub, SubAssign};

use crate::{Result, SampleRate, SuperbError};

// ---------------------------------------------------------------------------
// Audio-domain time
// ---------------------------------------------------------------------------

/// A signed number of samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimeDuration(i64);

impl TimeDuration {
    /// Zero-length duration.
    pub const ZERO: Self = Self(0);

    /// Creates a duration from a signed sample count.
    pub const fn from_samples(samples: i64) -> Self {
        Self(samples)
    }

    /// Creates a duration from seconds at the given rate (rounded to nearest sample).
    pub fn from_seconds(seconds: f64, rate: SampleRate) -> Self {
        Self(rate.seconds_to_samples(seconds))
    }

    /// Signed sample count.
    pub const fn samples(self) -> i64 {
        self.0
    }

    /// Duration in seconds at the given rate.
    pub fn to_seconds(self, rate: SampleRate) -> f64 {
        rate.samples_to_seconds(self.0)
    }

    /// Absolute value.
    pub const fn abs(self) -> Self {
        Self(self.0.abs())
    }

    /// `true` if the duration is negative.
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
}

/// An absolute position on the audio timeline, in samples.
///
/// Positions are signed so that pre-roll and count-in (negative positions)
/// can be represented without special cases.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TimePosition(i64);

impl TimePosition {
    /// The origin of the timeline.
    pub const ZERO: Self = Self(0);

    /// Creates a position from an absolute sample index.
    pub const fn from_samples(samples: i64) -> Self {
        Self(samples)
    }

    /// Creates a position from seconds at the given rate (rounded to nearest sample).
    pub fn from_seconds(seconds: f64, rate: SampleRate) -> Self {
        Self(rate.seconds_to_samples(seconds))
    }

    /// Absolute sample index.
    pub const fn samples(self) -> i64 {
        self.0
    }

    /// Position in seconds at the given rate.
    pub fn to_seconds(self, rate: SampleRate) -> f64 {
        rate.samples_to_seconds(self.0)
    }

    /// Offsets the position by a signed number of samples.
    pub const fn offset(self, samples: i64) -> Self {
        Self(self.0 + samples)
    }

    /// Distance from `other` to `self` (`self - other`).
    pub const fn distance_from(self, other: Self) -> TimeDuration {
        TimeDuration(self.0 - other.0)
    }

    /// Clamps the position into `[lo, hi]`.
    pub fn clamp(self, lo: Self, hi: Self) -> Self {
        Self(self.0.clamp(lo.0, hi.0))
    }

    /// Returns the position as an unsigned sample index, or an error if negative.
    pub fn as_unsigned(self) -> Result<u64> {
        u64::try_from(self.0).map_err(|_| SuperbError::InvalidArgument {
            what: "negative position",
        })
    }
}

impl Add<TimeDuration> for TimePosition {
    type Output = TimePosition;
    fn add(self, rhs: TimeDuration) -> Self::Output {
        TimePosition(self.0 + rhs.0)
    }
}

impl AddAssign<TimeDuration> for TimePosition {
    fn add_assign(&mut self, rhs: TimeDuration) {
        self.0 += rhs.0;
    }
}

impl Sub<TimeDuration> for TimePosition {
    type Output = TimePosition;
    fn sub(self, rhs: TimeDuration) -> Self::Output {
        TimePosition(self.0 - rhs.0)
    }
}

impl SubAssign<TimeDuration> for TimePosition {
    fn sub_assign(&mut self, rhs: TimeDuration) {
        self.0 -= rhs.0;
    }
}

impl Sub<TimePosition> for TimePosition {
    type Output = TimeDuration;
    fn sub(self, rhs: TimePosition) -> Self::Output {
        TimeDuration(self.0 - rhs.0)
    }
}

impl Add for TimeDuration {
    type Output = TimeDuration;
    fn add(self, rhs: Self) -> Self::Output {
        TimeDuration(self.0 + rhs.0)
    }
}

impl AddAssign for TimeDuration {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for TimeDuration {
    type Output = TimeDuration;
    fn sub(self, rhs: Self) -> Self::Output {
        TimeDuration(self.0 - rhs.0)
    }
}

impl SubAssign for TimeDuration {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

impl Neg for TimeDuration {
    type Output = TimeDuration;
    fn neg(self) -> Self::Output {
        TimeDuration(-self.0)
    }
}

impl fmt::Display for TimePosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} smp", self.0)
    }
}

impl fmt::Display for TimeDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} smp", self.0)
    }
}

// ---------------------------------------------------------------------------
// Musical-domain time
// ---------------------------------------------------------------------------

/// A signed musical duration in quarter notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct BeatDuration(f64);

impl BeatDuration {
    /// Zero-length duration.
    pub const ZERO: Self = Self(0.0);

    /// Creates a duration in quarter notes.
    pub const fn new(beats: f64) -> Self {
        Self(beats)
    }

    /// Quarter-note count.
    pub const fn beats(self) -> f64 {
        self.0
    }

    /// Absolute value.
    pub fn abs(self) -> Self {
        Self(self.0.abs())
    }
}

/// An absolute musical position in quarter notes from the start of the song.
///
/// Stored as `f64` (as most engines do in practice); sub-sample accuracy is
/// preserved for any realistic song length.
#[derive(Clone, Copy, Debug, Default, PartialEq, PartialOrd)]
pub struct BeatPosition(f64);

impl BeatPosition {
    /// The origin (bar 1, beat 1).
    pub const ZERO: Self = Self(0.0);

    /// Creates a beat position in quarter notes.
    pub const fn new(beats: f64) -> Self {
        Self(beats)
    }

    /// Quarter-note count from the song start.
    pub const fn beats(self) -> f64 {
        self.0
    }

    /// Distance from `other` to `self`.
    pub fn distance_from(self, other: Self) -> BeatDuration {
        BeatDuration(self.0 - other.0)
    }

    /// Snaps to the nearest multiple of `grid` beats (e.g. `0.25` for 16ths).
    pub fn snap(self, grid: f64) -> Self {
        if grid <= 0.0 {
            return self;
        }
        Self((self.0 / grid).round() * grid)
    }

    /// Rounds *down* to the nearest multiple of `grid` beats.
    pub fn floor_to(self, grid: f64) -> Self {
        if grid <= 0.0 {
            return self;
        }
        Self((self.0 / grid).floor() * grid)
    }

    /// Returns `true` if `self` is not NaN or infinite.
    pub fn is_finite(self) -> bool {
        self.0.is_finite()
    }
}

impl Add<BeatDuration> for BeatPosition {
    type Output = BeatPosition;
    fn add(self, rhs: BeatDuration) -> Self::Output {
        BeatPosition(self.0 + rhs.0)
    }
}

impl AddAssign<BeatDuration> for BeatPosition {
    fn add_assign(&mut self, rhs: BeatDuration) {
        self.0 += rhs.0;
    }
}

impl Sub<BeatDuration> for BeatPosition {
    type Output = BeatPosition;
    fn sub(self, rhs: BeatDuration) -> Self::Output {
        BeatPosition(self.0 - rhs.0)
    }
}

impl Sub<BeatPosition> for BeatPosition {
    type Output = BeatDuration;
    fn sub(self, rhs: BeatPosition) -> Self::Output {
        BeatDuration(self.0 - rhs.0)
    }
}

impl Add for BeatDuration {
    type Output = BeatDuration;
    fn add(self, rhs: Self) -> Self::Output {
        BeatDuration(self.0 + rhs.0)
    }
}

impl Sub for BeatDuration {
    type Output = BeatDuration;
    fn sub(self, rhs: Self) -> Self::Output {
        BeatDuration(self.0 - rhs.0)
    }
}

impl Neg for BeatDuration {
    type Output = BeatDuration;
    fn neg(self) -> Self::Output {
        BeatDuration(-self.0)
    }
}

impl fmt::Display for BeatPosition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} beats", self.0)
    }
}

impl fmt::Display for BeatDuration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.3} beats", self.0)
    }
}

// ---------------------------------------------------------------------------
// Meter
// ---------------------------------------------------------------------------

/// A time signature such as `4/4` or `7/8`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TimeSignature {
    numerator: u8,
    denominator: u8,
}

impl TimeSignature {
    /// Common time, `4/4`.
    pub const FOUR_FOUR: Self = Self {
        numerator: 4,
        denominator: 4,
    };

    /// Creates a validated time signature.
    ///
    /// The numerator must be `1..=64`; the denominator must be a power of two
    /// in `1..=64`.
    pub const fn new(numerator: u8, denominator: u8) -> Result<Self> {
        if numerator == 0 || numerator > 64 {
            return Err(SuperbError::InvalidArgument {
                what: "time_signature.numerator",
            });
        }
        if denominator == 0 || denominator > 64 || !denominator.is_power_of_two() {
            return Err(SuperbError::InvalidArgument {
                what: "time_signature.denominator",
            });
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }

    /// Beats per bar (the numerator).
    pub const fn numerator(self) -> u8 {
        self.numerator
    }

    /// Note value that gets one beat (the denominator).
    pub const fn denominator(self) -> u8 {
        self.denominator
    }

    /// Length of one bar in quarter notes.
    ///
    /// `4/4` → 4.0, `3/4` → 3.0, `6/8` → 3.0, `7/8` → 3.5.
    pub fn bar_length(self) -> BeatDuration {
        BeatDuration(self.numerator as f64 * 4.0 / self.denominator as f64)
    }

    /// Length of one *notated* beat in quarter notes (`4/4` → 1.0, `6/8` → 0.5).
    pub fn beat_length(self) -> BeatDuration {
        BeatDuration(4.0 / self.denominator as f64)
    }
}

impl Default for TimeSignature {
    fn default() -> Self {
        Self::FOUR_FOUR
    }
}

impl fmt::Display for TimeSignature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.numerator, self.denominator)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R48: SampleRate = SampleRate::HZ_48K;

    #[test]
    fn position_arithmetic_is_exact_integer_math() {
        let start = TimePosition::from_samples(1_000);
        let block = TimeDuration::from_samples(256);
        let mut pos = start;
        for _ in 0..1_000_000 {
            pos += block;
        }
        assert_eq!(pos.samples(), 1_000 + 256 * 1_000_000);
        assert_eq!(pos - start, TimeDuration::from_samples(256 * 1_000_000));
        assert_eq!(pos.distance_from(start).samples(), 256_000_000);
    }

    #[test]
    fn negative_positions_represent_preroll() {
        let preroll = TimePosition::from_seconds(-2.0, R48);
        assert_eq!(preroll.samples(), -96_000);
        assert!(preroll < TimePosition::ZERO);
        assert!(preroll.as_unsigned().is_err());
        assert_eq!(TimePosition::from_samples(5).as_unsigned().unwrap(), 5);
        assert!((preroll - TimePosition::ZERO).is_negative());
    }

    #[test]
    fn seconds_round_trip() {
        let p = TimePosition::from_seconds(1.25, R48);
        assert_eq!(p.samples(), 60_000);
        assert_eq!(p.to_seconds(R48), 1.25);
        let d = TimeDuration::from_seconds(0.001, R48);
        assert_eq!(d.samples(), 48);
        assert_eq!((-d).samples(), -48);
        assert_eq!((-d).abs(), d);
    }

    #[test]
    fn clamp_and_offset() {
        let lo = TimePosition::from_samples(0);
        let hi = TimePosition::from_samples(100);
        assert_eq!(TimePosition::from_samples(-5).clamp(lo, hi), lo);
        assert_eq!(TimePosition::from_samples(500).clamp(lo, hi), hi);
        assert_eq!(TimePosition::from_samples(50).offset(-10).samples(), 40);
    }

    #[test]
    fn beat_snapping() {
        let b = BeatPosition::new(3.37);
        assert_eq!(b.snap(0.25).beats(), 3.25);
        assert_eq!(b.snap(0.5).beats(), 3.5);
        assert_eq!(b.floor_to(1.0).beats(), 3.0);
        assert_eq!(b.snap(0.0), b); // degenerate grid is a no-op
        let moved = b + BeatDuration::new(1.0);
        assert!((moved.beats() - 4.37).abs() < 1e-12);
        assert!(((moved - b).beats() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn time_signature_validation_and_lengths() {
        assert!(TimeSignature::new(0, 4).is_err());
        assert!(TimeSignature::new(4, 3).is_err());
        assert!(TimeSignature::new(4, 0).is_err());
        assert!(TimeSignature::new(65, 4).is_err());
        let six_eight = TimeSignature::new(6, 8).unwrap();
        assert_eq!(six_eight.bar_length().beats(), 3.0);
        assert_eq!(six_eight.beat_length().beats(), 0.5);
        assert_eq!(TimeSignature::FOUR_FOUR.bar_length().beats(), 4.0);
        assert_eq!(TimeSignature::new(7, 8).unwrap().bar_length().beats(), 3.5);
        assert_eq!(six_eight.to_string(), "6/8");
        assert_eq!(TimeSignature::default(), TimeSignature::FOUR_FOUR);
    }

    #[test]
    fn display_formats() {
        assert_eq!(TimePosition::from_samples(12).to_string(), "12 smp");
        assert_eq!(BeatPosition::new(1.5).to_string(), "1.500 beats");
    }
}
