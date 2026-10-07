//! One second-order section, the building block of the K-weighting, the
//! de-esser's sidechain and whatever else needs a shelf, a pass or a band.
//! Transposed direct form II in `f64`, so a 38 Hz high-pass at 192 kHz
//! keeps its poles where they belong.

/// A biquad: `y = (b0 x + b1 x⁻¹ + b2 x⁻²) / (1 + a1 y⁻¹ + a2 y⁻²)`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Biquad {
    pub(crate) b0: f64,
    pub(crate) b1: f64,
    pub(crate) b2: f64,
    pub(crate) a1: f64,
    pub(crate) a2: f64,
    z1: f64,
    z2: f64,
}

impl Biquad {
    /// From unnormalised coefficients: `b` over `a`, divided by `a[0]`.
    pub(crate) fn new(b: [f64; 3], a: [f64; 3]) -> Self {
        Biquad {
            b0: b[0] / a[0],
            b1: b[1] / a[0],
            b2: b[2] / a[0],
            a1: a[1] / a[0],
            a2: a[2] / a[0],
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// A filter given at `from` Hz, as it is at `to` Hz: `s = c (1 - z⁻¹) /
    /// (1 + z⁻¹)` with `c = 2π f₀ / tan(π f₀ / rate)` turns the digital
    /// filter into the analog one and back, so the response at `anchor_hz`
    /// is kept exactly and the rest follows the analog curve. `digital` is
    /// `[b0, b1, b2, a1, a2]` with `a0 = 1`.
    pub(crate) fn moved(digital: [f64; 5], anchor_hz: f64, from: f64, to: f64) -> Self {
        let [b0, b1, b2, a1, a2] = digital;
        let warp = |rate: f64| {
            2.0 * core::f64::consts::PI * anchor_hz
                / (core::f64::consts::PI * anchor_hz / rate).tan()
        };
        let (c, d) = (warp(from), warp(to));
        // Analog numerator and denominator: polynomials in s.
        let n = [b0 - b1 + b2, 2.0 * c * (b0 - b2), c * c * (b0 + b1 + b2)];
        let m = [1.0 - a1 + a2, 2.0 * c * (1.0 - a2), c * c * (1.0 + a1 + a2)];
        let digitise = |[p2, p1, p0]: [f64; 3]| {
            [
                p2 * d * d + p1 * d + p0,
                -2.0 * p2 * d * d + 2.0 * p0,
                p2 * d * d - p1 * d + p0,
            ]
        };
        Biquad::new(digitise(n), digitise(m))
    }

    /// A band-pass with unity gain at `f0` and the width `q` gives (the
    /// usual second-order design: `α = sin ω₀ / 2Q`).
    pub(crate) fn band_pass(rate: f64, f0: f64, q: f64) -> Self {
        let w0 = 2.0 * core::f64::consts::PI * f0 / rate;
        let (cos, alpha) = (w0.cos(), w0.sin() / (2.0 * q));
        Biquad::new([alpha, 0.0, -alpha], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    /// One sample in, one out.
    #[inline]
    pub(crate) fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    /// Forgets what it heard.
    pub(crate) fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Steady-state gain in dB of `filter` at `hz`, from a second of sine.
    pub(crate) fn gain_db(filter: &Biquad, hz: f64, rate: f64) -> f64 {
        let mut f = *filter;
        let n = rate as usize;
        let mut peak = 0.0f64;
        for i in 0..n {
            let y = f.process((2.0 * core::f64::consts::PI * hz * i as f64 / rate).sin());
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        20.0 * peak.log10()
    }

    #[test]
    fn a_band_pass_passes_its_band_and_little_else() {
        let bp = Biquad::band_pass(48_000.0, 6_000.0, 1.0);
        assert!(gain_db(&bp, 6_000.0, 48_000.0).abs() < 0.1);
        assert!(gain_db(&bp, 200.0, 48_000.0) < -25.0);
        assert!(gain_db(&bp, 20_000.0, 48_000.0) < -10.0);
        let mut f = bp;
        f.process(1.0);
        f.reset();
        assert_eq!(f.process(0.0), 0.0);
    }
}
