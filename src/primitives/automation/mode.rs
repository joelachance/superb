//! Automation read / write modes (DAW-style).

use core::fmt;

/// How an automation lane interacts with its envelope while the transport
/// is rolling.
///
/// Behaviour summary (while **playing**):
///
/// | mode | reads envelope → param | records param → envelope |
/// |---|---|---|
/// | [`Off`](AutomationMode::Off) | no | no |
/// | [`Read`](AutomationMode::Read) | yes | no |
/// | [`Touch`](AutomationMode::Touch) | yes, until touched; then write while held | while touched |
/// | [`Latch`](AutomationMode::Latch) | yes, until first touch; then write until stop | after first touch |
/// | [`Write`](AutomationMode::Write) | no (manual/UI drives param) | continuously |
///
/// When **not playing**, every mode leaves the parameter alone (UI / last
/// value wins) and nothing is recorded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum AutomationMode {
    /// Parameter is manual; envelope is ignored and not written.
    Off = 0,
    /// Envelope drives the parameter; user moves are not recorded.
    #[default]
    Read = 1,
    /// Like [`Read`](AutomationMode::Read) until the user touches the
    /// control; while held, behaves like [`Write`](AutomationMode::Write);
    /// on release, returns to reading.
    Touch = 2,
    /// Like [`Touch`](AutomationMode::Touch), but once touched, keeps
    /// writing until transport stops (even after release).
    Latch = 3,
    /// Continuously records the parameter into the envelope while playing.
    Write = 4,
}

impl AutomationMode {
    /// Human-readable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            AutomationMode::Off => "off",
            AutomationMode::Read => "read",
            AutomationMode::Touch => "touch",
            AutomationMode::Latch => "latch",
            AutomationMode::Write => "write",
        }
    }
}

impl fmt::Display for AutomationMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mutable touch / write state machine that pairs with [`AutomationMode`].
///
/// Owned by an automation lane. Control-thread code calls
/// [`begin_touch`](AutomationState::begin_touch) /
/// [`end_touch`](AutomationState::end_touch); the audio / apply path calls
/// [`on_transport_stop`](AutomationState::on_transport_stop) and
/// [`wants_write`](AutomationState::wants_write) /
/// [`wants_read`](AutomationState::wants_read).
#[derive(Clone, Debug, Default)]
pub struct AutomationState {
    mode: AutomationMode,
    /// User is currently holding the control.
    touching: bool,
    /// Latch has been armed (touched at least once since play started).
    latched: bool,
}

impl AutomationState {
    /// Fresh state in [`AutomationMode::Read`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Fresh state with an explicit mode.
    pub fn with_mode(mode: AutomationMode) -> Self {
        Self {
            mode,
            touching: false,
            latched: false,
        }
    }

    /// Current mode.
    pub fn mode(&self) -> AutomationMode {
        self.mode
    }

    /// Sets the mode. Clears touch/latch arming so a mode change never
    /// leaves a stale write session running.
    pub fn set_mode(&mut self, mode: AutomationMode) {
        self.mode = mode;
        self.touching = false;
        self.latched = false;
    }

    /// `true` while the user is holding the control.
    pub fn is_touching(&self) -> bool {
        self.touching
    }

    /// `true` after a latch has been armed this play pass.
    pub fn is_latched(&self) -> bool {
        self.latched
    }

    /// Control surface: user started moving the parameter.
    pub fn begin_touch(&mut self) {
        self.touching = true;
        if self.mode == AutomationMode::Latch {
            self.latched = true;
        }
    }

    /// Control surface: user released the parameter.
    pub fn end_touch(&mut self) {
        self.touching = false;
    }

    /// Transport stopped (or seeked in a way that ends a write pass).
    /// Clears latch arming and touch.
    pub fn on_transport_stop(&mut self) {
        self.touching = false;
        self.latched = false;
    }

    /// Whether this block should record the live parameter into the envelope.
    pub fn wants_write(&self, playing: bool) -> bool {
        if !playing {
            return false;
        }
        match self.mode {
            AutomationMode::Off | AutomationMode::Read => false,
            AutomationMode::Write => true,
            AutomationMode::Touch => self.touching,
            AutomationMode::Latch => self.latched,
        }
    }

    /// Whether this block should push envelope values into the parameter.
    pub fn wants_read(&self, playing: bool) -> bool {
        if !playing {
            return false;
        }
        match self.mode {
            AutomationMode::Off | AutomationMode::Write => false,
            AutomationMode::Read => true,
            AutomationMode::Touch => !self.touching,
            AutomationMode::Latch => !self.latched,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_always_reads_never_writes() {
        let mut s = AutomationState::with_mode(AutomationMode::Read);
        assert!(s.wants_read(true));
        assert!(!s.wants_write(true));
        s.begin_touch();
        assert!(s.wants_read(true));
        assert!(!s.wants_write(true));
    }

    #[test]
    fn write_always_writes_never_reads_while_playing() {
        let s = AutomationState::with_mode(AutomationMode::Write);
        assert!(!s.wants_read(true));
        assert!(s.wants_write(true));
        assert!(!s.wants_write(false));
    }

    #[test]
    fn touch_writes_only_while_held() {
        let mut s = AutomationState::with_mode(AutomationMode::Touch);
        assert!(s.wants_read(true));
        assert!(!s.wants_write(true));
        s.begin_touch();
        assert!(!s.wants_read(true));
        assert!(s.wants_write(true));
        s.end_touch();
        assert!(s.wants_read(true));
        assert!(!s.wants_write(true));
    }

    #[test]
    fn latch_keeps_writing_after_release_until_stop() {
        let mut s = AutomationState::with_mode(AutomationMode::Latch);
        assert!(s.wants_read(true));
        s.begin_touch();
        assert!(s.wants_write(true));
        s.end_touch();
        assert!(s.wants_write(true));
        assert!(!s.wants_read(true));
        s.on_transport_stop();
        assert!(!s.wants_write(true));
        assert!(s.wants_read(true));
    }

    #[test]
    fn off_does_nothing() {
        let s = AutomationState::with_mode(AutomationMode::Off);
        assert!(!s.wants_read(true));
        assert!(!s.wants_write(true));
    }

    #[test]
    fn set_mode_clears_touch_state() {
        let mut s = AutomationState::with_mode(AutomationMode::Latch);
        s.begin_touch();
        assert!(s.is_latched());
        s.set_mode(AutomationMode::Read);
        assert!(!s.is_touching());
        assert!(!s.is_latched());
    }
}
