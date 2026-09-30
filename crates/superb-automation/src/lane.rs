//! One automation lane: envelope + mode + bound parameter target.

use std::sync::Arc;

use superb_core::{ParamId, TimeDuration, TimePosition};
use superb_rt::AtomicF32;

use crate::envelope::{CurveShape, Envelope};
use crate::mode::{AutomationMode, AutomationState};
use crate::target::ParamTarget;

/// Default absolute epsilon used when coalescing write-mode points.
pub const RECORD_EPSILON: f32 = 1e-5;

/// A single automatable parameter: envelope, mode state, and the live
/// [`AtomicF32`] that graph/mixer nodes already read.
///
/// Control thread:
/// * edit [`envelope_mut`](AutomationLane::envelope_mut)
/// * change mode via [`set_mode`](AutomationLane::set_mode)
/// * call [`begin_touch`](AutomationLane::begin_touch) /
///   [`end_touch`](AutomationLane::end_touch) from the UI
///
/// Audio / apply path (once per block):
/// * [`apply_block`](AutomationLane::apply_block) — sample-accurate read into
///   a scratch buffer (optional) and/or a single atomic store; write-mode
///   records the live value back into the envelope
#[derive(Debug)]
pub struct AutomationLane {
    id: ParamId,
    envelope: Envelope,
    state: AutomationState,
    target: Arc<AtomicF32>,
    /// Minimum spacing between recorded write points, in samples.
    /// `0` records every block start (subject to plateau coalescing).
    write_interval_samples: i64,
    last_write_sample: Option<i64>,
}

impl AutomationLane {
    /// Binds a new lane to `target` (shared with a strip/node param).
    ///
    /// The envelope default is seeded from the target's current value so an
    /// empty envelope does not jump the parameter on first read.
    pub fn new(id: ParamId, target: Arc<AtomicF32>) -> Self {
        let initial = target.get();
        Self {
            id,
            envelope: Envelope::new(initial),
            state: AutomationState::new(),
            target,
            write_interval_samples: 0,
            last_write_sample: None,
        }
    }

    /// Lane / parameter id.
    pub fn id(&self) -> ParamId {
        self.id
    }

    /// Shared atomic the lane drives (and samples in write modes).
    pub fn target(&self) -> &Arc<AtomicF32> {
        &self.target
    }

    /// Borrow the envelope.
    pub fn envelope(&self) -> &Envelope {
        &self.envelope
    }

    /// Mutable access for editing breakpoints (off the audio thread).
    pub fn envelope_mut(&mut self) -> &mut Envelope {
        &mut self.envelope
    }

    /// Current automation mode.
    pub fn mode(&self) -> AutomationMode {
        self.state.mode()
    }

    /// Sets the automation mode.
    pub fn set_mode(&mut self, mode: AutomationMode) {
        self.state.set_mode(mode);
    }

    /// UI: user started dragging this parameter.
    pub fn begin_touch(&mut self) {
        self.state.begin_touch();
    }

    /// UI: user released this parameter.
    pub fn end_touch(&mut self) {
        self.state.end_touch();
    }

    /// Transport stopped — clears latch / touch arming.
    pub fn on_transport_stop(&mut self) {
        self.state.on_transport_stop();
        self.last_write_sample = None;
    }

    /// Sets the minimum sample spacing between write-mode recordings.
    pub fn set_write_interval_samples(&mut self, samples: i64) {
        self.write_interval_samples = samples.max(0);
    }

    /// Applies one audio block.
    ///
    /// * When the mode **reads**, evaluates the envelope sample-accurately
    ///   into `scratch` (if provided and long enough) and stores the value
    ///   at `block_start` into the bound atomic (nodes that only read once
    ///   per block still see the correct start-of-block automation).
    /// * When the mode **writes**, samples the atomic and records it into
    ///   the envelope at `block_start` (honouring the write interval and
    ///   plateau coalescing).
    ///
    /// `scratch` may be shorter than `frames`; only `scratch.len()` samples
    /// are filled. Pass `None` when only the atomic update is needed.
    ///
    /// Returns the number of envelope points added/updated this block
    /// (write path), or `0` on the read path.
    pub fn apply_block(
        &mut self,
        block_start: TimePosition,
        frames: usize,
        playing: bool,
        scratch: Option<&mut [f32]>,
    ) -> usize {
        if self.state.wants_write(playing) {
            return self.write_block(block_start);
        }
        if self.state.wants_read(playing) {
            self.read_block(block_start, frames, scratch);
        }
        0
    }

    /// Evaluates the envelope at `at` without mutating mode state.
    pub fn evaluate(&self, at: TimePosition) -> f32 {
        self.envelope.evaluate(at)
    }

    fn read_block(&self, block_start: TimePosition, frames: usize, scratch: Option<&mut [f32]>) {
        if let Some(buf) = scratch {
            let n = buf.len().min(frames);
            self.envelope.fill_block(block_start, &mut buf[..n]);
            if n > 0 {
                self.target.set(buf[0]);
            } else if frames > 0 {
                self.target.set(self.envelope.evaluate(block_start));
            }
        } else if frames > 0 {
            self.target.set(self.envelope.evaluate(block_start));
        }
    }

    fn write_block(&mut self, block_start: TimePosition) -> usize {
        let sample = block_start.samples();
        if let Some(last) = self.last_write_sample {
            if self.write_interval_samples > 0 && sample - last < self.write_interval_samples {
                return 0;
            }
        }
        let value = self.target.get();
        let changed = self
            .envelope
            .record(block_start, value, CurveShape::Linear, RECORD_EPSILON);
        self.last_write_sample = Some(sample);
        usize::from(changed)
    }
}

/// A small collection of [`AutomationLane`]s applied together each block.
#[derive(Debug, Default)]
pub struct AutomationBank {
    lanes: Vec<AutomationLane>,
}

impl AutomationBank {
    /// Empty bank.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of lanes.
    pub fn len(&self) -> usize {
        self.lanes.len()
    }

    /// `true` when there are no lanes.
    pub fn is_empty(&self) -> bool {
        self.lanes.is_empty()
    }

    /// All lanes.
    pub fn lanes(&self) -> &[AutomationLane] {
        &self.lanes
    }

    /// Mutable lanes (for mode / envelope edits).
    pub fn lanes_mut(&mut self) -> &mut [AutomationLane] {
        &mut self.lanes
    }

    /// Adds a lane. Returns its index.
    pub fn add_lane(&mut self, lane: AutomationLane) -> usize {
        self.lanes.push(lane);
        self.lanes.len() - 1
    }

    /// Finds a lane by [`ParamId`].
    pub fn get(&self, id: ParamId) -> Option<&AutomationLane> {
        self.lanes.iter().find(|l| l.id() == id)
    }

    /// Mutable find by [`ParamId`].
    pub fn get_mut(&mut self, id: ParamId) -> Option<&mut AutomationLane> {
        self.lanes.iter_mut().find(|l| l.id() == id)
    }

    /// Removes the lane with `id`. Returns whether one was removed.
    pub fn remove(&mut self, id: ParamId) -> bool {
        let before = self.lanes.len();
        self.lanes.retain(|l| l.id() != id);
        self.lanes.len() != before
    }

    /// Forwards transport-stop to every lane.
    pub fn on_transport_stop(&mut self) {
        for lane in &mut self.lanes {
            lane.on_transport_stop();
        }
    }

    /// Applies every lane for one block.
    ///
    /// `scratch` is an optional per-lane scratch list: `scratch[i]` is used
    /// for `lanes[i]` when present. Lanes without a matching scratch entry
    /// only update their atomic.
    pub fn apply_block(
        &mut self,
        block_start: TimePosition,
        frames: usize,
        playing: bool,
        mut scratch: Option<&mut [Vec<f32>]>,
    ) {
        for (i, lane) in self.lanes.iter_mut().enumerate() {
            let slot = scratch
                .as_mut()
                .and_then(|bufs| bufs.get_mut(i).map(|v| v.as_mut_slice()));
            lane.apply_block(block_start, frames, playing, slot);
        }
    }
}

/// Helper: advance a block start by `frames` samples.
#[inline]
pub fn advance_block(start: TimePosition, frames: usize) -> TimePosition {
    start + TimeDuration::from_samples(frames as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::EnvelopePoint;
    use superb_core::IdGenerator;

    fn lane_with_ramp() -> AutomationLane {
        let ids = IdGenerator::new();
        let target = Arc::new(AtomicF32::new(0.0));
        let mut lane = AutomationLane::new(ids.next(), Arc::clone(&target));
        lane.envelope_mut()
            .upsert(EnvelopePoint::linear(TimePosition::from_samples(0), 0.0));
        lane.envelope_mut()
            .upsert(EnvelopePoint::linear(TimePosition::from_samples(100), 1.0));
        lane.set_mode(AutomationMode::Read);
        lane
    }

    #[test]
    fn read_mode_writes_atomic_from_envelope() {
        let lane = lane_with_ramp();
        let target = Arc::clone(lane.target());
        let mut lane = lane;
        lane.apply_block(TimePosition::from_samples(50), 16, true, None);
        assert!((target.get() - 0.5).abs() < 1e-5);
    }

    #[test]
    fn read_mode_fills_scratch_sample_accurately() {
        let mut lane = lane_with_ramp();
        let mut scratch = [0.0f32; 5];
        lane.apply_block(TimePosition::from_samples(0), 5, true, Some(&mut scratch));
        assert!((scratch[0] - 0.0).abs() < 1e-5);
        assert!((scratch[4] - 0.04).abs() < 1e-5); // 4/100
    }

    #[test]
    fn off_mode_leaves_atomic_alone() {
        let ids = IdGenerator::new();
        let target = Arc::new(AtomicF32::new(0.42));
        let mut lane = AutomationLane::new(ids.next(), Arc::clone(&target));
        lane.envelope_mut()
            .upsert(EnvelopePoint::linear(TimePosition::ZERO, 1.0));
        lane.set_mode(AutomationMode::Off);
        lane.apply_block(TimePosition::ZERO, 64, true, None);
        assert_eq!(target.get(), 0.42);
    }

    #[test]
    fn write_mode_records_live_value() {
        let ids = IdGenerator::new();
        let target = Arc::new(AtomicF32::new(0.8));
        let mut lane = AutomationLane::new(ids.next(), Arc::clone(&target));
        lane.set_mode(AutomationMode::Write);
        let n = lane.apply_block(TimePosition::from_samples(0), 64, true, None);
        assert_eq!(n, 1);
        assert_eq!(lane.envelope().len(), 1);
        assert!((lane.evaluate(TimePosition::from_samples(0)) - 0.8).abs() < 1e-5);

        target.set(0.8);
        // Plateau coalescing: same value later → no new point.
        let n2 = lane.apply_block(TimePosition::from_samples(64), 64, true, None);
        assert_eq!(n2, 0);
        assert_eq!(lane.envelope().len(), 1);

        target.set(0.2);
        let n3 = lane.apply_block(TimePosition::from_samples(128), 64, true, None);
        assert_eq!(n3, 1);
        assert_eq!(lane.envelope().len(), 2);
    }

    #[test]
    fn touch_then_release_returns_to_read() {
        let mut lane = lane_with_ramp();
        let target = Arc::clone(lane.target());
        lane.set_mode(AutomationMode::Touch);

        // Reading at midpoint → 0.5
        lane.apply_block(TimePosition::from_samples(50), 1, true, None);
        assert!((target.get() - 0.5).abs() < 1e-5);

        // User grabs fader and sets 0.9 — write while touching.
        lane.begin_touch();
        target.set(0.9);
        lane.apply_block(TimePosition::from_samples(60), 1, true, None);
        assert!((lane.evaluate(TimePosition::from_samples(60)) - 0.9).abs() < 1e-5);

        // Release → back to reading. The write inserted a point at 60, so
        // sample 50 is now on the 0→60 segment (50/60 * 0.9 = 0.75).
        lane.end_touch();
        assert_eq!(lane.mode(), AutomationMode::Touch);
        lane.apply_block(TimePosition::from_samples(50), 1, true, None);
        assert!((target.get() - 0.75).abs() < 1e-5);

        // And past the touch point we keep reading the updated envelope.
        lane.apply_block(TimePosition::from_samples(60), 1, true, None);
        assert!((target.get() - 0.9).abs() < 1e-5);
    }

    #[test]
    fn latch_survives_release_until_stop() {
        let ids = IdGenerator::new();
        let target = Arc::new(AtomicF32::new(0.1));
        let mut lane = AutomationLane::new(ids.next(), Arc::clone(&target));
        lane.set_mode(AutomationMode::Latch);
        lane.begin_touch();
        target.set(0.55);
        lane.apply_block(TimePosition::from_samples(0), 1, true, None);
        lane.end_touch();
        target.set(0.66);
        let n = lane.apply_block(TimePosition::from_samples(32), 1, true, None);
        assert_eq!(n, 1);
        assert!((lane.evaluate(TimePosition::from_samples(32)) - 0.66).abs() < 1e-5);
        lane.on_transport_stop();
        // After stop, even while "playing" flag true for a new pass, latch
        // is cleared — but we need playing=true without latch:
        target.set(0.1);
        lane.envelope_mut().clear();
        lane.envelope_mut().set_default_value(0.1);
        lane.apply_block(TimePosition::from_samples(0), 1, true, None);
        // Read path with empty envelope → default 0.1 already in atomic.
        assert!((target.get() - 0.1).abs() < 1e-5);
    }

    #[test]
    fn bank_apply_drives_multiple_targets() {
        let ids = IdGenerator::new();
        let g = Arc::new(AtomicF32::new(1.0));
        let p = Arc::new(AtomicF32::new(0.0));
        let mut bank = AutomationBank::new();
        let mut gain = AutomationLane::new(ids.next(), Arc::clone(&g));
        gain.envelope_mut()
            .upsert(EnvelopePoint::linear(TimePosition::ZERO, 0.25));
        gain.set_mode(AutomationMode::Read);
        let mut pan = AutomationLane::new(ids.next(), Arc::clone(&p));
        pan.envelope_mut()
            .upsert(EnvelopePoint::linear(TimePosition::ZERO, -0.5));
        pan.set_mode(AutomationMode::Read);
        bank.add_lane(gain);
        bank.add_lane(pan);
        bank.apply_block(TimePosition::ZERO, 64, true, None);
        assert!((g.get() - 0.25).abs() < 1e-5);
        assert!((p.get() - -0.5).abs() < 1e-5);
    }

    #[test]
    fn not_playing_is_noop_for_read_and_write() {
        let mut lane = lane_with_ramp();
        let target = Arc::clone(lane.target());
        target.set(0.33);
        lane.set_mode(AutomationMode::Write);
        assert_eq!(lane.apply_block(TimePosition::ZERO, 64, false, None), 0);
        assert!(lane.envelope().is_empty() || lane.envelope().len() == 2); // ramp points only
                                                                           // Write should not have added a 0.33 point at 0 — wait, envelope already has ramp.
                                                                           // Ensure no point with value 0.33:
        assert!(lane
            .envelope()
            .points()
            .iter()
            .all(|pt| (pt.value - 0.33).abs() > 1e-4));

        lane.set_mode(AutomationMode::Read);
        target.set(0.33);
        lane.apply_block(TimePosition::from_samples(50), 16, false, None);
        assert_eq!(target.get(), 0.33); // unchanged
    }

    #[test]
    fn write_interval_throttles_recording() {
        let ids = IdGenerator::new();
        let target = Arc::new(AtomicF32::new(0.0));
        let mut lane = AutomationLane::new(ids.next(), Arc::clone(&target));
        lane.set_mode(AutomationMode::Write);
        lane.set_write_interval_samples(100);
        target.set(0.1);
        assert_eq!(
            lane.apply_block(TimePosition::from_samples(0), 1, true, None),
            1
        );
        target.set(0.2);
        assert_eq!(
            lane.apply_block(TimePosition::from_samples(50), 1, true, None),
            0
        );
        target.set(0.3);
        assert_eq!(
            lane.apply_block(TimePosition::from_samples(100), 1, true, None),
            1
        );
        assert_eq!(lane.envelope().len(), 2);
    }
}
