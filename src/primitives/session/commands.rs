//! Undoable edits of a [`Session`], for use with
//! [`UndoStack<Session>`](crate::primitives::undo::UndoStack).
//!
//! Structural commands (`AddTrack`, `RemoveClip`, ...) restore removed
//! objects under their original IDs and positions, so IDs held by the UI
//! stay valid across undo/redo. Drag-style commands (`MoveClip`,
//! `ResizeClip`, `MoveMarker`, `SetTempo`, `SetLoopRange` and the
//! property helpers) carry a [`CoalesceKey`] and merge into one step.
//!
//! Allocate IDs up front so the caller knows them before the command is
//! pushed (the stack takes ownership of the command):
//!
//! ```
//! use superb::primitives::{SampleRate, TimeDuration, TimePosition};
//! use superb::primitives::session::commands::{AddClip, AddTrack, MoveClip};
//! use superb::primitives::session::{Clip, ClipSource, Session, Track, TrackKind};
//! use superb::primitives::undo::UndoStack;
//!
//! let mut session = Session::new("demo", SampleRate::HZ_48K);
//! let mut history = UndoStack::new();
//!
//! let track = session.allocate_track_id();
//! history.push(&mut session, AddTrack::new(Track::with_id(track, "vox", TrackKind::Audio))).unwrap();
//!
//! let clip = session.allocate_clip_id();
//! let take = Clip::with_id(clip, "take 1", TimePosition::ZERO, TimeDuration::from_samples(1_000), ClipSource::Empty).unwrap();
//! history.push(&mut session, AddClip::new(track, take)).unwrap();
//!
//! // A drag: many small moves, one undo step.
//! for x in [100, 200, 300] {
//!     history.push(&mut session, MoveClip::new(clip, TimePosition::from_samples(x))).unwrap();
//! }
//! history.seal();
//! assert_eq!(history.undo_len(), 3);
//! assert_eq!(session.clip(clip).unwrap().start().samples(), 300);
//!
//! history.undo(&mut session).unwrap();
//! assert_eq!(session.clip(clip).unwrap().start().samples(), 0);
//! history.undo(&mut session).unwrap();
//! assert!(session.clip(clip).is_err());
//! history.redo(&mut session).unwrap();
//! assert!(session.clip(clip).is_ok(), "restored under the same id");
//! ```

use crate::primitives::transport::LoopRange;
use crate::primitives::undo::{CoalesceKey, Command, PropertyEdit};
use crate::primitives::{
    BeatPosition, ClipId, InsertId, MarkerId, Result, SendId, SuperbError, TimeDuration,
    TimePosition, TrackId,
};

use crate::primitives::session::{Clip, Insert, Marker, Session, Track, TrackKind, TrackSend};

/// `kind` values of the [`CoalesceKey`]s used by this module.
pub mod keys {
    /// [`set_session_name`](super::set_session_name).
    pub const SESSION_NAME: u64 = 1;
    /// [`set_track_name`](super::set_track_name).
    pub const TRACK_NAME: u64 = 10;
    /// [`set_track_gain_db`](super::set_track_gain_db).
    pub const TRACK_GAIN: u64 = 11;
    /// [`set_track_pan`](super::set_track_pan).
    pub const TRACK_PAN: u64 = 12;
    /// [`set_track_mute`](super::set_track_mute).
    pub const TRACK_MUTE: u64 = 13;
    /// [`set_track_solo`](super::set_track_solo).
    pub const TRACK_SOLO: u64 = 14;
    /// [`set_track_armed`](super::set_track_armed).
    pub const TRACK_ARMED: u64 = 15;
    /// [`set_track_color`](super::set_track_color).
    pub const TRACK_COLOR: u64 = 16;
    /// [`MoveClip`](super::MoveClip).
    pub const CLIP_MOVE: u64 = 20;
    /// [`ResizeClip`](super::ResizeClip).
    pub const CLIP_RESIZE: u64 = 21;
    /// [`set_clip_name`](super::set_clip_name).
    pub const CLIP_NAME: u64 = 22;
    /// [`set_clip_gain_db`](super::set_clip_gain_db).
    pub const CLIP_GAIN: u64 = 23;
    /// [`set_clip_mute`](super::set_clip_mute).
    pub const CLIP_MUTE: u64 = 24;
    /// [`MoveMarker`](super::MoveMarker).
    pub const MARKER_MOVE: u64 = 30;
    /// [`set_marker_name`](super::set_marker_name).
    pub const MARKER_NAME: u64 = 31;
    /// [`SetTempo`](super::SetTempo).
    pub const TEMPO: u64 = 40;
    /// [`SetLoopRange`](super::SetLoopRange).
    pub const LOOP: u64 = 41;
    /// [`set_send_level_db`](super::set_send_level_db).
    pub const SEND_LEVEL: u64 = 50;
    /// [`set_send_enabled`](super::set_send_enabled).
    pub const SEND_ENABLED: u64 = 51;
    /// [`set_send_pre_fader`](super::set_send_pre_fader).
    pub const SEND_PRE_FADER: u64 = 52;
    /// [`set_insert_bypassed`](super::set_insert_bypassed).
    pub const INSERT_BYPASS: u64 = 60;
    /// [`set_insert_param`](super::set_insert_param): the key's `kind` is
    /// this base plus the parameter id, so different parameters never merge.
    pub const INSERT_PARAM_BASE: u64 = 1 << 32;
}

fn not_applied() -> SuperbError {
    SuperbError::Message("command has not been applied".into())
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

/// Adds a track (at the end, or at [`at`](AddTrack::at)).
///
/// Give the track a pre-allocated ID (`Session::allocate_track_id`) to know
/// it before pushing; otherwise the session assigns one on first apply and
/// redo keeps it.
pub struct AddTrack {
    track: Option<Track>,
    index: Option<usize>,
    id: TrackId,
}

impl AddTrack {
    /// Creates the command.
    pub fn new(track: Track) -> Self {
        Self {
            id: track.id(),
            track: Some(track),
            index: None,
        }
    }

    /// Inserts at `index` instead of appending.
    pub fn at(mut self, index: usize) -> Self {
        self.index = Some(index);
        self
    }

    /// The track's ID (valid once applied, or from the start if pre-allocated).
    pub fn id(&self) -> TrackId {
        self.id
    }
}

impl Command<Session> for AddTrack {
    fn label(&self) -> &str {
        "Add track"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let track = self.track.as_ref().ok_or_else(not_applied)?.clone();
        let index = self
            .index
            .unwrap_or(session.track_count())
            .min(session.track_count());
        self.id = session.insert_track(index, track)?;
        self.track = None;
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        self.index = Some(session.track_index(self.id)?);
        self.track = Some(session.remove_track(self.id)?);
        Ok(())
    }
}

/// Removes a track with its clips and sends; undo restores it at its old
/// position. Removing a bus also detaches the tracks and sends routed into
/// it (see [`Session::remove_track`]); undo re-attaches them.
pub struct RemoveTrack {
    id: TrackId,
    removed: Option<(usize, Track)>,
    routed: Vec<TrackId>,
    sends: Vec<(TrackId, TrackSend)>,
}

impl RemoveTrack {
    /// Creates the command.
    pub fn new(id: TrackId) -> Self {
        Self {
            id,
            removed: None,
            routed: Vec::new(),
            sends: Vec::new(),
        }
    }
}

impl Command<Session> for RemoveTrack {
    fn label(&self) -> &str {
        "Remove track"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let index = session.track_index(self.id)?;
        if session.track(self.id)?.kind == TrackKind::Bus {
            self.routed = session.tracks_routed_to(self.id).collect();
            self.sends = session
                .sends_to(self.id)
                .map(|(t, s)| (t, s.clone()))
                .collect();
        }
        self.removed = Some((index, session.remove_track(self.id)?));
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (index, track) = self.removed.take().ok_or_else(not_applied)?;
        session.insert_track(index.min(session.track_count()), track)?;
        for t in self.routed.drain(..) {
            session.set_track_output(t, Some(self.id))?;
        }
        for (t, send) in self.sends.drain(..) {
            session.add_send(t, send)?;
        }
        Ok(())
    }
}

/// Reorders a track.
pub struct MoveTrack {
    id: TrackId,
    to: usize,
    from: Option<usize>,
}

impl MoveTrack {
    /// Creates the command.
    pub fn new(id: TrackId, to: usize) -> Self {
        Self { id, to, from: None }
    }
}

impl Command<Session> for MoveTrack {
    fn label(&self) -> &str {
        "Move track"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        self.from = Some(session.track_index(self.id)?);
        session.move_track(self.id, self.to)
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.move_track(self.id, self.from.ok_or_else(not_applied)?)
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Routes a track's output into a bus track (`None` = master). Validated
/// by [`Session::set_track_output`]; undo restores the previous output.
pub struct SetTrackOutput {
    track: TrackId,
    output: Option<TrackId>,
    previous: Option<Option<TrackId>>,
}

impl SetTrackOutput {
    /// Creates the command.
    pub fn new(track: TrackId, output: Option<TrackId>) -> Self {
        Self {
            track,
            output,
            previous: None,
        }
    }
}

impl Command<Session> for SetTrackOutput {
    fn label(&self) -> &str {
        "Route track"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let before = session.track(self.track)?.output();
        session.set_track_output(self.track, self.output)?;
        self.previous = Some(before);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.set_track_output(self.track, self.previous.ok_or_else(not_applied)?)
    }
}

/// Adds a send from a track to a bus track.
pub struct AddSend {
    track: TrackId,
    send: Option<TrackSend>,
    id: SendId,
}

impl AddSend {
    /// Creates the command. Give the send a pre-allocated ID
    /// (`Session::allocate_send_id`) to know it before pushing.
    pub fn new(track: TrackId, send: TrackSend) -> Self {
        Self {
            track,
            id: send.id(),
            send: Some(send),
        }
    }

    /// The send's ID (valid once applied, or from the start if pre-allocated).
    pub fn id(&self) -> SendId {
        self.id
    }
}

impl Command<Session> for AddSend {
    fn label(&self) -> &str {
        "Add send"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let send = self.send.as_ref().ok_or_else(not_applied)?.clone();
        self.id = session.add_send(self.track, send)?;
        self.send = None;
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, send) = session.remove_send(self.id)?;
        self.track = track;
        self.send = Some(send);
        Ok(())
    }
}

/// Removes a send; undo puts it back on its track.
pub struct RemoveSend {
    id: SendId,
    removed: Option<(TrackId, TrackSend)>,
}

impl RemoveSend {
    /// Creates the command.
    pub fn new(id: SendId) -> Self {
        Self { id, removed: None }
    }
}

impl Command<Session> for RemoveSend {
    fn label(&self) -> &str {
        "Remove send"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        self.removed = Some(session.remove_send(self.id)?);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, send) = self.removed.take().ok_or_else(not_applied)?;
        session.add_send(track, send)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Inserts
// ---------------------------------------------------------------------------

/// Adds an effect to a track's insert chain (at the end, or at
/// [`at`](AddInsert::at)). Undo removes it again.
pub struct AddInsert {
    track: TrackId,
    insert: Option<Insert>,
    index: Option<usize>,
    id: InsertId,
}

impl AddInsert {
    /// Creates the command. Give the insert a pre-allocated ID
    /// (`Session::allocate_insert_id`) to know it before pushing.
    pub fn new(track: TrackId, insert: Insert) -> Self {
        Self {
            track,
            id: insert.id(),
            insert: Some(insert),
            index: None,
        }
    }

    /// Places the insert at `index` in the chain instead of the end.
    pub fn at(mut self, index: usize) -> Self {
        self.index = Some(index);
        self
    }

    /// The insert's ID (valid once applied, or from the start if pre-allocated).
    pub fn id(&self) -> InsertId {
        self.id
    }
}

impl Command<Session> for AddInsert {
    fn label(&self) -> &str {
        "Add insert"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let insert = self.insert.as_ref().ok_or_else(not_applied)?.clone();
        self.id = match self.index {
            Some(index) => session.add_insert_at(self.track, index, insert)?,
            None => session.add_insert(self.track, insert)?,
        };
        self.insert = None;
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, index, insert) = session.remove_insert(self.id)?;
        self.track = track;
        self.index = Some(index);
        self.insert = Some(insert);
        Ok(())
    }
}

/// Removes an insert; undo puts it back at the same position.
pub struct RemoveInsert {
    id: InsertId,
    removed: Option<(TrackId, usize, Insert)>,
}

impl RemoveInsert {
    /// Creates the command.
    pub fn new(id: InsertId) -> Self {
        Self { id, removed: None }
    }
}

impl Command<Session> for RemoveInsert {
    fn label(&self) -> &str {
        "Remove insert"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        self.removed = Some(session.remove_insert(self.id)?);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, index, insert) = self.removed.take().ok_or_else(not_applied)?;
        session.add_insert_at(
            track,
            index.min(session.track(track)?.inserts().len()),
            insert,
        )?;
        Ok(())
    }
}

/// Reorders an insert within its chain.
pub struct MoveInsert {
    id: InsertId,
    to: usize,
    from: Option<usize>,
}

impl MoveInsert {
    /// Creates the command.
    pub fn new(id: InsertId, to: usize) -> Self {
        Self { id, to, from: None }
    }
}

impl Command<Session> for MoveInsert {
    fn label(&self) -> &str {
        "Move insert"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let track = session.insert_owner(self.id)?;
        let from = session
            .track(track)?
            .inserts()
            .iter()
            .position(|i| i.id() == self.id)
            .ok_or_else(not_applied)?;
        session.move_insert(self.id, self.to)?;
        self.from = Some(from);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.move_insert(self.id, self.from.ok_or_else(not_applied)?)
    }
}

// ---------------------------------------------------------------------------
// Clips
// ---------------------------------------------------------------------------

/// Adds a clip to a track.
pub struct AddClip {
    track: TrackId,
    clip: Option<Clip>,
    id: ClipId,
}

impl AddClip {
    /// Creates the command. Give the clip a pre-allocated ID
    /// (`Session::allocate_clip_id`) to know it before pushing.
    pub fn new(track: TrackId, clip: Clip) -> Self {
        Self {
            track,
            id: clip.id(),
            clip: Some(clip),
        }
    }

    /// The clip's ID (valid once applied, or from the start if pre-allocated).
    pub fn id(&self) -> ClipId {
        self.id
    }
}

impl Command<Session> for AddClip {
    fn label(&self) -> &str {
        "Add clip"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let clip = self.clip.as_ref().ok_or_else(not_applied)?.clone();
        self.id = session.add_clip(self.track, clip)?;
        self.clip = None;
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, clip) = session.remove_clip(self.id)?;
        self.track = track;
        self.clip = Some(clip);
        Ok(())
    }
}

/// Removes a clip; undo puts it back on its track.
pub struct RemoveClip {
    id: ClipId,
    removed: Option<(TrackId, Clip)>,
}

impl RemoveClip {
    /// Creates the command.
    pub fn new(id: ClipId) -> Self {
        Self { id, removed: None }
    }
}

impl Command<Session> for RemoveClip {
    fn label(&self) -> &str {
        "Remove clip"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        self.removed = Some(session.remove_clip(self.id)?);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, clip) = self.removed.take().ok_or_else(not_applied)?;
        session.add_clip(track, clip)?;
        Ok(())
    }
}

/// Moves a clip in time and optionally to another track. Coalesces.
pub struct MoveClip {
    id: ClipId,
    start: TimePosition,
    track: Option<TrackId>,
    from: Option<(TrackId, TimePosition)>,
}

impl MoveClip {
    /// Moves the clip so it starts at `start`.
    pub fn new(id: ClipId, start: TimePosition) -> Self {
        Self {
            id,
            start,
            track: None,
            from: None,
        }
    }

    /// Also moves the clip to `track`.
    pub fn to_track(mut self, track: TrackId) -> Self {
        self.track = Some(track);
        self
    }
}

impl Command<Session> for MoveClip {
    fn label(&self) -> &str {
        "Move clip"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let track = session.clip_track(self.id)?;
        let start = session.clip(self.id)?.start();
        if let Some(t) = self.track {
            session.move_clip_to_track(self.id, t)?;
        }
        session.move_clip(self.id, self.start)?;
        self.from = Some((track, start));
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let (track, start) = self.from.ok_or_else(not_applied)?;
        session.move_clip_to_track(self.id, track)?;
        session.move_clip(self.id, start)
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::CLIP_MOVE, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Session>>,
    ) -> core::result::Result<(), Box<dyn Command<Session>>> {
        let next = next.downcast::<Self>()?;
        self.start = next.start;
        if next.track.is_some() {
            self.track = next.track;
        }
        Ok(())
    }
}

/// Changes a clip's length. Coalesces.
pub struct ResizeClip {
    id: ClipId,
    length: TimeDuration,
    from: Option<TimeDuration>,
}

impl ResizeClip {
    /// Creates the command.
    pub fn new(id: ClipId, length: TimeDuration) -> Self {
        Self {
            id,
            length,
            from: None,
        }
    }
}

impl Command<Session> for ResizeClip {
    fn label(&self) -> &str {
        "Resize clip"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let before = session.clip(self.id)?.length();
        session.resize_clip(self.id, self.length)?;
        self.from = Some(before);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.resize_clip(self.id, self.from.ok_or_else(not_applied)?)
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::CLIP_RESIZE, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Session>>,
    ) -> core::result::Result<(), Box<dyn Command<Session>>> {
        let next = next.downcast::<Self>()?;
        self.length = next.length;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Markers
// ---------------------------------------------------------------------------

/// Adds a marker.
pub struct AddMarker {
    marker: Option<Marker>,
    id: MarkerId,
}

impl AddMarker {
    /// Creates the command. Give the marker a pre-allocated ID
    /// (`Session::allocate_marker_id`) to know it before pushing.
    pub fn new(marker: Marker) -> Self {
        Self {
            id: marker.id(),
            marker: Some(marker),
        }
    }

    /// The marker's ID (valid once applied, or from the start if pre-allocated).
    pub fn id(&self) -> MarkerId {
        self.id
    }
}

impl Command<Session> for AddMarker {
    fn label(&self) -> &str {
        "Add marker"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let marker = self.marker.as_ref().ok_or_else(not_applied)?.clone();
        self.id = session.insert_marker(marker)?;
        self.marker = None;
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        self.marker = Some(session.remove_marker(self.id)?);
        Ok(())
    }
}

/// Removes a marker.
pub struct RemoveMarker {
    id: MarkerId,
    removed: Option<Marker>,
}

impl RemoveMarker {
    /// Creates the command.
    pub fn new(id: MarkerId) -> Self {
        Self { id, removed: None }
    }
}

impl Command<Session> for RemoveMarker {
    fn label(&self) -> &str {
        "Remove marker"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        self.removed = Some(session.remove_marker(self.id)?);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.insert_marker(self.removed.take().ok_or_else(not_applied)?)?;
        Ok(())
    }
}

/// Moves a marker. Coalesces.
pub struct MoveMarker {
    id: MarkerId,
    position: TimePosition,
    from: Option<TimePosition>,
}

impl MoveMarker {
    /// Creates the command.
    pub fn new(id: MarkerId, position: TimePosition) -> Self {
        Self {
            id,
            position,
            from: None,
        }
    }
}

impl Command<Session> for MoveMarker {
    fn label(&self) -> &str {
        "Move marker"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let before = session.marker(self.id)?.position();
        session.move_marker(self.id, self.position)?;
        self.from = Some(before);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.move_marker(self.id, self.from.ok_or_else(not_applied)?)
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::MARKER_MOVE, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Session>>,
    ) -> core::result::Result<(), Box<dyn Command<Session>>> {
        let next = next.downcast::<Self>()?;
        self.position = next.position;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tempo and loop
// ---------------------------------------------------------------------------

/// Sets (or adds) the tempo at a beat, constant or ramping to the next
/// point. Undo restores the previous tempo and ramp flag there, or removes
/// the point if there was none. Coalesces per beat.
pub struct SetTempo {
    beat: BeatPosition,
    bpm: f64,
    ramp: bool,
    previous: Option<Option<(f64, bool)>>,
}

impl SetTempo {
    /// Creates the command. Use `BeatPosition::ZERO` for the initial tempo.
    pub fn new(beat: BeatPosition, bpm: f64) -> Self {
        Self {
            beat,
            bpm,
            ramp: false,
            previous: None,
        }
    }

    /// Makes the point ramp linearly to the next tempo point.
    pub fn ramped(mut self) -> Self {
        self.ramp = true;
        self
    }
}

impl Command<Session> for SetTempo {
    fn label(&self) -> &str {
        "Set tempo"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        let before = session
            .tempo_map()
            .tempo_points()
            .iter()
            .find(|p| p.beat == self.beat)
            .map(|p| (p.bpm, p.ramp));
        let map = session.tempo_map_mut();
        if self.ramp {
            map.add_tempo_ramp(self.beat, self.bpm)?;
        } else {
            map.add_tempo(self.beat, self.bpm)?;
        }
        self.previous = Some(before);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        let map = session.tempo_map_mut();
        match self.previous.ok_or_else(not_applied)? {
            Some((bpm, true)) => map.add_tempo_ramp(self.beat, bpm),
            Some((bpm, false)) => map.add_tempo(self.beat, bpm),
            None => map.remove_tempo(self.beat),
        }
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::TEMPO, self.beat.beats().to_bits()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Session>>,
    ) -> core::result::Result<(), Box<dyn Command<Session>>> {
        let next = next.downcast::<Self>()?;
        self.bpm = next.bpm;
        self.ramp = next.ramp;
        Ok(())
    }
}

/// Sets or clears the loop range. Coalesces.
pub struct SetLoopRange {
    range: Option<LoopRange>,
    previous: Option<Option<LoopRange>>,
}

impl SetLoopRange {
    /// Creates the command.
    pub fn new(range: Option<LoopRange>) -> Self {
        Self {
            range,
            previous: None,
        }
    }
}

impl Command<Session> for SetLoopRange {
    fn label(&self) -> &str {
        "Set loop range"
    }

    fn apply(&mut self, session: &mut Session) -> Result<()> {
        self.previous = Some(session.loop_range());
        session.set_loop_range(self.range);
        Ok(())
    }

    fn revert(&mut self, session: &mut Session) -> Result<()> {
        session.set_loop_range(self.previous.ok_or_else(not_applied)?);
        Ok(())
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::LOOP, 0))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Session>>,
    ) -> core::result::Result<(), Box<dyn Command<Session>>> {
        let next = next.downcast::<Self>()?;
        self.range = next.range;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Property edits
// ---------------------------------------------------------------------------

fn track_property<V: Clone + Send + Sync + 'static>(
    label: &'static str,
    kind: u64,
    id: TrackId,
    value: V,
    get: impl Fn(&Track) -> V + Send + Sync + 'static,
    set: impl Fn(&mut Track, V) -> Result<()> + Send + Sync + 'static,
) -> PropertyEdit<Session, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Session| s.track(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Session, v| set(s.track_mut(id)?, v),
    )
}

fn clip_property<V: Clone + Send + Sync + 'static>(
    label: &'static str,
    kind: u64,
    id: ClipId,
    value: V,
    get: impl Fn(&Clip) -> V + Send + Sync + 'static,
    set: impl Fn(&mut Clip, V) + Send + Sync + 'static,
) -> PropertyEdit<Session, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Session| s.clip(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Session, v| {
            set(s.clip_mut(id)?, v);
            Ok(())
        },
    )
}

/// Renames the session.
pub fn set_session_name(name: impl Into<String>) -> PropertyEdit<Session, String> {
    PropertyEdit::new(
        "Rename session",
        Some(CoalesceKey::new(keys::SESSION_NAME, 0)),
        name.into(),
        |s: &Session| s.name().to_owned(),
        |s: &mut Session, v| {
            s.set_name(v);
            Ok(())
        },
    )
}

/// Renames a track.
pub fn set_track_name(id: TrackId, name: impl Into<String>) -> PropertyEdit<Session, String> {
    track_property(
        "Rename track",
        keys::TRACK_NAME,
        id,
        name.into(),
        |t| t.name.clone(),
        |t, v| {
            t.name = v;
            Ok(())
        },
    )
}

/// Sets a track's fader gain in dB. Rejects non-finite values.
pub fn set_track_gain_db(id: TrackId, gain_db: f32) -> PropertyEdit<Session, f32> {
    track_property(
        "Set track gain",
        keys::TRACK_GAIN,
        id,
        gain_db,
        |t| t.gain_db,
        |t, v| {
            if !v.is_finite() {
                return Err(SuperbError::InvalidArgument { what: "gain_db" });
            }
            t.gain_db = v;
            Ok(())
        },
    )
}

/// Sets a track's pan (`-1.0..=1.0`).
pub fn set_track_pan(id: TrackId, pan: f32) -> PropertyEdit<Session, f32> {
    track_property(
        "Set track pan",
        keys::TRACK_PAN,
        id,
        pan,
        |t| t.pan,
        |t, v| {
            if !(-1.0..=1.0).contains(&v) {
                return Err(SuperbError::InvalidArgument { what: "pan" });
            }
            t.pan = v;
            Ok(())
        },
    )
}

/// Mutes or unmutes a track.
pub fn set_track_mute(id: TrackId, muted: bool) -> PropertyEdit<Session, bool> {
    track_property(
        "Mute track",
        keys::TRACK_MUTE,
        id,
        muted,
        |t| t.muted,
        |t, v| {
            t.muted = v;
            Ok(())
        },
    )
}

/// Solos or unsolos a track.
pub fn set_track_solo(id: TrackId, soloed: bool) -> PropertyEdit<Session, bool> {
    track_property(
        "Solo track",
        keys::TRACK_SOLO,
        id,
        soloed,
        |t| t.soloed,
        |t, v| {
            t.soloed = v;
            Ok(())
        },
    )
}

/// Arms or disarms a track for recording.
pub fn set_track_armed(id: TrackId, armed: bool) -> PropertyEdit<Session, bool> {
    track_property(
        "Arm track",
        keys::TRACK_ARMED,
        id,
        armed,
        |t| t.armed,
        |t, v| {
            t.armed = v;
            Ok(())
        },
    )
}

/// Sets a track's colour.
pub fn set_track_color(id: TrackId, color: Option<u32>) -> PropertyEdit<Session, Option<u32>> {
    track_property(
        "Set track colour",
        keys::TRACK_COLOR,
        id,
        color,
        |t| t.color,
        |t, v| {
            t.color = v;
            Ok(())
        },
    )
}

fn send_property<V: Clone + Send + Sync + 'static>(
    label: &'static str,
    kind: u64,
    id: SendId,
    value: V,
    get: impl Fn(&TrackSend) -> V + Send + Sync + 'static,
    set: impl Fn(&mut TrackSend, V) -> Result<()> + Send + Sync + 'static,
) -> PropertyEdit<Session, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Session| s.send(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Session, v| set(s.send_mut(id)?, v),
    )
}

/// Sets a send's level in dB. Rejects non-finite values. Coalesces.
pub fn set_send_level_db(id: SendId, level_db: f32) -> PropertyEdit<Session, f32> {
    send_property(
        "Set send level",
        keys::SEND_LEVEL,
        id,
        level_db,
        |s| s.level_db,
        |s, v| {
            if !v.is_finite() {
                return Err(SuperbError::InvalidArgument { what: "level_db" });
            }
            s.level_db = v;
            Ok(())
        },
    )
}

/// Enables or disables a send.
pub fn set_send_enabled(id: SendId, enabled: bool) -> PropertyEdit<Session, bool> {
    send_property(
        "Enable send",
        keys::SEND_ENABLED,
        id,
        enabled,
        |s| s.enabled,
        |s, v| {
            s.enabled = v;
            Ok(())
        },
    )
}

/// Switches a send between pre-fader (`true`) and post-fader.
pub fn set_send_pre_fader(id: SendId, pre_fader: bool) -> PropertyEdit<Session, bool> {
    send_property(
        "Set send tap",
        keys::SEND_PRE_FADER,
        id,
        pre_fader,
        |s| s.pre_fader,
        |s, v| {
            s.pre_fader = v;
            Ok(())
        },
    )
}

fn insert_property<V: Clone + Send + Sync + 'static>(
    label: &'static str,
    kind: u64,
    id: InsertId,
    value: V,
    get: impl Fn(&Insert) -> V + Send + Sync + 'static,
    set: impl Fn(&mut Insert, V) + Send + Sync + 'static,
) -> PropertyEdit<Session, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Session| s.insert(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Session, v| {
            set(s.insert_mut(id)?, v);
            Ok(())
        },
    )
}

/// Bypasses or re-enables an insert.
pub fn set_insert_bypassed(id: InsertId, bypassed: bool) -> PropertyEdit<Session, bool> {
    insert_property(
        "Bypass insert",
        keys::INSERT_BYPASS,
        id,
        bypassed,
        |i| i.bypassed,
        |i, v| i.bypassed = v,
    )
}

/// Sets one plugin parameter on an insert. Consecutive edits of the same
/// parameter coalesce (a knob drag is one undo step); undoing an edit of a
/// parameter that had no stored value clears it again.
pub fn set_insert_param(
    id: InsertId,
    param: u32,
    value: f64,
) -> PropertyEdit<Session, Option<f64>> {
    insert_property(
        "Set plugin parameter",
        keys::INSERT_PARAM_BASE + param as u64,
        id,
        Some(value),
        move |i| i.param(param),
        move |i, v| match v {
            Some(value) => i.set_param(param, value),
            None => {
                i.clear_param(param);
            }
        },
    )
}

/// Renames a clip.
pub fn set_clip_name(id: ClipId, name: impl Into<String>) -> PropertyEdit<Session, String> {
    clip_property(
        "Rename clip",
        keys::CLIP_NAME,
        id,
        name.into(),
        |i| i.name.clone(),
        |i, v| i.name = v,
    )
}

/// Sets a clip's gain in dB.
pub fn set_clip_gain_db(id: ClipId, gain_db: f32) -> PropertyEdit<Session, f32> {
    clip_property(
        "Set clip gain",
        keys::CLIP_GAIN,
        id,
        gain_db,
        |i| i.gain_db,
        |i, v| i.gain_db = v,
    )
}

/// Mutes or unmutes a clip.
pub fn set_clip_mute(id: ClipId, muted: bool) -> PropertyEdit<Session, bool> {
    clip_property(
        "Mute clip",
        keys::CLIP_MUTE,
        id,
        muted,
        |i| i.muted,
        |i, v| i.muted = v,
    )
}

/// Renames a marker.
pub fn set_marker_name(id: MarkerId, name: impl Into<String>) -> PropertyEdit<Session, String> {
    let name = name.into();
    let fallback = name.clone();
    PropertyEdit::new(
        "Rename marker",
        Some(CoalesceKey::new(keys::MARKER_NAME, id.raw())),
        name,
        move |s: &Session| {
            s.marker(id)
                .map(|m| m.name.clone())
                .unwrap_or_else(|_| fallback.clone())
        },
        move |s: &mut Session, v| {
            s.marker_mut(id)?.name = v;
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::{ClipSource, TrackKind};
    use crate::primitives::undo::UndoStack;
    use crate::primitives::SampleRate;

    const R: SampleRate = SampleRate::HZ_48K;

    fn make_clip(id: ClipId, start: i64, len: i64) -> Clip {
        Clip::with_id(
            id,
            "i",
            TimePosition::from_samples(start),
            TimeDuration::from_samples(len),
            ClipSource::Empty,
        )
        .unwrap()
    }

    fn base() -> (Session, UndoStack<Session>, TrackId, ClipId) {
        let mut s = Session::new("t", R);
        let t = s.add_track("a", TrackKind::Audio);
        let c = s.allocate_clip_id();
        s.add_clip(t, make_clip(c, 100, 50)).unwrap();
        (s, UndoStack::new(), t, c)
    }

    #[test]
    fn add_and_remove_track_keep_ids_and_positions() {
        let (mut s, mut h, a, clip) = base();
        let b = s.allocate_track_id();
        let cmd = AddTrack::new(Track::with_id(b, "b", TrackKind::Midi)).at(0);
        assert_eq!(cmd.id(), b);
        h.push(&mut s, cmd).unwrap();
        assert_eq!(s.track_index(b).unwrap(), 0);
        h.undo(&mut s).unwrap();
        assert!(s.track(b).is_err());
        h.redo(&mut s).unwrap();
        assert_eq!(s.track_index(b).unwrap(), 0);
        assert_eq!(s.track(b).unwrap().name, "b");

        // Without a pre-allocated id the session assigns one; redo keeps it.
        h.push(&mut s, AddTrack::new(Track::new("c", TrackKind::Bus)))
            .unwrap();
        let c = s.tracks().last().unwrap().id();
        assert!(c.is_valid());
        h.undo(&mut s).unwrap();
        h.redo(&mut s).unwrap();
        assert_eq!(s.tracks().last().unwrap().id(), c);
        // Out-of-range insert index is clamped to the end.
        h.push(
            &mut s,
            AddTrack::new(Track::new("d", TrackKind::Audio)).at(99),
        )
        .unwrap();
        assert_eq!(s.tracks().last().unwrap().name, "d");

        // Remove restores the track with its clips at the same index.
        h.push(&mut s, RemoveTrack::new(a)).unwrap();
        assert!(s.clip(clip).is_err());
        assert_eq!(h.undo_label(), Some("Remove track"));
        h.undo(&mut s).unwrap();
        assert_eq!(s.track_index(a).unwrap(), 1);
        assert_eq!(s.clip_track(clip).unwrap(), a);
        h.redo(&mut s).unwrap();
        assert!(s.track(a).is_err());

        // Failures leave nothing recorded.
        let before = h.undo_len();
        assert!(h.push(&mut s, RemoveTrack::new(a)).is_err());
        assert_eq!(h.undo_len(), before);
        assert!(h
            .push(
                &mut s,
                AddTrack::new(Track::with_id(b, "dup", TrackKind::Audio))
            )
            .is_err());
        assert_eq!(h.undo_len(), before);
    }

    #[test]
    fn move_track_round_trips() {
        let (mut s, mut h, a, _) = base();
        let b = s.add_track("b", TrackKind::Audio);
        let c = s.add_track("c", TrackKind::Audio);
        h.push(&mut s, MoveTrack::new(a, 2)).unwrap();
        assert_eq!(
            s.tracks().iter().map(Track::id).collect::<Vec<_>>(),
            [b, c, a]
        );
        h.undo(&mut s).unwrap();
        assert_eq!(
            s.tracks().iter().map(Track::id).collect::<Vec<_>>(),
            [a, b, c]
        );
        h.redo(&mut s).unwrap();
        assert_eq!(s.track_index(a).unwrap(), 2);
        assert!(h.push(&mut s, MoveTrack::new(a, 7)).is_err());
        assert_eq!(s.track_index(a).unwrap(), 2);
    }

    #[test]
    fn add_remove_move_resize_clips() {
        let (mut s, mut h, a, clip) = base();
        let b = s.add_track("b", TrackKind::Audio);
        let c2 = s.allocate_clip_id();
        let cmd = AddClip::new(b, make_clip(c2, 0, 10));
        assert_eq!(cmd.id(), c2);
        h.push(&mut s, cmd).unwrap();
        assert_eq!(s.clip_track(c2).unwrap(), b);
        h.undo(&mut s).unwrap();
        assert!(s.clip(c2).is_err());
        h.redo(&mut s).unwrap();
        assert_eq!(s.clip_track(c2).unwrap(), b);

        // Move coalesces; track change is remembered through the merge.
        h.push(&mut s, MoveClip::new(clip, TimePosition::from_samples(200)))
            .unwrap();
        h.push(
            &mut s,
            MoveClip::new(clip, TimePosition::from_samples(300)).to_track(b),
        )
        .unwrap();
        h.push(&mut s, MoveClip::new(clip, TimePosition::from_samples(400)))
            .unwrap();
        assert_eq!(h.undo_label(), Some("Move clip"));
        assert_eq!(s.clip_track(clip).unwrap(), b);
        assert_eq!(s.clip(clip).unwrap().start().samples(), 400);
        assert_eq!(
            s.track(b)
                .unwrap()
                .clips()
                .iter()
                .map(Clip::id)
                .collect::<Vec<_>>(),
            [c2, clip]
        );
        h.undo(&mut s).unwrap();
        assert_eq!(s.clip_track(clip).unwrap(), a);
        assert_eq!(s.clip(clip).unwrap().start().samples(), 100);
        h.redo(&mut s).unwrap();
        assert_eq!(s.clip_track(clip).unwrap(), b);
        assert_eq!(s.clip(clip).unwrap().start().samples(), 400);
        h.seal();

        h.push(
            &mut s,
            ResizeClip::new(clip, TimeDuration::from_samples(60)),
        )
        .unwrap();
        h.push(
            &mut s,
            ResizeClip::new(clip, TimeDuration::from_samples(70)),
        )
        .unwrap();
        assert!(h
            .push(&mut s, ResizeClip::new(clip, TimeDuration::ZERO))
            .is_err());
        assert_eq!(s.clip(clip).unwrap().length().samples(), 70);
        h.undo(&mut s).unwrap();
        assert_eq!(s.clip(clip).unwrap().length().samples(), 50);
        h.redo(&mut s).unwrap();
        assert_eq!(s.clip(clip).unwrap().length().samples(), 70);

        h.push(&mut s, RemoveClip::new(clip)).unwrap();
        assert!(s.clip(clip).is_err());
        h.undo(&mut s).unwrap();
        assert_eq!(s.clip_track(clip).unwrap(), b);
        assert_eq!(s.clip(clip).unwrap().length().samples(), 70);
        assert!(h
            .push(
                &mut s,
                MoveClip::new(ClipId::from_raw(999), TimePosition::ZERO)
            )
            .is_err());
    }

    #[test]
    fn markers_round_trip_and_coalesce() {
        let (mut s, mut h, _, _) = base();
        let m = s.allocate_marker_id();
        let cmd = AddMarker::new(Marker::with_id(m, "verse", TimePosition::from_samples(10)));
        assert_eq!(cmd.id(), m);
        h.push(&mut s, cmd).unwrap();
        h.push(&mut s, MoveMarker::new(m, TimePosition::from_samples(20)))
            .unwrap();
        h.push(&mut s, MoveMarker::new(m, TimePosition::from_samples(30)))
            .unwrap();
        assert_eq!(h.undo_len(), 2);
        h.push(&mut s, set_marker_name(m, "chorus")).unwrap();
        assert_eq!(s.marker(m).unwrap().name, "chorus");
        h.push(&mut s, RemoveMarker::new(m)).unwrap();
        assert!(s.marker(m).is_err());
        h.undo(&mut s).unwrap();
        assert_eq!(s.marker(m).unwrap().position().samples(), 30);
        h.undo(&mut s).unwrap();
        assert_eq!(s.marker(m).unwrap().name, "verse");
        h.undo(&mut s).unwrap();
        assert_eq!(s.marker(m).unwrap().position().samples(), 10);
        h.undo(&mut s).unwrap();
        assert!(s.marker(m).is_err());
        assert!(!h.can_undo());
        while h.redo(&mut s).unwrap() {}
        assert!(s.marker(m).is_err());
        // An unapplied structural command cannot be reverted.
        let mut fresh = RemoveMarker::new(m);
        assert!(fresh.revert(&mut s).is_err());
        let mut fresh = MoveMarker::new(m, TimePosition::ZERO);
        assert!(fresh.revert(&mut s).is_err());
    }

    #[test]
    fn tempo_and_loop_commands() {
        let (mut s, mut h, _, _) = base();
        let beat = BeatPosition::new(4.0);
        h.push(&mut s, SetTempo::new(beat, 130.0)).unwrap();
        h.push(&mut s, SetTempo::new(beat, 135.0)).unwrap();
        assert_eq!(h.undo_len(), 1);
        assert_eq!(s.tempo_map().tempo_at(beat), 135.0);
        h.undo(&mut s).unwrap();
        assert_eq!(
            s.tempo_map().tempo_points().len(),
            1,
            "point removed on undo"
        );
        h.redo(&mut s).unwrap();
        assert_eq!(s.tempo_map().tempo_at(beat), 135.0);
        h.seal();
        // Existing point: undo restores the old bpm.
        h.push(&mut s, SetTempo::new(BeatPosition::ZERO, 90.0))
            .unwrap();
        assert_eq!(s.tempo_map().tempo_at(BeatPosition::ZERO), 90.0);
        h.undo(&mut s).unwrap();
        assert_eq!(s.tempo_map().tempo_at(BeatPosition::ZERO), 120.0);
        assert!(h.push(&mut s, SetTempo::new(beat, -3.0)).is_err());
        // Ramp flag is applied, merged and restored.
        h.seal();
        h.push(&mut s, SetTempo::new(BeatPosition::ZERO, 120.0).ramped())
            .unwrap();
        assert!(s.tempo_map().tempo_points()[0].ramp);
        assert_eq!(
            s.tempo_map().tempo_at(BeatPosition::new(2.0)),
            127.5,
            "ramping to 135 at beat 4"
        );
        h.push(&mut s, SetTempo::new(BeatPosition::ZERO, 100.0))
            .unwrap(); // merged: constant again
        assert!(!s.tempo_map().tempo_points()[0].ramp);
        assert_eq!(h.undo_len(), 2, "both tempo edits at beat 0 are one step");
        h.undo(&mut s).unwrap();
        assert!(!s.tempo_map().tempo_points()[0].ramp);
        assert_eq!(s.tempo_map().tempo_at(BeatPosition::ZERO), 120.0);

        let r1 = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(10)).unwrap();
        let r2 = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(20)).unwrap();
        h.push(&mut s, SetLoopRange::new(Some(r1))).unwrap();
        h.push(&mut s, SetLoopRange::new(Some(r2))).unwrap();
        assert_eq!(s.loop_range(), Some(r2));
        h.undo(&mut s).unwrap();
        assert_eq!(s.loop_range(), None);
        h.redo(&mut s).unwrap();
        h.seal();
        h.push(&mut s, SetLoopRange::new(None)).unwrap();
        assert_eq!(s.loop_range(), None);
        h.undo(&mut s).unwrap();
        assert_eq!(s.loop_range(), Some(r2));
    }

    #[test]
    fn property_edits_validate_and_coalesce() {
        let (mut s, mut h, a, clip) = base();
        h.push(&mut s, set_session_name("song")).unwrap();
        h.push(&mut s, set_track_name(a, "vox")).unwrap();
        h.push(&mut s, set_track_gain_db(a, -3.0)).unwrap();
        h.push(&mut s, set_track_gain_db(a, -6.0)).unwrap();
        assert_eq!(h.undo_len(), 3);
        assert!(h.push(&mut s, set_track_gain_db(a, f32::NAN)).is_err());
        assert!(h.push(&mut s, set_track_pan(a, 1.5)).is_err());
        h.push(&mut s, set_track_pan(a, -0.5)).unwrap();
        h.push(&mut s, set_track_mute(a, true)).unwrap();
        h.push(&mut s, set_track_solo(a, true)).unwrap();
        h.push(&mut s, set_track_armed(a, true)).unwrap();
        h.push(&mut s, set_track_color(a, Some(0x123456))).unwrap();
        h.push(&mut s, set_clip_name(clip, "take")).unwrap();
        h.push(&mut s, set_clip_gain_db(clip, -2.0)).unwrap();
        h.push(&mut s, set_clip_mute(clip, true)).unwrap();
        let t = s.track(a).unwrap();
        assert_eq!(
            (
                t.name.as_str(),
                t.gain_db,
                t.pan,
                t.muted,
                t.soloed,
                t.armed,
                t.color
            ),
            ("vox", -6.0, -0.5, true, true, true, Some(0x123456))
        );
        let i = s.clip(clip).unwrap();
        assert_eq!((i.name.as_str(), i.gain_db, i.muted), ("take", -2.0, true));
        assert_eq!(s.name(), "song");
        while h.undo(&mut s).unwrap() {}
        let t = s.track(a).unwrap();
        assert_eq!(
            (
                t.name.as_str(),
                t.gain_db,
                t.pan,
                t.muted,
                t.soloed,
                t.armed,
                t.color
            ),
            ("a", 0.0, 0.0, false, false, false, None)
        );
        let i = s.clip(clip).unwrap();
        assert_eq!((i.name.as_str(), i.gain_db, i.muted), ("i", 0.0, false));
        assert_eq!(s.name(), "t");
        while h.redo(&mut s).unwrap() {}
        assert_eq!(s.track(a).unwrap().gain_db, -6.0);
        // Missing targets fail cleanly.
        assert!(h
            .push(&mut s, set_track_name(TrackId::from_raw(999), "x"))
            .is_err());
        assert!(h
            .push(&mut s, set_clip_mute(ClipId::from_raw(999), true))
            .is_err());
        assert!(h
            .push(&mut s, set_marker_name(MarkerId::from_raw(999), "x"))
            .is_err());
    }

    #[test]
    fn routing_commands_round_trip_and_bus_removal_reattaches() {
        let (mut s, mut h, a, _) = base();
        let k = s.add_track("k", TrackKind::Midi);
        let sub = s.add_track("sub", TrackKind::Bus);
        let fx = s.add_track("fx", TrackKind::Bus);

        h.push(&mut s, SetTrackOutput::new(a, Some(sub))).unwrap();
        h.push(&mut s, SetTrackOutput::new(sub, Some(fx))).unwrap();
        assert_eq!(h.undo_label(), Some("Route track"));
        assert_eq!(h.undo_len(), 2, "routing does not coalesce");
        assert!(
            h.push(&mut s, SetTrackOutput::new(fx, Some(sub))).is_err(),
            "cycle rejected, nothing recorded"
        );
        assert_eq!(h.undo_len(), 2);
        assert!(h.push(&mut s, SetTrackOutput::new(a, Some(k))).is_err());
        h.undo(&mut s).unwrap();
        assert_eq!(s.track(sub).unwrap().output(), None);
        h.redo(&mut s).unwrap();
        assert_eq!(s.track(sub).unwrap().output(), Some(fx));

        let id = s.allocate_send_id();
        let cmd = AddSend::new(a, TrackSend::with_id(id, fx).level_db(-3.0));
        assert_eq!(cmd.id(), id);
        h.push(&mut s, cmd).unwrap();
        h.push(&mut s, AddSend::new(k, TrackSend::new(fx))).unwrap();
        let auto = s.track(k).unwrap().sends()[0].id();
        assert!(
            h.push(&mut s, AddSend::new(sub, TrackSend::new(fx)))
                .is_err(),
            "buses cannot send"
        );
        h.push(&mut s, set_send_level_db(id, -6.0)).unwrap();
        h.push(&mut s, set_send_level_db(id, -9.0)).unwrap();
        assert!(h
            .push(&mut s, set_send_level_db(id, f32::INFINITY))
            .is_err());
        h.push(&mut s, set_send_enabled(id, false)).unwrap();
        h.push(&mut s, set_send_pre_fader(id, true)).unwrap();
        let send = s.send(id).unwrap();
        assert_eq!(
            (send.level_db, send.enabled, send.pre_fader),
            (-9.0, false, true)
        );
        h.undo(&mut s).unwrap();
        h.undo(&mut s).unwrap();
        h.undo(&mut s).unwrap();
        let send = s.send(id).unwrap();
        assert_eq!(
            (send.level_db, send.enabled, send.pre_fader),
            (-3.0, true, false),
            "level edits were one step"
        );
        while h.redo(&mut s).unwrap() {}
        assert_eq!(s.send(id).unwrap().level_db, -9.0);
        h.seal();

        h.push(&mut s, RemoveSend::new(auto)).unwrap();
        assert!(s.send(auto).is_err());
        h.undo(&mut s).unwrap();
        assert_eq!(s.send_track(auto).unwrap(), k);
        h.redo(&mut s).unwrap();
        h.undo(&mut s).unwrap();
        assert!(h
            .push(&mut s, RemoveSend::new(SendId::from_raw(999)))
            .is_err());
        assert!(h
            .push(&mut s, set_send_enabled(SendId::from_raw(999), true))
            .is_err());

        // Removing the fx bus detaches sub's output and both sends; undo
        // brings the bus back with everything re-attached.
        h.push(&mut s, RemoveTrack::new(fx)).unwrap();
        assert_eq!(s.track(sub).unwrap().output(), None);
        assert_eq!(s.send_count(), 0);
        h.undo(&mut s).unwrap();
        assert_eq!(s.track(sub).unwrap().output(), Some(fx));
        assert_eq!(s.send_count(), 2);
        assert_eq!(s.send(id).unwrap().level_db, -9.0);
        assert_eq!(s.send_track(auto).unwrap(), k);
        h.redo(&mut s).unwrap();
        assert_eq!(s.send_count(), 0);
        h.undo(&mut s).unwrap();
        // Removing a routed track and undoing keeps its output and sends.
        h.push(&mut s, RemoveTrack::new(a)).unwrap();
        h.undo(&mut s).unwrap();
        assert_eq!(s.track(a).unwrap().output(), Some(sub));
        assert_eq!(s.track(a).unwrap().sends().len(), 1);
        // Unapplied commands cannot be reverted.
        let mut fresh = SetTrackOutput::new(a, None);
        assert!(fresh.revert(&mut s).is_err());
        let mut fresh = RemoveSend::new(id);
        assert!(fresh.revert(&mut s).is_err());
    }

    #[test]
    fn insert_commands_round_trip_and_params_coalesce_per_parameter() {
        use crate::primitives::plugins::PluginFormat;
        use crate::primitives::session::PluginRef;
        let plugin = |id: &str| PluginRef {
            format: PluginFormat::Clap,
            path: "/plugins/fx.clap".into(),
            id: id.into(),
        };
        let (mut s, mut h, a, _) = base();
        let eq = s.allocate_insert_id();
        let cmd = AddInsert::new(a, Insert::with_id(eq, plugin("eq")));
        assert_eq!(cmd.id(), eq);
        h.push(&mut s, cmd).unwrap();
        h.push(&mut s, AddInsert::new(a, Insert::new(plugin("comp"))))
            .unwrap();
        let comp = s.track(a).unwrap().inserts()[1].id();
        h.push(&mut s, AddInsert::new(a, Insert::new(plugin("gate"))).at(0))
            .unwrap();
        let gate = s.track(a).unwrap().inserts()[0].id();
        let chain = |s: &Session| -> Vec<InsertId> {
            s.track(a)
                .unwrap()
                .inserts()
                .iter()
                .map(Insert::id)
                .collect()
        };
        assert_eq!(chain(&s), [gate, eq, comp]);
        h.undo(&mut s).unwrap();
        assert_eq!(chain(&s), [eq, comp]);
        h.redo(&mut s).unwrap();
        assert_eq!(
            chain(&s),
            [gate, eq, comp],
            "redo keeps the position and id"
        );

        h.push(&mut s, MoveInsert::new(comp, 0)).unwrap();
        assert_eq!(chain(&s), [comp, gate, eq]);
        h.undo(&mut s).unwrap();
        assert_eq!(chain(&s), [gate, eq, comp]);
        h.redo(&mut s).unwrap();
        h.seal();

        // Two parameters edited in turn are two steps; a drag on one is one.
        h.push(&mut s, set_insert_param(eq, 1, 0.1)).unwrap();
        h.push(&mut s, set_insert_param(eq, 1, 0.2)).unwrap();
        h.push(&mut s, set_insert_param(eq, 2, 0.9)).unwrap();
        assert_eq!(s.insert(eq).unwrap().param(1), Some(0.2));
        assert_eq!(s.insert(eq).unwrap().param(2), Some(0.9));
        h.undo(&mut s).unwrap();
        assert_eq!(
            s.insert(eq).unwrap().param(2),
            None,
            "cleared, not defaulted"
        );
        assert_eq!(s.insert(eq).unwrap().param(1), Some(0.2));
        h.undo(&mut s).unwrap();
        assert_eq!(s.insert(eq).unwrap().param(1), None);
        while h.redo(&mut s).unwrap() {}
        assert_eq!(s.insert(eq).unwrap().param(1), Some(0.2));

        h.push(&mut s, set_insert_bypassed(eq, true)).unwrap();
        assert!(s.insert(eq).unwrap().bypassed);
        h.push(&mut s, RemoveInsert::new(gate)).unwrap();
        assert_eq!(chain(&s), [comp, eq]);
        h.undo(&mut s).unwrap();
        assert_eq!(chain(&s), [comp, gate, eq], "restored at its old position");
        assert!(s.insert(eq).unwrap().bypassed);
        assert!(h
            .push(&mut s, RemoveInsert::new(InsertId::from_raw(999)))
            .is_err());
        assert!(h
            .push(&mut s, set_insert_param(InsertId::from_raw(999), 1, 0.5))
            .is_err());
        // Removing the track takes the chain along; undo brings it back.
        h.push(&mut s, RemoveTrack::new(a)).unwrap();
        assert_eq!(s.insert_count(), 0);
        h.undo(&mut s).unwrap();
        assert_eq!(chain(&s), [comp, gate, eq]);
    }

    #[test]
    fn grouped_edits_are_one_step() {
        let (mut s, mut h, a, clip) = base();
        h.begin_group("Duplicate track");
        let b = s.allocate_track_id();
        h.push(
            &mut s,
            AddTrack::new(Track::with_id(b, "a copy", TrackKind::Audio)),
        )
        .unwrap();
        let c = s.allocate_clip_id();
        h.push(&mut s, AddClip::new(b, make_clip(c, 100, 50)))
            .unwrap();
        h.push(&mut s, set_track_color(b, Some(1))).unwrap();
        assert!(h.end_group());
        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.undo_label(), Some("Duplicate track"));
        h.undo(&mut s).unwrap();
        assert_eq!(s.track_count(), 1);
        assert!(s.clip(c).is_err());
        assert!(s.clip(clip).is_ok());
        h.redo(&mut s).unwrap();
        assert_eq!(s.track(b).unwrap().color, Some(1));
        assert_eq!(s.clip_track(c).unwrap(), b);
        let _ = a;
    }
}
