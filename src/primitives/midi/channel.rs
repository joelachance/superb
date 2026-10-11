//! MIDI channel newtype.

use core::fmt;

use crate::primitives::{AuraError, Result};

/// A MIDI channel, stored internally as a 0-based index (`0..=15`).
///
/// MIDI wire values and most hardware/software UIs number channels `1..=16`;
/// [`MidiChannel::number`] and the [`fmt::Display`] impl use that 1-based
/// convention, while [`MidiChannel::index`] returns the raw 0-based value
/// used in status bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MidiChannel(u8);

impl MidiChannel {
    /// Creates a channel from a 0-based index in `0..=15`.
    ///
    /// # Errors
    /// Returns [`AuraError::InvalidArgument`] if `index > 15`.
    pub const fn new(index: u8) -> Result<Self> {
        if index > 15 {
            Err(AuraError::InvalidArgument {
                what: "midi_channel",
            })
        } else {
            Ok(Self(index))
        }
    }

    /// The 0-based channel index (`0..=15`), matching the low nibble of a
    /// MIDI status byte.
    pub const fn index(self) -> u8 {
        self.0
    }

    /// The 1-based channel number (`1..=16`), matching the convention used
    /// by most DAWs and hardware.
    pub const fn number(self) -> u8 {
        self.0 + 1
    }
}

impl fmt::Display for MidiChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_full_range() {
        for i in 0..=15u8 {
            let ch = MidiChannel::new(i).unwrap();
            assert_eq!(ch.index(), i);
            assert_eq!(ch.number(), i + 1);
        }
    }

    #[test]
    fn rejects_out_of_range() {
        assert_eq!(
            MidiChannel::new(16),
            Err(AuraError::InvalidArgument {
                what: "midi_channel"
            })
        );
        assert!(MidiChannel::new(255).is_err());
    }

    #[test]
    fn display_is_one_based() {
        assert_eq!(MidiChannel::new(0).unwrap().to_string(), "1");
        assert_eq!(MidiChannel::new(15).unwrap().to_string(), "16");
    }

    #[test]
    fn ordering_matches_index() {
        let a = MidiChannel::new(1).unwrap();
        let b = MidiChannel::new(2).unwrap();
        assert!(a < b);
    }
}
