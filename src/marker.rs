//! Markers: named places on the timeline.
//!
//! A marker is a name at a position, nothing more: it plays nothing and
//! moves nothing. Add one, move it, rename it, remove it; every change is
//! an undo step, and nothing here rebuilds the mixer.
//!
//! Made of `superb::primitives::session::Marker` and its commands.

use crate::primitives::session::commands::{self, AddMarker, MoveMarker, RemoveMarker};
use crate::primitives::session::Marker;
use crate::primitives::{MarkerId, Result, TimePosition};
use crate::session::Session;

/// Drops a named marker at `at`. One undo step.
///
/// Made of: `Memory::allocate_marker_id` → `Marker::with_id` → `AddMarker`
/// through `session::edit`.
pub fn add(session: &mut Session, name: &str, at: TimePosition) -> Result<MarkerId> {
    let id = session.memory.allocate_marker_id();
    crate::session::edit(session, AddMarker::new(Marker::with_id(id, name, at)))?;
    Ok(id)
}

/// Moves a marker to `at`. Moves within [`Session::COALESCE_WINDOW`] are
/// one undo step, so a drag undoes in one.
///
/// Made of: `MoveMarker` through `session::edit`.
pub fn move_to(session: &mut Session, marker: MarkerId, at: TimePosition) -> Result<()> {
    crate::session::edit(session, MoveMarker::new(marker, at))
}

/// Renames a marker. Quick successive renames of one marker are one undo
/// step.
///
/// Made of: `set_marker_name` through `session::edit`.
pub fn rename(session: &mut Session, marker: MarkerId, name: &str) -> Result<()> {
    crate::session::edit(session, commands::set_marker_name(marker, name))
}

/// Removes a marker; undo puts it back under its id.
///
/// Made of: `RemoveMarker` through `session::edit`.
pub fn remove(session: &mut Session, marker: MarkerId) -> Result<()> {
    crate::session::edit(session, RemoveMarker::new(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::SampleRate;
    use crate::session;

    #[test]
    fn markers_are_added_moved_renamed_and_removed_with_undo() {
        let mut session =
            session::create(session::scratch("markers"), SampleRate::HZ_48K, 64).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        assert!(!session.needs_rebuild_for_test());
        let steps = session.history.undo_len();
        let verse = add(&mut session, "verse", TimePosition::from_samples(100)).unwrap();
        assert_eq!(session.memory.markers().len(), 1);
        assert_eq!(session.history.undo_len(), steps + 1);
        assert!(!session.needs_rebuild_for_test(), "a marker never rebuilds");

        // A drag is one step.
        for at in [200, 300, 400] {
            move_to(&mut session, verse, TimePosition::from_samples(at)).unwrap();
        }
        assert_eq!(session.history.undo_len(), steps + 2);
        assert_eq!(
            session.memory.marker(verse).unwrap().position(),
            TimePosition::from_samples(400)
        );
        rename(&mut session, verse, "chorus").unwrap();
        assert_eq!(session.memory.marker(verse).unwrap().name, "chorus");
        assert_eq!(session.history.undo_len(), steps + 3);

        remove(&mut session, verse).unwrap();
        assert!(session.memory.marker(verse).is_err());
        assert!(session::undo(&mut session).unwrap());
        let back = session.memory.marker(verse).unwrap();
        assert_eq!(back.name, "chorus");
        assert_eq!(back.position(), TimePosition::from_samples(400));
        assert!(session::undo(&mut session).unwrap());
        assert_eq!(session.memory.marker(verse).unwrap().name, "verse");
        assert!(session::undo(&mut session).unwrap());
        assert_eq!(
            session.memory.marker(verse).unwrap().position(),
            TimePosition::from_samples(100),
            "the whole drag undone"
        );
        assert!(remove(&mut session, MarkerId::from_raw(999)).is_err());
    }
}
