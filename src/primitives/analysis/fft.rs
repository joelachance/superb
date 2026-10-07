//! A radix-2 complex FFT for the analysis tools. Offset estimation needs a
//! cross-correlation over long stretches, which is a multiplication in the
//! frequency domain; nothing else in the crate needs one, so this stays
//! private and plain: iterative, in place, sizes a power of two, `f64`
//! throughout so a million-point correlation keeps its peak where it is.

/// A transform of one fixed size: the twiddle factors and the bit-reversal
/// order are computed once and reused.
pub(crate) struct Fft {
    n: usize,
    cos: Vec<f64>,
    sin: Vec<f64>,
    reversed: Vec<u32>,
}

impl Fft {
    /// A transform of `n` points; `n` must be a power of two of at least 2.
    pub(crate) fn new(n: usize) -> Self {
        assert!(
            n >= 2 && n.is_power_of_two(),
            "fft size must be a power of two"
        );
        let bits = n.trailing_zeros();
        let reversed = (0..n as u32)
            .map(|i| i.reverse_bits() >> (32 - bits))
            .collect();
        let (cos, sin) = (0..n / 2)
            .map(|k| {
                let angle = -2.0 * core::f64::consts::PI * k as f64 / n as f64;
                (angle.cos(), angle.sin())
            })
            .unzip();
        Fft {
            n,
            cos,
            sin,
            reversed,
        }
    }

    /// Points per transform.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.n
    }

    /// The forward transform of `re + i·im`, in place.
    pub(crate) fn forward(&self, re: &mut [f64], im: &mut [f64]) {
        self.transform(re, im, 1.0);
    }

    /// The inverse transform, in place, scaled by `1/n` so that inverse ∘
    /// forward is the identity.
    pub(crate) fn inverse(&self, re: &mut [f64], im: &mut [f64]) {
        self.transform(re, im, -1.0);
        let scale = 1.0 / self.n as f64;
        for (r, i) in re.iter_mut().zip(im.iter_mut()) {
            *r *= scale;
            *i *= scale;
        }
    }

    /// Cooley–Tukey, decimation in time: reorder, then butterflies of
    /// doubling span. `sign` is +1 forward, −1 inverse (conjugate twiddles).
    fn transform(&self, re: &mut [f64], im: &mut [f64], sign: f64) {
        let n = self.n;
        assert!(re.len() == n && im.len() == n, "fft buffers must be n long");
        for (i, &j) in self.reversed.iter().enumerate() {
            let j = j as usize;
            if i < j {
                re.swap(i, j);
                im.swap(i, j);
            }
        }
        let mut span = 2;
        while span <= n {
            let half = span / 2;
            let stride = n / span;
            for start in (0..n).step_by(span) {
                for k in 0..half {
                    let (wr, wi) = (self.cos[k * stride], sign * self.sin[k * stride]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * wr - im[b] * wi;
                    let ti = re[b] * wi + im[b] * wr;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            span *= 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_lands_in_one_bin_and_comes_back() {
        let n = 64;
        let fft = Fft::new(n);
        assert_eq!(fft.len(), n);
        // Three cycles of a cosine across the window: energy in bins 3 and 61.
        let mut re: Vec<f64> = (0..n)
            .map(|i| (2.0 * core::f64::consts::PI * 3.0 * i as f64 / n as f64).cos())
            .collect();
        let original = re.clone();
        let mut im = vec![0.0; n];
        fft.forward(&mut re, &mut im);
        for k in 0..n {
            let magnitude = (re[k] * re[k] + im[k] * im[k]).sqrt();
            if k == 3 || k == n - 3 {
                assert!(
                    (magnitude - n as f64 / 2.0).abs() < 1e-9,
                    "bin {k}: {magnitude}"
                );
            } else {
                assert!(magnitude < 1e-9, "bin {k}: {magnitude}");
            }
        }
        fft.inverse(&mut re, &mut im);
        for (a, b) in re.iter().zip(&original) {
            assert!((a - b).abs() < 1e-12);
        }
        assert!(im.iter().all(|v| v.abs() < 1e-12));
    }

    #[test]
    #[should_panic(expected = "power of two")]
    fn sizes_must_be_powers_of_two() {
        let _ = Fft::new(48);
    }
}
