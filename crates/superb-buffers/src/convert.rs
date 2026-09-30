//! Sample format conversion (integer PCM <-> `f32`) built on `dasp_sample`.
//!
//! These helpers are slice-based so a device or file layer can convert
//! without allocating.

use dasp_sample::{FromSample, Sample, ToSample};

/// Converts any `dasp` sample type into `f32` in `[-1.0, 1.0]`.
///
/// Panics if `dst.len() < src.len()`.
pub fn to_f32<S>(src: &[S], dst: &mut [f32])
where
    S: Sample + ToSample<f32>,
{
    assert!(dst.len() >= src.len(), "destination too small");
    for (d, s) in dst.iter_mut().zip(src) {
        *d = s.to_sample::<f32>();
    }
}

/// Converts `f32` samples into any `dasp` sample type (with clamping for ints).
///
/// Panics if `dst.len() < src.len()`.
pub fn from_f32<S>(src: &[f32], dst: &mut [S])
where
    S: Sample + FromSample<f32>,
{
    assert!(dst.len() >= src.len(), "destination too small");
    for (d, s) in dst.iter_mut().zip(src) {
        *d = S::from_sample(*s);
    }
}

/// Clamps a float sample to `[-1.0, 1.0]`.
#[inline]
pub fn clamp_unit(sample: f32) -> f32 {
    sample.clamp(-1.0, 1.0)
}

/// Converts `f32` to 16-bit PCM with saturation (independent of `dasp`,
/// useful for WAV writers that need exact behaviour at the rails).
#[inline]
pub fn f32_to_i16_saturating(sample: f32) -> i16 {
    let s = clamp_unit(sample);
    if s >= 1.0 {
        i16::MAX
    } else {
        (s * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_round_trip_is_close() {
        let ints: Vec<i16> = vec![0, 16_384, -16_384, i16::MAX, i16::MIN];
        let mut floats = vec![0.0f32; ints.len()];
        to_f32(&ints, &mut floats);
        assert_eq!(floats[0], 0.0);
        assert!((floats[1] - 0.5).abs() < 1e-4);
        assert!((floats[2] + 0.5).abs() < 1e-4);
        assert!(floats[3] > 0.99 && floats[3] <= 1.0);
        assert_eq!(floats[4], -1.0);

        let mut back = vec![0i16; ints.len()];
        from_f32(&floats, &mut back);
        for (a, b) in ints.iter().zip(&back) {
            assert!((*a as i32 - *b as i32).abs() <= 1, "{a} vs {b}");
        }
    }

    #[test]
    fn overs_are_saturated() {
        assert_eq!(f32_to_i16_saturating(2.0), i16::MAX);
        assert_eq!(f32_to_i16_saturating(-2.0), i16::MIN);
        assert_eq!(f32_to_i16_saturating(0.0), 0);
        assert_eq!(f32_to_i16_saturating(-1.0), i16::MIN);
        assert_eq!(clamp_unit(1.5), 1.0);
        let mut out = [0i16; 1];
        from_f32(&[3.0], &mut out);
        assert_eq!(out[0], i16::MAX);
    }

    #[test]
    fn u8_unsigned_pcm_is_centred() {
        let mut f = [0.0f32; 3];
        to_f32(&[0u8, 128u8, 255u8], &mut f);
        assert_eq!(f[0], -1.0);
        assert_eq!(f[1], 0.0);
        assert!(f[2] > 0.99);
    }
}
