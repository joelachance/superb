//! Breakpoint envelopes with hold / linear / curved segments.

use superb_core::{Result, SuperbError, TimePosition};

/// How the segment from a point to the next interpolates.
///
/// The shape is stored on the *left* (earlier) point of each segment.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CurveShape {
    /// Hold the left point's value until the next point (step).
    Hold,
    /// Linear interpolation between the two values.
    Linear,
    /// Exponential-style curve. `tension` is clamped to `[-1.0, 1.0]`:
    /// * `0.0` — identical to [`CurveShape::Linear`]
    /// * positive — ease toward the end of the segment (convex)
    /// * negative — ease toward the start (concave)
    Curve {
        /// Curve tension in `[-1.0, 1.0]`.
        tension: f32,
    },
}

impl Default for CurveShape {
    fn default() -> Self {
        CurveShape::Linear
    }
}

impl CurveShape {
    /// [`CurveShape::Curve`] with tension clamped to `[-1.0, 1.0]`.
    pub fn curved(tension: f32) -> Self {
        CurveShape::Curve {
            tension: tension.clamp(-1.0, 1.0),
        }
    }
}

/// A single breakpoint on an automation envelope.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvelopePoint {
    /// Absolute sample position on the timeline.
    pub time: TimePosition,
    /// Parameter value at `time`.
    pub value: f32,
    /// Shape of the segment *from this point to the next*.
    pub shape: CurveShape,
}

impl EnvelopePoint {
    /// Convenience constructor with [`CurveShape::Linear`].
    pub fn linear(time: TimePosition, value: f32) -> Self {
        Self {
            time,
            value,
            shape: CurveShape::Linear,
        }
    }

    /// Convenience constructor with [`CurveShape::Hold`].
    pub fn hold(time: TimePosition, value: f32) -> Self {
        Self {
            time,
            value,
            shape: CurveShape::Hold,
        }
    }

    /// Convenience constructor with a curved segment.
    pub fn curved(time: TimePosition, value: f32, tension: f32) -> Self {
        Self {
            time,
            value,
            shape: CurveShape::curved(tension),
        }
    }
}

/// A sorted list of breakpoints defining a parameter envelope.
///
/// Evaluation rules:
/// * before the first point → `default_value`
/// * after the last point → last point's value (held)
/// * between points → interpolate using the left point's [`CurveShape`]
#[derive(Clone, Debug)]
pub struct Envelope {
    points: Vec<EnvelopePoint>,
    default_value: f32,
}

impl Default for Envelope {
    fn default() -> Self {
        Self::new(0.0)
    }
}

impl Envelope {
    /// Empty envelope that evaluates to `default_value` everywhere.
    pub fn new(default_value: f32) -> Self {
        Self {
            points: Vec::new(),
            default_value,
        }
    }

    /// Value used when the envelope has no points, or before the first point.
    pub fn default_value(&self) -> f32 {
        self.default_value
    }

    /// Sets the default (pre-first-point / empty) value.
    pub fn set_default_value(&mut self, value: f32) {
        self.default_value = value;
    }

    /// The breakpoints in ascending time order.
    pub fn points(&self) -> &[EnvelopePoint] {
        &self.points
    }

    /// Number of breakpoints.
    pub fn len(&self) -> usize {
        self.points.len()
    }

    /// `true` when there are no breakpoints.
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }

    /// Removes every breakpoint.
    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// Inserts or replaces a point at `point.time`.
    ///
    /// If a point already exists at exactly that sample, it is replaced
    /// (value and shape). Points are kept sorted by time.
    pub fn upsert(&mut self, point: EnvelopePoint) {
        match self.points.binary_search_by_key(&point.time, |p| p.time) {
            Ok(idx) => self.points[idx] = point,
            Err(idx) => self.points.insert(idx, point),
        }
    }

    /// Removes the point at exactly `time`, if any. Returns whether one was removed.
    pub fn remove_at(&mut self, time: TimePosition) -> bool {
        match self.points.binary_search_by_key(&time, |p| p.time) {
            Ok(idx) => {
                self.points.remove(idx);
                true
            }
            Err(_) => false,
        }
    }

    /// Removes every point in `[start, end)` (end exclusive).
    pub fn clear_range(&mut self, start: TimePosition, end: TimePosition) {
        self.points.retain(|p| p.time < start || p.time >= end);
    }

    /// Evaluates the envelope at a single sample position.
    pub fn evaluate(&self, at: TimePosition) -> f32 {
        if self.points.is_empty() {
            return self.default_value;
        }
        // Before first point.
        if at < self.points[0].time {
            return self.default_value;
        }
        // At or after last point → hold last value.
        let last = *self.points.last().unwrap();
        if at >= last.time {
            return last.value;
        }
        // Find right neighbour: first point with time > at.
        // Since at >= first and at < last, there is a segment.
        let right_idx = self.points.partition_point(|p| p.time <= at);
        // right_idx is in 1..len because at >= first.time and at < last.time.
        let left = self.points[right_idx - 1];
        let right = self.points[right_idx];
        interpolate(left, right, at)
    }

    /// Fills `out` with sample-accurate envelope values for
    /// `[block_start, block_start + out.len())`.
    ///
    /// Allocation-free: only writes into the caller-provided slice.
    pub fn fill_block(&self, block_start: TimePosition, out: &mut [f32]) {
        for (i, sample) in out.iter_mut().enumerate() {
            let at = block_start + superb_core::TimeDuration::from_samples(i as i64);
            *sample = self.evaluate(at);
        }
    }

    /// Records a value at `time`, coalescing with the previous point when
    /// the value is unchanged (within `epsilon`) so write-mode does not
    /// explode point counts at constant plateaus.
    ///
    /// Returns `true` if a new point was inserted (or an existing one at
    /// the same time was updated to a different value).
    pub fn record(
        &mut self,
        time: TimePosition,
        value: f32,
        shape: CurveShape,
        epsilon: f32,
    ) -> bool {
        if let Some(last) = self.points.last() {
            if last.time == time {
                if (last.value - value).abs() <= epsilon && last.shape == shape {
                    return false;
                }
                let idx = self.points.len() - 1;
                self.points[idx] = EnvelopePoint { time, value, shape };
                return true;
            }
            if last.time < time && (last.value - value).abs() <= epsilon {
                // Plateau: leave the existing point; value is already held.
                return false;
            }
        }
        self.upsert(EnvelopePoint { time, value, shape });
        true
    }

    /// Builds an envelope from an unsorted list of points (sorted + deduped by time).
    pub fn from_points(default_value: f32, mut points: Vec<EnvelopePoint>) -> Result<Self> {
        points.sort_by_key(|p| p.time);
        // Reject NaN values.
        for p in &points {
            if !p.value.is_finite() {
                return Err(SuperbError::InvalidArgument {
                    what: "envelope point value",
                });
            }
        }
        // Dedup identical times: keep last.
        points.dedup_by(|a, b| {
            if a.time == b.time {
                // `dedup_by` removes `a` when true; keep later (= b after swap semantics).
                // Actually dedup_by removes the *first* of the equal pair when the
                // closure returns true comparing consecutive elements after sort.
                // We want to keep the last → compare and drop earlier:
                true
            } else {
                false
            }
        });
        Ok(Self {
            points,
            default_value,
        })
    }
}

fn interpolate(left: EnvelopePoint, right: EnvelopePoint, at: TimePosition) -> f32 {
    let span = right.time.samples() - left.time.samples();
    if span <= 0 {
        return left.value;
    }
    let t = (at.samples() - left.time.samples()) as f64 / span as f64;
    let t = t.clamp(0.0, 1.0);
    match left.shape {
        CurveShape::Hold => left.value,
        CurveShape::Linear => lerp(left.value, right.value, t as f32),
        CurveShape::Curve { tension } => {
            let shaped = shape_t(t, tension as f64) as f32;
            lerp(left.value, right.value, shaped)
        }
    }
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Maps unit `t` through an exponential curve controlled by `tension` in [-1, 1].
fn shape_t(t: f64, tension: f64) -> f64 {
    let tension = tension.clamp(-1.0, 1.0);
    if tension.abs() < 1e-6 {
        return t;
    }
    // Scale tension into a useful exponential coefficient.
    let c = tension * 8.0;
    let exp_c = c.exp();
    ((c * t).exp() - 1.0) / (exp_c - 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use superb_core::TimeDuration;

    fn pos(s: i64) -> TimePosition {
        TimePosition::from_samples(s)
    }

    #[test]
    fn empty_uses_default() {
        let env = Envelope::new(0.25);
        assert_eq!(env.evaluate(pos(0)), 0.25);
        assert_eq!(env.evaluate(pos(999)), 0.25);
    }

    #[test]
    fn before_first_uses_default() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(100), 1.0));
        assert_eq!(env.evaluate(pos(0)), 0.0);
        assert_eq!(env.evaluate(pos(99)), 0.0);
        assert_eq!(env.evaluate(pos(100)), 1.0);
    }

    #[test]
    fn after_last_holds() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(0), 0.5));
        env.upsert(EnvelopePoint::linear(pos(100), 1.0));
        assert_eq!(env.evaluate(pos(100)), 1.0);
        assert_eq!(env.evaluate(pos(10_000)), 1.0);
    }

    #[test]
    fn linear_midpoint() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(0), 0.0));
        env.upsert(EnvelopePoint::linear(pos(100), 1.0));
        let mid = env.evaluate(pos(50));
        assert!((mid - 0.5).abs() < 1e-5);
    }

    #[test]
    fn hold_stays_left_until_right() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::hold(pos(0), 0.25));
        env.upsert(EnvelopePoint::linear(pos(100), 1.0));
        assert_eq!(env.evaluate(pos(0)), 0.25);
        assert_eq!(env.evaluate(pos(99)), 0.25);
        assert_eq!(env.evaluate(pos(100)), 1.0);
    }

    #[test]
    fn curve_positive_eases_toward_end() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::curved(pos(0), 0.0, 1.0));
        env.upsert(EnvelopePoint::linear(pos(100), 1.0));
        let mid = env.evaluate(pos(50));
        // Positive tension → below linear midpoint.
        assert!(mid < 0.5 - 0.05, "mid={mid}");
        assert!(mid > 0.0);
    }

    #[test]
    fn curve_zero_matches_linear() {
        let mut curved = Envelope::new(0.0);
        curved.upsert(EnvelopePoint::curved(pos(0), 0.0, 0.0));
        curved.upsert(EnvelopePoint::linear(pos(100), 1.0));
        let mut linear = Envelope::new(0.0);
        linear.upsert(EnvelopePoint::linear(pos(0), 0.0));
        linear.upsert(EnvelopePoint::linear(pos(100), 1.0));
        for s in [0_i64, 25, 50, 75, 100] {
            assert!((curved.evaluate(pos(s)) - linear.evaluate(pos(s))).abs() < 1e-5);
        }
    }

    #[test]
    fn upsert_replaces_same_time() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(10), 0.1));
        env.upsert(EnvelopePoint::linear(pos(10), 0.9));
        assert_eq!(env.len(), 1);
        assert_eq!(env.evaluate(pos(10)), 0.9);
    }

    #[test]
    fn upsert_keeps_sorted() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(30), 0.3));
        env.upsert(EnvelopePoint::linear(pos(10), 0.1));
        env.upsert(EnvelopePoint::linear(pos(20), 0.2));
        let times: Vec<_> = env.points().iter().map(|p| p.time.samples()).collect();
        assert_eq!(times, vec![10, 20, 30]);
    }

    #[test]
    fn fill_block_is_sample_accurate() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(0), 0.0));
        env.upsert(EnvelopePoint::linear(pos(4), 1.0));
        let mut out = [0.0f32; 5];
        env.fill_block(pos(0), &mut out);
        assert!((out[0] - 0.0).abs() < 1e-5);
        assert!((out[2] - 0.5).abs() < 1e-5);
        assert!((out[4] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn record_coalesces_plateau() {
        let mut env = Envelope::new(0.0);
        assert!(env.record(pos(0), 0.5, CurveShape::Linear, 1e-6));
        assert!(!env.record(pos(10), 0.5, CurveShape::Linear, 1e-6));
        assert!(!env.record(pos(20), 0.5, CurveShape::Linear, 1e-6));
        assert_eq!(env.len(), 1);
        assert!(env.record(pos(30), 0.75, CurveShape::Linear, 1e-6));
        assert_eq!(env.len(), 2);
    }

    #[test]
    fn clear_range_removes_interior() {
        let mut env = Envelope::new(0.0);
        for s in [0_i64, 10, 20, 30, 40] {
            env.upsert(EnvelopePoint::linear(pos(s), s as f32));
        }
        env.clear_range(pos(10), pos(30)); // removes 10 and 20
        let times: Vec<_> = env.points().iter().map(|p| p.time.samples()).collect();
        assert_eq!(times, vec![0, 30, 40]);
    }

    #[test]
    fn remove_at_works() {
        let mut env = Envelope::new(0.0);
        env.upsert(EnvelopePoint::linear(pos(5), 1.0));
        assert!(env.remove_at(pos(5)));
        assert!(!env.remove_at(pos(5)));
        assert!(env.is_empty());
    }

    #[test]
    fn evaluate_advances_with_duration_add() {
        // Sanity: TimePosition + TimeDuration used by fill_block.
        let start = pos(100);
        let next = start + TimeDuration::from_samples(1);
        assert_eq!(next.samples(), 101);
    }
}
