//! Decibel / linear gain helpers.

/// Gain below which [`db_to_linear`] returns exactly `0.0` (true silence).
pub const SILENCE_DB: f32 = -120.0;

/// Converts decibels to a linear amplitude multiplier.
///
/// Values at or below [`SILENCE_DB`] map to exactly `0.0`.
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    if db <= SILENCE_DB {
        0.0
    } else {
        10.0f32.powf(db / 20.0)
    }
}

/// Converts a linear amplitude multiplier to decibels.
///
/// Zero or negative input returns [`SILENCE_DB`] rather than `-inf`.
#[inline]
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 0.0 {
        SILENCE_DB
    } else {
        (20.0 * linear.log10()).max(SILENCE_DB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_and_common_points() {
        assert_eq!(db_to_linear(0.0), 1.0);
        assert!((db_to_linear(-6.0206) - 0.5).abs() < 1e-4);
        assert!((db_to_linear(6.0206) - 2.0).abs() < 1e-4);
        assert!((linear_to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(linear_to_db(1.0), 0.0);
    }

    #[test]
    fn silence_floor() {
        assert_eq!(db_to_linear(SILENCE_DB), 0.0);
        assert_eq!(db_to_linear(-200.0), 0.0);
        assert_eq!(linear_to_db(0.0), SILENCE_DB);
        assert_eq!(linear_to_db(-1.0), SILENCE_DB);
        assert_eq!(linear_to_db(1e-30), SILENCE_DB);
    }

    #[test]
    fn round_trip() {
        for db in [-60.0f32, -20.0, -3.0, 0.0, 3.0, 12.0] {
            assert!((linear_to_db(db_to_linear(db)) - db).abs() < 1e-3);
        }
    }
}
