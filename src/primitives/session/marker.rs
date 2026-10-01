//! Timeline markers.

use crate::primitives::{MarkerId, TimePosition};

/// A named position on the timeline.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub(crate) id: MarkerId,
    /// Display name.
    pub name: String,
    pub(crate) position: TimePosition,
    /// Display colour as `0xRRGGBB`.
    pub color: Option<u32>,
}

impl Marker {
    /// Creates a marker with no ID yet.
    pub fn new(name: impl Into<String>, position: TimePosition) -> Self {
        Self::with_id(MarkerId::INVALID, name, position)
    }

    /// Creates a marker with a pre-allocated ID.
    pub fn with_id(id: MarkerId, name: impl Into<String>, position: TimePosition) -> Self {
        Self {
            id,
            name: name.into(),
            position,
            color: None,
        }
    }

    /// The marker's ID (`MarkerId::INVALID` until added to a document).
    pub fn id(&self) -> MarkerId {
        self.id
    }

    /// Where the marker sits.
    pub fn position(&self) -> TimePosition {
        self.position
    }
}
