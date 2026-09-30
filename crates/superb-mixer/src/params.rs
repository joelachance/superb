//! Lock-free parameter blocks shared between control and audio threads.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;

use superb_buffers::gain::{db_to_linear, linear_to_db};
use superb_rt::AtomicF32;

/// How a pan position maps to left/right gains.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum PanLaw {
    /// `L = cos(θ)`, `R = sin(θ)`: power is constant across the sweep and
    /// the centre sits at −3 dB. The usual choice for mono-in-stereo.
    #[default]
    ConstantPower = 0,
    /// Centre is unity; panning attenuates the opposite side linearly.
    /// The usual choice for stereo material ("balance").
    Balance = 1,
}

impl PanLaw {
    /// Left/right gain multipliers for `pan` in `-1.0..=1.0`.
    pub fn gains(self, pan: f32) -> (f32, f32) {
        let p = pan.clamp(-1.0, 1.0);
        match self {
            PanLaw::ConstantPower => {
                let theta = (p + 1.0) * 0.5 * core::f32::consts::FRAC_PI_2;
                (theta.cos(), theta.sin())
            }
            PanLaw::Balance => {
                if p <= 0.0 {
                    (1.0, 1.0 + p)
                } else {
                    (1.0 - p, 1.0)
                }
            }
        }
    }

    fn from_u8(v: u8) -> Self {
        if v == 1 {
            PanLaw::Balance
        } else {
            PanLaw::ConstantPower
        }
    }
}

/// Number of channels that are metered per strip.
pub const METER_CHANNELS: usize = 2;

/// Control surface of a channel strip (track or bus).
///
/// All setters are safe from any thread; the audio thread only loads.
#[derive(Debug)]
pub struct StripParams {
    gain: AtomicF32,
    pan: AtomicF32,
    mute: AtomicBool,
    solo: AtomicBool,
    pan_law: AtomicU8,
    peaks: [AtomicF32; METER_CHANNELS],
    /// Shared count of soloed tracks in the mixer. `None` for strips that
    /// solo does not affect (buses, master).
    solo_count: Option<Arc<AtomicUsize>>,
}

impl StripParams {
    /// Creates unity-gain, centred, unmuted parameters.
    ///
    /// `solo_count` links the strip into the mixer's solo logic; pass
    /// `None` for buses.
    pub fn new(solo_count: Option<Arc<AtomicUsize>>) -> Self {
        Self {
            gain: AtomicF32::new(1.0),
            pan: AtomicF32::new(0.0),
            mute: AtomicBool::new(false),
            solo: AtomicBool::new(false),
            pan_law: AtomicU8::new(PanLaw::default() as u8),
            peaks: [AtomicF32::new(0.0), AtomicF32::new(0.0)],
            solo_count,
        }
    }

    /// Linear gain.
    pub fn gain(&self) -> f32 {
        self.gain.get()
    }

    /// Sets linear gain (negative values are clamped to 0).
    pub fn set_gain(&self, gain: f32) {
        self.gain.set(gain.max(0.0));
    }

    /// Gain in decibels.
    pub fn gain_db(&self) -> f32 {
        linear_to_db(self.gain())
    }

    /// Sets gain in decibels.
    pub fn set_gain_db(&self, db: f32) {
        self.set_gain(db_to_linear(db));
    }

    /// Pan position, `-1.0` (left) to `1.0` (right).
    pub fn pan(&self) -> f32 {
        self.pan.get()
    }

    /// Sets the pan position (clamped).
    pub fn set_pan(&self, pan: f32) {
        self.pan.set(pan.clamp(-1.0, 1.0));
    }

    /// The pan law in use.
    pub fn pan_law(&self) -> PanLaw {
        PanLaw::from_u8(self.pan_law.load(Ordering::Relaxed))
    }

    /// Changes the pan law.
    pub fn set_pan_law(&self, law: PanLaw) {
        self.pan_law.store(law as u8, Ordering::Relaxed);
    }

    /// `true` if muted.
    pub fn is_muted(&self) -> bool {
        self.mute.load(Ordering::Relaxed)
    }

    /// Mutes or unmutes.
    pub fn set_mute(&self, mute: bool) {
        self.mute.store(mute, Ordering::Relaxed);
    }

    /// `true` if soloed.
    pub fn is_solo(&self) -> bool {
        self.solo.load(Ordering::Relaxed)
    }

    /// Solos or un-solos. Keeps the mixer-wide solo count consistent.
    pub fn set_solo(&self, solo: bool) {
        let was = self.solo.swap(solo, Ordering::AcqRel);
        if let Some(count) = &self.solo_count {
            if solo && !was {
                count.fetch_add(1, Ordering::AcqRel);
            } else if !solo && was {
                count.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }

    /// `true` if solo logic applies to this strip (tracks) — buses return `false`.
    pub fn is_solo_affected(&self) -> bool {
        self.solo_count.is_some()
    }

    /// `true` if the strip is currently silenced by mute or by another
    /// track's solo.
    pub fn is_effectively_muted(&self) -> bool {
        if self.is_muted() {
            return true;
        }
        match &self.solo_count {
            Some(count) => count.load(Ordering::Acquire) > 0 && !self.is_solo(),
            None => false,
        }
    }

    /// Effective linear gain after mute/solo.
    pub fn effective_gain(&self) -> f32 {
        if self.is_effectively_muted() {
            0.0
        } else {
            self.gain()
        }
    }

    /// Peak-hold meter value for `channel` (0 = left, 1 = right).
    pub fn peak(&self, channel: usize) -> f32 {
        self.peaks.get(channel).map_or(0.0, |p| p.get())
    }

    /// Reads and resets the peak hold for `channel`.
    pub fn take_peak(&self, channel: usize) -> f32 {
        self.peaks
            .get(channel)
            .map_or(0.0, |p| p.swap(0.0, Ordering::Relaxed))
    }

    /// Raises the peak hold for `channel` (called by the audio thread).
    pub(crate) fn raise_peak(&self, channel: usize, value: f32) {
        if let Some(p) = self.peaks.get(channel) {
            p.fetch_max(value, Ordering::Relaxed);
        }
    }
}

/// Control surface of a send.
#[derive(Debug)]
pub struct SendParams {
    level: AtomicF32,
    enabled: AtomicBool,
}

impl SendParams {
    /// Creates an enabled send at `level` (linear).
    pub fn new(level: f32) -> Self {
        Self {
            level: AtomicF32::new(level.max(0.0)),
            enabled: AtomicBool::new(true),
        }
    }

    /// Linear send level.
    pub fn level(&self) -> f32 {
        self.level.get()
    }

    /// Sets the linear send level (clamped at 0).
    pub fn set_level(&self, level: f32) {
        self.level.set(level.max(0.0));
    }

    /// Send level in dB.
    pub fn level_db(&self) -> f32 {
        linear_to_db(self.level())
    }

    /// Sets the send level in dB.
    pub fn set_level_db(&self, db: f32) {
        self.set_level(db_to_linear(db));
    }

    /// `true` if the send passes signal.
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Enables or disables the send.
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    /// Level after the enable switch.
    pub fn effective_level(&self) -> f32 {
        if self.is_enabled() {
            self.level()
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_power_law_keeps_power_and_hits_rails() {
        for i in 0..=20 {
            let pan = -1.0 + i as f32 * 0.1;
            let (l, r) = PanLaw::ConstantPower.gains(pan);
            assert!((l * l + r * r - 1.0).abs() < 1e-5, "pan {pan}");
        }
        let (l, r) = PanLaw::ConstantPower.gains(0.0);
        assert!((l - core::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((l - r).abs() < 1e-6);
        let (l, r) = PanLaw::ConstantPower.gains(-1.0);
        assert!((l - 1.0).abs() < 1e-6 && r.abs() < 1e-6);
        let (l, r) = PanLaw::ConstantPower.gains(5.0); // clamped
        assert!(l.abs() < 1e-6 && (r - 1.0).abs() < 1e-6);
    }

    #[test]
    fn balance_law_is_unity_at_centre() {
        assert_eq!(PanLaw::Balance.gains(0.0), (1.0, 1.0));
        assert_eq!(PanLaw::Balance.gains(-1.0), (1.0, 0.0));
        assert_eq!(PanLaw::Balance.gains(1.0), (0.0, 1.0));
        assert_eq!(PanLaw::Balance.gains(0.5), (0.5, 1.0));
        assert_eq!(PanLaw::default(), PanLaw::ConstantPower);
    }

    #[test]
    fn gain_setters_in_db_and_linear() {
        let p = StripParams::new(None);
        assert_eq!(p.gain(), 1.0);
        assert_eq!(p.gain_db(), 0.0);
        p.set_gain_db(-6.0206);
        assert!((p.gain() - 0.5).abs() < 1e-4);
        p.set_gain(-3.0);
        assert_eq!(p.gain(), 0.0);
        p.set_pan(2.0);
        assert_eq!(p.pan(), 1.0);
        p.set_pan_law(PanLaw::Balance);
        assert_eq!(p.pan_law(), PanLaw::Balance);
    }

    #[test]
    fn solo_logic_uses_shared_count() {
        let count = Arc::new(AtomicUsize::new(0));
        let a = StripParams::new(Some(Arc::clone(&count)));
        let b = StripParams::new(Some(Arc::clone(&count)));
        let bus = StripParams::new(None);
        assert!(!a.is_effectively_muted() && !b.is_effectively_muted());
        a.set_solo(true);
        a.set_solo(true); // idempotent
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(!a.is_effectively_muted());
        assert!(b.is_effectively_muted());
        assert!(!bus.is_effectively_muted(), "buses ignore solo");
        assert_eq!(b.effective_gain(), 0.0);
        b.set_solo(true);
        assert!(!b.is_effectively_muted());
        a.set_solo(false);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(a.is_effectively_muted());
        b.set_solo(false);
        assert_eq!(count.load(Ordering::Relaxed), 0);
        assert!(!a.is_effectively_muted());
        a.set_mute(true);
        assert!(a.is_effectively_muted());
        assert!(a.is_solo_affected() && !bus.is_solo_affected());
    }

    #[test]
    fn meters_hold_and_reset() {
        let p = StripParams::new(None);
        p.raise_peak(0, 0.5);
        p.raise_peak(0, 0.25);
        p.raise_peak(1, 0.75);
        p.raise_peak(7, 1.0); // ignored
        assert_eq!(p.peak(0), 0.5);
        assert_eq!(p.peak(1), 0.75);
        assert_eq!(p.peak(7), 0.0);
        assert_eq!(p.take_peak(0), 0.5);
        assert_eq!(p.peak(0), 0.0);
    }

    #[test]
    fn send_params() {
        let s = SendParams::new(0.5);
        assert_eq!(s.effective_level(), 0.5);
        s.set_enabled(false);
        assert_eq!(s.effective_level(), 0.0);
        assert!(!s.is_enabled());
        s.set_level_db(0.0);
        assert_eq!(s.level(), 1.0);
        assert_eq!(s.level_db(), 0.0);
        s.set_level(-1.0);
        assert_eq!(s.level(), 0.0);
    }
}
