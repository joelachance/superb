//! Channel layouts.

use core::fmt;

/// Describes how many channels a buffer or node carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ChannelLayout {
    /// A single channel.
    Mono,
    /// Left/right pair.
    #[default]
    Stereo,
    /// An arbitrary channel count (may be 1 or 2 as well; use
    /// [`ChannelLayout::from_count`] to normalise).
    Multi(u16),
}

impl ChannelLayout {
    /// Returns the canonical layout for a channel count.
    pub fn from_count(channels: usize) -> Self {
        match channels {
            1 => Self::Mono,
            2 => Self::Stereo,
            n => Self::Multi(n as u16),
        }
    }

    /// Number of channels.
    pub fn channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Multi(n) => n as usize,
        }
    }
}

impl fmt::Display for ChannelLayout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mono => f.write_str("mono"),
            Self::Stereo => f.write_str("stereo"),
            Self::Multi(n) => write!(f, "{n}ch"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_common_counts() {
        assert_eq!(ChannelLayout::from_count(1), ChannelLayout::Mono);
        assert_eq!(ChannelLayout::from_count(2), ChannelLayout::Stereo);
        assert_eq!(ChannelLayout::from_count(6), ChannelLayout::Multi(6));
        assert_eq!(ChannelLayout::Multi(2).channels(), 2);
        assert_eq!(ChannelLayout::Multi(6).to_string(), "6ch");
        assert_eq!(ChannelLayout::default().channels(), 2);
    }
}
