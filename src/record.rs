//! Recording: arm tracks, capture input, turn the take into a clip.
//!
//! (Verbs are added below as the flow is built up.)

use crate::primitives::io::capture::CaptureConsumer;
use crate::primitives::{TimePosition, TrackId};

/// A take in progress.
pub struct Recording {
    /// Captured blocks arriving from the input.
    pub blocks: CaptureConsumer,
    /// Where on the timeline the take started.
    pub punch_in: TimePosition,
    /// Tracks that were armed when recording started; each gets a clip.
    pub tracks: Vec<TrackId>,
    /// Channels in the captured blocks.
    pub channels: usize,
    /// The input stream keeping the capture alive, when recording from hardware.
    #[cfg(feature = "devices")]
    pub stream: Option<crate::primitives::io::device::InputStream>,
}
