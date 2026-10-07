//! Lining two recordings up by their audio: how far one lags the other.
//!
//! Two microphones on the same performance, or a guest's recording against
//! the host's reference, carry the same sound at a different time. The
//! shift that lines them up is where their cross-correlation peaks, and
//! how clearly it peaks says how much to trust it.

use super::fft::Fft;
use crate::primitives::{Result, SuperbError};

/// What [`estimate`] found.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offset {
    /// How many samples `other` lags `reference`: `other[n]` sounds like
    /// `reference[n - shift]`. Positive means `other` is late; to line it
    /// up, start it `shift` samples earlier (or place the reference that
    /// much later).
    pub shift: i64,
    /// How alike the two are at that shift, 0.0 to 1.0: the normalised
    /// correlation over their overlap. Two recordings of one performance
    /// score well above 0.5; unrelated audio scores near 0.
    pub confidence: f32,
}

/// The shift that best lines `other` up with `reference`, searched within
/// `max_shift` samples either way, with a confidence. Both signals are
/// mono at the same sample rate (sum a stereo pair, or pick a channel);
/// their means are removed first, so a DC offset does not count as
/// similarity. The work is a pair of FFTs the size of both signals
/// together, so pass excerpts: a minute of each at 48 kHz is a few
/// hundred megabytes of working memory and under a second; an hour is
/// neither necessary nor wise. Inverted polarity is not matched: a flipped
/// microphone scores low.
///
/// # Errors
/// An empty signal.
pub fn estimate(reference: &[f32], other: &[f32], max_shift: usize) -> Result<Offset> {
    if reference.is_empty() || other.is_empty() {
        return Err(SuperbError::InvalidArgument {
            what: "offset::estimate needs two signals",
        });
    }
    let (a, b) = (centred(reference), centred(other));
    let longest = a.len().max(b.len());
    let max_shift = max_shift.min(longest - 1) as i64;
    // Linear correlation through a circular one: room for both signals,
    // so no lag wraps into another.
    let n = (a.len() + b.len() - 1).next_power_of_two().max(2);
    let fft = Fft::new(n);
    let mut ar = a.clone();
    ar.resize(n, 0.0);
    let mut ai = vec![0.0; n];
    let mut br = b.clone();
    br.resize(n, 0.0);
    let mut bi = vec![0.0; n];
    fft.forward(&mut ar, &mut ai);
    fft.forward(&mut br, &mut bi);
    // conj(A) · B: its inverse at index k is Σ a[n] · b[n + k].
    for k in 0..n {
        let (re, im) = (ar[k] * br[k] + ai[k] * bi[k], ar[k] * bi[k] - ai[k] * br[k]);
        ar[k] = re;
        ai[k] = im;
    }
    fft.inverse(&mut ar, &mut ai);
    let correlation = ar;
    let mut best = (0i64, f64::NEG_INFINITY);
    for shift in -max_shift..=max_shift {
        let index = shift.rem_euclid(n as i64) as usize;
        if correlation[index] > best.1 {
            best = (shift, correlation[index]);
        }
    }
    let shift = best.0;
    Ok(Offset {
        shift,
        confidence: normalised_at(&a, &b, shift),
    })
}

/// The signal in `f64` with its mean removed.
fn centred(samples: &[f32]) -> Vec<f64> {
    let mean = samples.iter().map(|&s| f64::from(s)).sum::<f64>() / samples.len() as f64;
    samples.iter().map(|&s| f64::from(s) - mean).collect()
}

/// The normalised correlation of `a[n]` with `b[n + shift]` over the
/// samples both have, clamped to 0.0..=1.0.
fn normalised_at(a: &[f64], b: &[f64], shift: i64) -> f32 {
    let (mut dot, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for (n, &x) in a.iter().enumerate() {
        let m = n as i64 + shift;
        if m < 0 || m >= b.len() as i64 {
            continue;
        }
        let y = b[m as usize];
        dot += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa <= 0.0 || bb <= 0.0 {
        return 0.0;
    }
    (dot / (aa * bb).sqrt()).clamp(0.0, 1.0) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic noise: a linear congruential generator is plenty.
    fn noise(len: usize, seed: u64) -> Vec<f32> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                ((state >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
            })
            .collect()
    }

    #[test]
    fn a_delayed_copy_is_found_exactly() {
        let reference = noise(20_000, 1);
        // `other` is the reference 1 234 samples late, quieter, with its
        // own noise on top and a DC offset.
        let hiss = noise(20_000, 2);
        let other: Vec<f32> = (0..20_000)
            .map(|n| {
                let late = if n >= 1_234 {
                    reference[n - 1_234]
                } else {
                    0.0
                };
                0.5 * late + 0.1 * hiss[n] + 0.3
            })
            .collect();
        let found = estimate(&reference, &other, 5_000).unwrap();
        assert_eq!(found.shift, 1_234);
        assert!(found.confidence > 0.9, "{}", found.confidence);

        // The other way round the shift is negative.
        let back = estimate(&other, &reference, 5_000).unwrap();
        assert_eq!(back.shift, -1_234);

        // Outside the search window the best shift inside is reported, and
        // it is not a confident one.
        let bounded = estimate(&reference, &other, 500).unwrap();
        assert!(bounded.shift.abs() <= 500);
        assert!(bounded.confidence < 0.3, "{}", bounded.confidence);
    }

    #[test]
    fn unrelated_audio_scores_low_and_empty_input_is_refused() {
        let a = noise(10_000, 3);
        let b = noise(10_000, 4);
        let found = estimate(&a, &b, 2_000).unwrap();
        assert!(found.confidence < 0.2, "{}", found.confidence);
        assert!(estimate(&[], &b, 10).is_err());
        assert!(estimate(&a, &[], 10).is_err());
        // Shorter than the window asked for: the window shrinks to fit.
        let short = estimate(&a[..100], &a[..100], 1_000).unwrap();
        assert_eq!(short.shift, 0);
        assert!((short.confidence - 1.0).abs() < 1e-6);
    }
}
