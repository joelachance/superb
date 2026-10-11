//! Undoable edits of a [`Memory`], for use with
//! [`UndoStack<Memory>`](crate::primitives::undo::UndoStack).
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
//! use aura::primitives::{SampleRate, TimeDuration, TimePosition};
//! use aura::primitives::session::commands::{AddClip, AddTrack, MoveClip};
//! use aura::primitives::session::{Clip, ClipSource, Memory, Track, TrackKind};
//! use aura::primitives::undo::UndoStack;
//!
//! let mut memory = Memory::new("demo", SampleRate::HZ_48K);
//! let mut history = UndoStack::new();
//!
//! let track = memory.allocate_track_id();
//! history.push(&mut memory, AddTrack::new(Track::with_id(track, "vox", TrackKind::Audio))).unwrap();
//!
//! let clip = memory.allocate_clip_id();
//! let take = Clip::with_id(clip, "take 1", TimePosition::ZERO, TimeDuration::from_samples(1_000), ClipSource::Empty).unwrap();
//! history.push(&mut memory, AddClip::new(track, take)).unwrap();
//!
//! // A drag: many small moves, one undo step.
//! for x in [100, 200, 300] {
//!     history.push(&mut memory, MoveClip::new(clip, TimePosition::from_samples(x))).unwrap();
//! }
//! history.seal();
//! assert_eq!(history.undo_len(), 3);
//! assert_eq!(memory.clip(clip).unwrap().start().samples(), 300);
//!
//! history.undo(&mut memory).unwrap();
//! assert_eq!(memory.clip(clip).unwrap().start().samples(), 0);
//! history.undo(&mut memory).unwrap();
//! assert!(memory.clip(clip).is_err());
//! history.redo(&mut memory).unwrap();
//! assert!(memory.clip(clip).is_ok(), "restored under the same id");
//! ```

use crate::primitives::transport::LoopRange;
use crate::primitives::undo::{CoalesceKey, Command, PropertyEdit};
use crate::primitives::{
    AuraError, BeatPosition, ClipId, InsertId, MarkerId, Result, SendId, TimeDuration,
    TimePosition, TimeSignature, TrackId,
};

use crate::primitives::session::{
    Clip, ClipSource, ClipState, Insert, Marker, Memory, MidiNote, Track, TrackKind, TrackSend,
};

/// `kind` values of the [`CoalesceKey`]s used by this module.
pub mod keys {
    /// [`set_session_name`](super::set_session_name).
    pub const SESSION_NAME: u64 = 1;
    /// [`set_track_name`](super::set_track_name).
    pub const TRACK_NAME: u64 = 10;
    /// [`set_track_volume_db`](super::set_track_volume_db).
    pub const TRACK_VOLUME: u64 = 11;
    /// [`set_track_pan`](super::set_track_pan).
    pub const TRACK_PAN: u64 = 12;
    /// [`set_track_mute`](super::set_track_mute).
    pub const TRACK_MUTE: u64 = 13;
    /// [`set_track_solo`](super::set_track_solo).
    pub const TRACK_SOLO: u64 = 14;
    /// [`set_track_arm`](super::set_track_arm).
    pub const TRACK_ARM: u64 = 15;
    /// [`set_track_color`](super::set_track_color).
    pub const TRACK_COLOR: u64 = 16;
    /// [`set_track_monitor`](super::set_track_monitor).
    pub const TRACK_MONITOR: u64 = 17;
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
    /// [`set_clip_fade_in`](super::set_clip_fade_in).
    pub const CLIP_FADE_IN: u64 = 25;
    /// [`set_clip_fade_out`](super::set_clip_fade_out).
    pub const CLIP_FADE_OUT: u64 = 26;
    /// [`TrimClip`](super::TrimClip).
    pub const CLIP_TRIM: u64 = 27;
    /// A note changed inside a clip (one key per clip; merges only while
    /// the same note keeps changing).
    pub const CLIP_NOTE: u64 = 28;
    /// [`MoveMarker`](super::MoveMarker).
    pub const MARKER_MOVE: u64 = 30;
    /// [`set_marker_name`](super::set_marker_name).
    pub const MARKER_NAME: u64 = 31;
    /// [`SetTempo`](super::SetTempo).
    pub const TEMPO: u64 = 40;
    /// [`SetLoopRange`](super::SetLoopRange).
    pub const LOOP: u64 = 41;
    /// [`SetPunch`](super::SetPunch); the key's `id` is the point.
    pub const PUNCH: u64 = 42;
    /// [`set_metronome_level_db`](super::set_metronome_level_db).
    pub const METRONOME_LEVEL: u64 = 43;
    /// [`set_send_level_db`](super::set_send_level_db).
    pub const SEND_LEVEL: u64 = 50;
    /// [`set_send_enabled`](super::set_send_enabled).
    pub const SEND_ENABLED: u64 = 51;
    /// [`set_send_pre_fader`](super::set_send_pre_fader).
    pub const SEND_PRE_FADER: u64 = 52;
    /// [`set_insert_bypass`](super::set_insert_bypass).
    pub const INSERT_BYPASS: u64 = 60;
    /// [`set_insert_param`](super::set_insert_param): the key's `kind` is
    /// this base plus the parameter id, so different parameters never merge.
    pub const INSERT_PARAM_BASE: u64 = 1 << 32;
}

fn not_applied() -> AuraError {
    AuraError::Message("command has not been applied".into())
}

// ---------------------------------------------------------------------------
// Tracks
// ---------------------------------------------------------------------------

/// Adds a track (at the end, or at [`at`](AddTrack::at)).
///
/// Give the track a pre-allocated ID (`Memory::allocate_track_id`) to know
/// it before pushing; otherwise the memory assigns one on first apply and
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

impl Command<Memory> for AddTrack {
    fn label(&self) -> &str {
        "Add track"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let track = self.track.as_ref().ok_or_else(not_applied)?.clone();
        let index = self
            .index
            .unwrap_or(memory.track_count())
            .min(memory.track_count());
        self.id = memory.insert_track(index, track)?;
        self.track = None;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        self.index = Some(memory.track_index(self.id)?);
        self.track = Some(memory.remove_track(self.id)?);
        Ok(())
    }
}

/// Removes a track with its clips and sends; undo restores it at its old
/// position. Removing a bus also detaches the tracks and sends routed into
/// it (see [`Memory::remove_track`]); undo re-attaches them.
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

impl Command<Memory> for RemoveTrack {
    fn label(&self) -> &str {
        "Remove track"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let index = memory.track_index(self.id)?;
        if memory.track(self.id)?.kind == TrackKind::Bus {
            self.routed = memory.tracks_routed_to(self.id).collect();
            self.sends = memory
                .sends_to(self.id)
                .map(|(t, s)| (t, s.clone()))
                .collect();
        }
        self.removed = Some((index, memory.remove_track(self.id)?));
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (index, track) = self.removed.take().ok_or_else(not_applied)?;
        memory.insert_track(index.min(memory.track_count()), track)?;
        for t in self.routed.drain(..) {
            memory.set_track_output(t, Some(self.id))?;
        }
        for (t, send) in self.sends.drain(..) {
            memory.add_send(t, send)?;
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

impl Command<Memory> for MoveTrack {
    fn label(&self) -> &str {
        "Move track"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.from = Some(memory.track_index(self.id)?);
        memory.move_track(self.id, self.to)
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.move_track(self.id, self.from.ok_or_else(not_applied)?)
    }
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// Routes a track's output into a bus track (`None` = master). Validated
/// by [`Memory::set_track_output`]; undo restores the previous output.
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

impl Command<Memory> for SetTrackOutput {
    fn label(&self) -> &str {
        "Route track"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let before = memory.track(self.track)?.output();
        memory.set_track_output(self.track, self.output)?;
        self.previous = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.set_track_output(self.track, self.previous.ok_or_else(not_applied)?)
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
    /// (`Memory::allocate_send_id`) to know it before pushing.
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

impl Command<Memory> for AddSend {
    fn label(&self) -> &str {
        "Add send"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let send = self.send.as_ref().ok_or_else(not_applied)?.clone();
        self.id = memory.add_send(self.track, send)?;
        self.send = None;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, send) = memory.remove_send(self.id)?;
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

impl Command<Memory> for RemoveSend {
    fn label(&self) -> &str {
        "Remove send"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.removed = Some(memory.remove_send(self.id)?);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, send) = self.removed.take().ok_or_else(not_applied)?;
        memory.add_send(track, send)?;
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
    /// (`Memory::allocate_insert_id`) to know it before pushing.
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

impl Command<Memory> for AddInsert {
    fn label(&self) -> &str {
        "Add insert"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let insert = self.insert.as_ref().ok_or_else(not_applied)?.clone();
        self.id = match self.index {
            Some(index) => memory.add_insert_at(self.track, index, insert)?,
            None => memory.add_insert(self.track, insert)?,
        };
        self.insert = None;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, index, insert) = memory.remove_insert(self.id)?;
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

impl Command<Memory> for RemoveInsert {
    fn label(&self) -> &str {
        "Remove insert"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.removed = Some(memory.remove_insert(self.id)?);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, index, insert) = self.removed.take().ok_or_else(not_applied)?;
        memory.add_insert_at(
            track,
            index.min(memory.track(track)?.inserts().len()),
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

impl Command<Memory> for MoveInsert {
    fn label(&self) -> &str {
        "Move insert"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let track = memory.insert_owner(self.id)?;
        let from = memory
            .track(track)?
            .inserts()
            .iter()
            .position(|i| i.id() == self.id)
            .ok_or_else(not_applied)?;
        memory.move_insert(self.id, self.to)?;
        self.from = Some(from);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.move_insert(self.id, self.from.ok_or_else(not_applied)?)
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
    /// (`Memory::allocate_clip_id`) to know it before pushing.
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

impl Command<Memory> for AddClip {
    fn label(&self) -> &str {
        "Add clip"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let clip = self.clip.as_ref().ok_or_else(not_applied)?.clone();
        self.id = memory.add_clip(self.track, clip)?;
        self.clip = None;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, clip) = memory.remove_clip(self.id)?;
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

impl Command<Memory> for RemoveClip {
    fn label(&self) -> &str {
        "Remove clip"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.removed = Some(memory.remove_clip(self.id)?);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, clip) = self.removed.take().ok_or_else(not_applied)?;
        memory.add_clip(track, clip)?;
        Ok(())
    }
}

/// Archives a clip (keeps it, silent and off the timeline's end) or
/// restores it. One undo step each; never coalesces, so two quick flips
/// stay two steps.
pub struct ArchiveClip {
    id: ClipId,
    archived: bool,
    previous: Option<bool>,
}

impl ArchiveClip {
    /// `archived = true` archives, `false` restores.
    pub fn new(id: ClipId, archived: bool) -> Self {
        Self {
            id,
            archived,
            previous: None,
        }
    }
}

impl Command<Memory> for ArchiveClip {
    fn label(&self) -> &str {
        if self.archived {
            "Archive clip"
        } else {
            "Restore clip"
        }
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let clip = memory.clip_mut(self.id)?;
        self.previous = Some(clip.state.archived);
        clip.state.archived = self.archived;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let previous = self.previous.ok_or_else(not_applied)?;
        memory.clip_mut(self.id)?.state.archived = previous;
        Ok(())
    }
}

/// Archives a track (keeps it and its clips, but the mixer builds no strip
/// for it) or restores it. One undo step each; never coalesces.
pub struct ArchiveTrack {
    id: TrackId,
    archived: bool,
    previous: Option<bool>,
}

impl ArchiveTrack {
    /// `archived = true` archives, `false` restores.
    pub fn new(id: TrackId, archived: bool) -> Self {
        Self {
            id,
            archived,
            previous: None,
        }
    }
}

impl Command<Memory> for ArchiveTrack {
    fn label(&self) -> &str {
        if self.archived {
            "Archive track"
        } else {
            "Restore track"
        }
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let track = memory.track_mut(self.id)?;
        self.previous = Some(track.state.archived);
        track.state.archived = self.archived;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let previous = self.previous.ok_or_else(not_applied)?;
        memory.track_mut(self.id)?.state.archived = previous;
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

impl Command<Memory> for MoveClip {
    fn label(&self) -> &str {
        "Move clip"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let track = memory.clip_track(self.id)?;
        let start = memory.clip(self.id)?.start();
        if let Some(t) = self.track {
            memory.move_clip_to_track(self.id, t)?;
        }
        memory.move_clip(self.id, self.start)?;
        self.from = Some((track, start));
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (track, start) = self.from.ok_or_else(not_applied)?;
        memory.move_clip_to_track(self.id, track)?;
        memory.move_clip(self.id, start)
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::CLIP_MOVE, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
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

impl Command<Memory> for ResizeClip {
    fn label(&self) -> &str {
        "Resize clip"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let before = memory.clip(self.id)?.length();
        memory.resize_clip(self.id, self.length)?;
        self.from = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.resize_clip(self.id, self.from.ok_or_else(not_applied)?)
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::CLIP_RESIZE, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
        let next = next.downcast::<Self>()?;
        self.length = next.length;
        Ok(())
    }
}

/// Moves a clip's edges on the timeline. What is underneath stays put: a
/// head trim shows later audio (the source offset moves with the edge) and
/// MIDI notes keep their place on the timeline. Coalesces, so dragging an
/// edge is one undo step.
pub struct TrimClip {
    id: ClipId,
    start: TimePosition,
    end: TimePosition,
    from: Option<(TimePosition, TimeDuration, ClipSource)>,
}

impl TrimClip {
    /// Trims the clip to `[start, end)`.
    pub fn new(id: ClipId, start: TimePosition, end: TimePosition) -> Self {
        Self {
            id,
            start,
            end,
            from: None,
        }
    }
}

impl Command<Memory> for TrimClip {
    fn label(&self) -> &str {
        "Trim clip"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let length = TimeDuration::from_samples(self.end.samples() - self.start.samples());
        if length.samples() <= 0 {
            return Err(AuraError::InvalidArgument {
                what: "clip trim (end must be after start)",
            });
        }
        let clip = memory.clip(self.id)?;
        let before = (clip.start(), clip.length(), clip.source.clone());
        let moved = self.start.samples() - before.0.samples();
        let source = match &clip.source {
            ClipSource::Audio { path, offset } => ClipSource::Audio {
                path: path.clone(),
                offset: TimeDuration::from_samples((offset.samples() + moved).max(0)),
            },
            ClipSource::Midi { notes } if moved != 0 => {
                let tempo = memory.tempo_map();
                let delta = tempo.samples_to_beat(self.start).beats()
                    - tempo.samples_to_beat(before.0).beats();
                ClipSource::Midi {
                    notes: notes
                        .iter()
                        .map(|n| MidiNote {
                            beat: n.beat - delta,
                            ..*n
                        })
                        .collect(),
                }
            }
            other => other.clone(),
        };
        memory.move_clip(self.id, self.start)?;
        memory.resize_clip(self.id, length)?;
        memory.clip_mut(self.id)?.source = source;
        self.from = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (start, length, source) = self.from.take().ok_or_else(not_applied)?;
        memory.move_clip(self.id, start)?;
        memory.resize_clip(self.id, length)?;
        memory.clip_mut(self.id)?.source = source;
        Ok(())
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::CLIP_TRIM, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
        let next = next.downcast::<Self>()?;
        self.start = next.start;
        self.end = next.end;
        Ok(())
    }
}

/// Splits a clip in two at a timeline position. The left clip keeps the
/// clip's id and shrinks; the right one is new, starts at the split and
/// shows what was underneath (a later audio offset, or the notes from the
/// split on). Fades stay at the outer edges; gain, mute and archived are
/// copied. One undo step.
pub struct SplitClip {
    id: ClipId,
    at: TimePosition,
    right: ClipId,
    before: Option<(TimeDuration, ClipSource, ClipState)>,
}

impl SplitClip {
    /// Splits `id` at `at`; the new right-hand clip gets `right`, an id
    /// from `Memory::allocate_clip_id`.
    pub fn new(id: ClipId, at: TimePosition, right: ClipId) -> Self {
        Self {
            id,
            at,
            right,
            before: None,
        }
    }
}

impl Command<Memory> for SplitClip {
    fn label(&self) -> &str {
        "Split clip"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let track = memory.clip_track(self.id)?;
        let clip = memory.clip(self.id)?;
        if self.at <= clip.start() || self.at >= clip.end() {
            return Err(AuraError::InvalidArgument {
                what: "clip split (must fall inside the clip)",
            });
        }
        let before = (clip.length(), clip.source.clone(), clip.state.clone());
        let left_length = TimeDuration::from_samples(self.at.samples() - clip.start().samples());
        let right_length = TimeDuration::from_samples(clip.end().samples() - self.at.samples());
        let right_source = match &clip.source {
            ClipSource::Audio { path, offset } => ClipSource::Audio {
                path: path.clone(),
                offset: TimeDuration::from_samples(offset.samples() + left_length.samples()),
            },
            ClipSource::Midi { notes } => {
                let tempo = memory.tempo_map();
                let delta = tempo.samples_to_beat(self.at).beats()
                    - tempo.samples_to_beat(clip.start()).beats();
                ClipSource::Midi {
                    notes: notes
                        .iter()
                        .filter(|n| n.beat >= delta)
                        .map(|n| MidiNote {
                            beat: n.beat - delta,
                            ..*n
                        })
                        .collect(),
                }
            }
            ClipSource::Empty => ClipSource::Empty,
        };
        let mut right = Clip::with_id(
            self.right,
            clip.name.clone(),
            self.at,
            right_length,
            right_source,
        )?;
        right.state = ClipState {
            fade_in: TimeDuration::ZERO,
            ..clip.state.clone()
        };
        memory.resize_clip(self.id, left_length)?;
        memory.clip_mut(self.id)?.state.fade_out = TimeDuration::ZERO;
        memory.add_clip(track, right)?;
        self.before = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (length, source, state) = self.before.take().ok_or_else(not_applied)?;
        memory.remove_clip(self.right)?;
        memory.resize_clip(self.id, length)?;
        let left = memory.clip_mut(self.id)?;
        left.source = source;
        left.state = state;
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
    /// (`Memory::allocate_marker_id`) to know it before pushing.
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

impl Command<Memory> for AddMarker {
    fn label(&self) -> &str {
        "Add marker"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let marker = self.marker.as_ref().ok_or_else(not_applied)?.clone();
        self.id = memory.insert_marker(marker)?;
        self.marker = None;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        self.marker = Some(memory.remove_marker(self.id)?);
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

impl Command<Memory> for RemoveMarker {
    fn label(&self) -> &str {
        "Remove marker"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.removed = Some(memory.remove_marker(self.id)?);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.insert_marker(self.removed.take().ok_or_else(not_applied)?)?;
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

impl Command<Memory> for MoveMarker {
    fn label(&self) -> &str {
        "Move marker"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let before = memory.marker(self.id)?.position();
        memory.move_marker(self.id, self.position)?;
        self.from = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.move_marker(self.id, self.from.ok_or_else(not_applied)?)
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::MARKER_MOVE, self.id.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
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

impl Command<Memory> for SetTempo {
    fn label(&self) -> &str {
        "Set tempo"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let before = memory
            .tempo_map()
            .tempo_points()
            .iter()
            .find(|p| p.beat == self.beat)
            .map(|p| (p.bpm, p.ramp));
        let map = memory.tempo_map_mut();
        if self.ramp {
            map.add_tempo_ramp(self.beat, self.bpm)?;
        } else {
            map.add_tempo(self.beat, self.bpm)?;
        }
        self.previous = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let map = memory.tempo_map_mut();
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
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
        let next = next.downcast::<Self>()?;
        self.bpm = next.bpm;
        self.ramp = next.ramp;
        Ok(())
    }
}

/// Removes a tempo change; undo puts it back. The tempo at beat zero
/// cannot be removed.
pub struct RemoveTempo {
    beat: BeatPosition,
    previous: Option<(f64, bool)>,
}

impl RemoveTempo {
    /// Creates the command.
    pub fn new(beat: BeatPosition) -> Self {
        Self {
            beat,
            previous: None,
        }
    }
}

impl Command<Memory> for RemoveTempo {
    fn label(&self) -> &str {
        "Remove tempo change"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let point = memory
            .tempo_map()
            .tempo_points()
            .iter()
            .find(|p| p.beat == self.beat)
            .map(|p| (p.bpm, p.ramp))
            .ok_or(AuraError::NotFound {
                kind: "tempo change",
                id: self.beat.beats().to_bits(),
            })?;
        memory.tempo_map_mut().remove_tempo(self.beat)?;
        self.previous = Some(point);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let (bpm, ramp) = self.previous.ok_or_else(not_applied)?;
        let map = memory.tempo_map_mut();
        if ramp {
            map.add_tempo_ramp(self.beat, bpm)
        } else {
            map.add_tempo(self.beat, bpm)
        }
    }
}

/// Sets or removes the meter (time signature) from a bar on. Bar 1 always
/// has a meter and cannot be cleared. One undo step.
pub struct SetMeter {
    bar: u32,
    signature: Option<TimeSignature>,
    previous: Option<Option<TimeSignature>>,
}

impl SetMeter {
    /// `Some` sets the meter from `bar` (1-based) on, `None` removes the
    /// change at that bar.
    pub fn new(bar: u32, signature: Option<TimeSignature>) -> Self {
        Self {
            bar,
            signature,
            previous: None,
        }
    }
}

impl Command<Memory> for SetMeter {
    fn label(&self) -> &str {
        "Set meter"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let before = memory
            .tempo_map()
            .meter_points()
            .iter()
            .find(|m| m.bar == self.bar)
            .map(|m| m.signature);
        let map = memory.tempo_map_mut();
        match self.signature {
            Some(signature) => map.add_meter(self.bar, signature)?,
            None => map.remove_meter(self.bar)?,
        }
        self.previous = Some(before);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let map = memory.tempo_map_mut();
        match self.previous.ok_or_else(not_applied)? {
            Some(signature) => map.add_meter(self.bar, signature),
            None => map.remove_meter(self.bar),
        }
    }
}

/// A note is valid when it starts at or after the clip start, lasts,
/// and keeps its numbers in MIDI range.
fn check_note(note: &MidiNote) -> Result<()> {
    if note.beat.is_nan() || note.beat < 0.0 || note.length.is_nan() || note.length <= 0.0 {
        return Err(AuraError::InvalidArgument {
            what: "a note starts at or after the clip start and has a length",
        });
    }
    if note.note > 127 || note.velocity == 0 || note.velocity > 127 || note.channel > 15 {
        return Err(AuraError::InvalidArgument {
            what: "note 0..=127, velocity 1..=127, channel 0..=15",
        });
    }
    Ok(())
}

/// The notes of a MIDI clip, or an error for any other clip.
fn notes_mut(memory: &mut Memory, clip: ClipId) -> Result<&mut Vec<MidiNote>> {
    match &mut memory.clip_mut(clip)?.source {
        ClipSource::Midi { notes } => Ok(notes),
        _ => Err(AuraError::InvalidArgument {
            what: "only a MIDI clip holds notes",
        }),
    }
}

/// Where a note ends, as a clip-relative duration, given the clip's place
/// on the tempo map.
fn note_end(memory: &Memory, clip: ClipId, note: &MidiNote) -> Result<TimeDuration> {
    let start = memory.clip(clip)?.start();
    let tempo = memory.tempo_map();
    let start_beat = tempo.samples_to_beat(start).beats();
    let end = tempo.beat_to_samples(BeatPosition::new(start_beat + note.beat + note.length));
    Ok(TimeDuration::from_samples(end.samples() - start.samples()))
}

/// Grows the clip so `note` fits, remembering the length it had.
fn fit_note(memory: &mut Memory, clip: ClipId, note: &MidiNote) -> Result<Option<TimeDuration>> {
    let end = note_end(memory, clip, note)?;
    let length = memory.clip(clip)?.length();
    if end > length {
        memory.resize_clip(clip, end)?;
        return Ok(Some(length));
    }
    Ok(None)
}

/// Adds a note to a MIDI clip; a note that ends past the clip end extends
/// the clip. One undo step. Notes are identified by their content, so two
/// identical notes are one note twice.
pub struct AddNote {
    clip: ClipId,
    note: MidiNote,
    grew_from: Option<TimeDuration>,
}

impl AddNote {
    /// Creates the command.
    pub fn new(clip: ClipId, note: MidiNote) -> Self {
        Self {
            clip,
            note,
            grew_from: None,
        }
    }
}

impl Command<Memory> for AddNote {
    fn label(&self) -> &str {
        "Add note"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        check_note(&self.note)?;
        notes_mut(memory, self.clip)?;
        self.grew_from = fit_note(memory, self.clip, &self.note)?;
        let notes = notes_mut(memory, self.clip)?;
        notes.push(self.note);
        notes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let notes = notes_mut(memory, self.clip)?;
        let i = notes
            .iter()
            .position(|n| *n == self.note)
            .ok_or(AuraError::NotFound {
                kind: "note",
                id: u64::from(self.note.note),
            })?;
        notes.remove(i);
        if let Some(length) = self.grew_from.take() {
            memory.resize_clip(self.clip, length)?;
        }
        Ok(())
    }
}

/// Removes a note from a MIDI clip; undo puts it back. One undo step.
pub struct RemoveNote {
    clip: ClipId,
    note: MidiNote,
}

impl RemoveNote {
    /// Creates the command.
    pub fn new(clip: ClipId, note: MidiNote) -> Self {
        Self { clip, note }
    }
}

impl Command<Memory> for RemoveNote {
    fn label(&self) -> &str {
        "Remove note"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        let notes = notes_mut(memory, self.clip)?;
        let i = notes
            .iter()
            .position(|n| *n == self.note)
            .ok_or(AuraError::NotFound {
                kind: "note",
                id: u64::from(self.note.note),
            })?;
        notes.remove(i);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let notes = notes_mut(memory, self.clip)?;
        notes.push(self.note);
        notes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        Ok(())
    }
}

/// Replaces one note of a MIDI clip with another: moved, lengthened,
/// louder, on another channel, whatever differs. A note that ends past the
/// clip end extends the clip. Coalesces per clip, so a drag is one undo
/// step; the merge only folds changes to the same note into one command.
pub struct ChangeNote {
    clip: ClipId,
    from: MidiNote,
    to: MidiNote,
    grew_from: Option<TimeDuration>,
}

impl ChangeNote {
    /// Creates the command.
    pub fn new(clip: ClipId, from: MidiNote, to: MidiNote) -> Self {
        Self {
            clip,
            from,
            to,
            grew_from: None,
        }
    }
}

impl Command<Memory> for ChangeNote {
    fn label(&self) -> &str {
        "Change note"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        check_note(&self.to)?;
        let notes = notes_mut(memory, self.clip)?;
        let i = notes
            .iter()
            .position(|n| *n == self.from)
            .ok_or(AuraError::NotFound {
                kind: "note",
                id: u64::from(self.from.note),
            })?;
        notes[i] = self.to;
        notes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        self.grew_from = fit_note(memory, self.clip, &self.to)?;
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let notes = notes_mut(memory, self.clip)?;
        let i = notes
            .iter()
            .position(|n| *n == self.to)
            .ok_or(AuraError::NotFound {
                kind: "note",
                id: u64::from(self.to.note),
            })?;
        notes[i] = self.from;
        notes.sort_by(|a, b| a.beat.total_cmp(&b.beat));
        if let Some(length) = self.grew_from.take() {
            memory.resize_clip(self.clip, length)?;
        }
        Ok(())
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::CLIP_NOTE, self.clip.raw()))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
        // Only the same note, still moving: another note's change in the
        // same clip stays its own command.
        if !next.is::<Self>() {
            return Err(next);
        }
        let other = next.downcast::<Self>()?;
        if other.from != self.to {
            return Err(other);
        }
        self.to = other.to;
        if self.grew_from.is_none() {
            self.grew_from = other.grew_from;
        }
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

impl Command<Memory> for SetLoopRange {
    fn label(&self) -> &str {
        "Set loop range"
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.previous = Some(memory.loop_range());
        memory.set_loop_range(self.range);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        memory.set_loop_range(self.previous.ok_or_else(not_applied)?);
        Ok(())
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::LOOP, 0))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
        let next = next.downcast::<Self>()?;
        self.range = next.range;
        Ok(())
    }
}

/// Which punch point a [`SetPunch`] sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PunchPoint {
    /// Where a recording drops in.
    In,
    /// Where a recording drops out.
    Out,
}

/// Sets or clears a punch point. Coalesces per point, like a drag.
pub struct SetPunch {
    point: PunchPoint,
    position: Option<TimePosition>,
    previous: Option<Option<TimePosition>>,
}

impl SetPunch {
    /// Creates the command.
    pub fn new(point: PunchPoint, position: Option<TimePosition>) -> Self {
        Self {
            point,
            position,
            previous: None,
        }
    }

    fn read(&self, memory: &Memory) -> Option<TimePosition> {
        match self.point {
            PunchPoint::In => memory.punch_in(),
            PunchPoint::Out => memory.punch_out(),
        }
    }

    fn write(&self, memory: &mut Memory, position: Option<TimePosition>) {
        match self.point {
            PunchPoint::In => memory.set_punch_in(position),
            PunchPoint::Out => memory.set_punch_out(position),
        }
    }
}

impl Command<Memory> for SetPunch {
    fn label(&self) -> &str {
        match self.point {
            PunchPoint::In => "Set punch in",
            PunchPoint::Out => "Set punch out",
        }
    }

    fn apply(&mut self, memory: &mut Memory) -> Result<()> {
        self.previous = Some(self.read(memory));
        self.write(memory, self.position);
        Ok(())
    }

    fn revert(&mut self, memory: &mut Memory) -> Result<()> {
        let previous = self.previous.ok_or_else(not_applied)?;
        self.write(memory, previous);
        Ok(())
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        Some(CoalesceKey::new(keys::PUNCH, self.point as u64))
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<Memory>>,
    ) -> core::result::Result<(), Box<dyn Command<Memory>>> {
        let next = next.downcast::<Self>()?;
        self.position = next.position;
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
) -> PropertyEdit<Memory, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Memory| s.track(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Memory, v| set(s.track_mut(id)?, v),
    )
}

fn clip_property<V: Clone + Send + Sync + 'static>(
    label: &'static str,
    kind: u64,
    id: ClipId,
    value: V,
    get: impl Fn(&Clip) -> V + Send + Sync + 'static,
    set: impl Fn(&mut Clip, V) + Send + Sync + 'static,
) -> PropertyEdit<Memory, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Memory| s.clip(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Memory, v| {
            set(s.clip_mut(id)?, v);
            Ok(())
        },
    )
}

/// Renames the session (the memory's name).
pub fn set_session_name(name: impl Into<String>) -> PropertyEdit<Memory, String> {
    PropertyEdit::new(
        "Rename session",
        Some(CoalesceKey::new(keys::SESSION_NAME, 0)),
        name.into(),
        |s: &Memory| s.name().to_owned(),
        |s: &mut Memory, v| {
            s.set_name(v);
            Ok(())
        },
    )
}

/// Sets the metronome's level in dB.
pub fn set_metronome_level_db(level_db: f32) -> PropertyEdit<Memory, f32> {
    PropertyEdit::new(
        "Set metronome level",
        Some(CoalesceKey::new(keys::METRONOME_LEVEL, 0)),
        level_db,
        |m: &Memory| m.metronome().level_db,
        |m: &mut Memory, v| {
            m.metronome_mut().level_db = v;
            Ok(())
        },
    )
}

/// Renames a track.
pub fn set_track_name(id: TrackId, name: impl Into<String>) -> PropertyEdit<Memory, String> {
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
pub fn set_track_volume_db(id: TrackId, gain_db: f32) -> PropertyEdit<Memory, f32> {
    track_property(
        "Set track gain",
        keys::TRACK_VOLUME,
        id,
        gain_db,
        |t| t.state.volume_db,
        |t, v| {
            if !v.is_finite() {
                return Err(AuraError::InvalidArgument { what: "gain_db" });
            }
            t.state.volume_db = v;
            Ok(())
        },
    )
}

/// Sets a track's pan (`-1.0..=1.0`).
pub fn set_track_pan(id: TrackId, pan: f32) -> PropertyEdit<Memory, f32> {
    track_property(
        "Set track pan",
        keys::TRACK_PAN,
        id,
        pan,
        |t| t.state.pan,
        |t, v| {
            if !(-1.0..=1.0).contains(&v) {
                return Err(AuraError::InvalidArgument { what: "pan" });
            }
            t.state.pan = v;
            Ok(())
        },
    )
}

/// Mutes or unmutes a track.
pub fn set_track_mute(id: TrackId, muted: bool) -> PropertyEdit<Memory, bool> {
    track_property(
        "Mute track",
        keys::TRACK_MUTE,
        id,
        muted,
        |t| t.state.mute,
        |t, v| {
            t.state.mute = v;
            Ok(())
        },
    )
}

/// Solos or unsolos a track.
pub fn set_track_solo(id: TrackId, soloed: bool) -> PropertyEdit<Memory, bool> {
    track_property(
        "Solo track",
        keys::TRACK_SOLO,
        id,
        soloed,
        |t| t.state.solo,
        |t, v| {
            t.state.solo = v;
            Ok(())
        },
    )
}

/// Arms or disarms a track for recording.
pub fn set_track_arm(id: TrackId, arm: bool) -> PropertyEdit<Memory, bool> {
    track_property(
        "Arm track",
        keys::TRACK_ARM,
        id,
        arm,
        |t| t.state.arm,
        |t, v| {
            t.state.arm = v;
            Ok(())
        },
    )
}

/// Turns a MIDI track's live monitoring on or off.
pub fn set_track_monitor(id: TrackId, monitor: bool) -> PropertyEdit<Memory, bool> {
    track_property(
        "Monitor track",
        keys::TRACK_MONITOR,
        id,
        monitor,
        |t| t.state.monitor,
        |t, v| {
            t.state.monitor = v;
            Ok(())
        },
    )
}

/// Sets a track's colour.
pub fn set_track_color(id: TrackId, color: Option<u32>) -> PropertyEdit<Memory, Option<u32>> {
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
) -> PropertyEdit<Memory, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Memory| s.send(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Memory, v| set(s.send_mut(id)?, v),
    )
}

/// Sets a send's level in dB. Rejects non-finite values. Coalesces.
pub fn set_send_level_db(id: SendId, level_db: f32) -> PropertyEdit<Memory, f32> {
    send_property(
        "Set send level",
        keys::SEND_LEVEL,
        id,
        level_db,
        |s| s.state.level_db,
        |s, v| {
            if !v.is_finite() {
                return Err(AuraError::InvalidArgument { what: "level_db" });
            }
            s.state.level_db = v;
            Ok(())
        },
    )
}

/// Enables or disables a send.
pub fn set_send_enabled(id: SendId, enabled: bool) -> PropertyEdit<Memory, bool> {
    send_property(
        "Enable send",
        keys::SEND_ENABLED,
        id,
        enabled,
        |s| s.state.enabled,
        |s, v| {
            s.state.enabled = v;
            Ok(())
        },
    )
}

/// Switches a send between pre-fader (`true`) and post-fader.
pub fn set_send_pre_fader(id: SendId, pre_fader: bool) -> PropertyEdit<Memory, bool> {
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
) -> PropertyEdit<Memory, V> {
    let fallback = value.clone();
    PropertyEdit::new(
        label,
        Some(CoalesceKey::new(kind, id.raw())),
        value,
        move |s: &Memory| s.insert(id).map(&get).unwrap_or_else(|_| fallback.clone()),
        move |s: &mut Memory, v| {
            set(s.insert_mut(id)?, v);
            Ok(())
        },
    )
}

/// Bypasses or re-enables an insert.
pub fn set_insert_bypass(id: InsertId, bypass: bool) -> PropertyEdit<Memory, bool> {
    insert_property(
        "Bypass insert",
        keys::INSERT_BYPASS,
        id,
        bypass,
        |i| i.state.bypass,
        |i, v| i.state.bypass = v,
    )
}

/// Sets one plugin parameter on an insert. Consecutive edits of the same
/// parameter coalesce (a knob drag is one undo step); undoing an edit of a
/// parameter that had no stored value clears it again.
pub fn set_insert_param(id: InsertId, param: u32, value: f64) -> PropertyEdit<Memory, Option<f64>> {
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
pub fn set_clip_name(id: ClipId, name: impl Into<String>) -> PropertyEdit<Memory, String> {
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
pub fn set_clip_gain_db(id: ClipId, gain_db: f32) -> PropertyEdit<Memory, f32> {
    clip_property(
        "Set clip gain",
        keys::CLIP_GAIN,
        id,
        gain_db,
        |i| i.state.gain_db,
        |i, v| i.state.gain_db = v,
    )
}

/// Mutes or unmutes a clip.
pub fn set_clip_mute(id: ClipId, muted: bool) -> PropertyEdit<Memory, bool> {
    clip_property(
        "Mute clip",
        keys::CLIP_MUTE,
        id,
        muted,
        |i| i.state.mute,
        |i, v| i.state.mute = v,
    )
}

/// Sets a clip's fade-in length.
pub fn set_clip_fade_in(id: ClipId, length: TimeDuration) -> PropertyEdit<Memory, TimeDuration> {
    clip_property(
        "Set clip fade in",
        keys::CLIP_FADE_IN,
        id,
        length,
        |i| i.state.fade_in,
        |i, v| i.state.fade_in = v,
    )
}

/// Sets a clip's fade-out length.
pub fn set_clip_fade_out(id: ClipId, length: TimeDuration) -> PropertyEdit<Memory, TimeDuration> {
    clip_property(
        "Set clip fade out",
        keys::CLIP_FADE_OUT,
        id,
        length,
        |i| i.state.fade_out,
        |i, v| i.state.fade_out = v,
    )
}

/// Renames a marker.
pub fn set_marker_name(id: MarkerId, name: impl Into<String>) -> PropertyEdit<Memory, String> {
    let name = name.into();
    let fallback = name.clone();
    PropertyEdit::new(
        "Rename marker",
        Some(CoalesceKey::new(keys::MARKER_NAME, id.raw())),
        name,
        move |s: &Memory| {
            s.marker(id)
                .map(|m| m.name.clone())
                .unwrap_or_else(|_| fallback.clone())
        },
        move |s: &mut Memory, v| {
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

    fn base() -> (Memory, UndoStack<Memory>, TrackId, ClipId) {
        let mut s = Memory::new("t", R);
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

        // Without a pre-allocated id the memory assigns one; redo keeps it.
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
        h.push(&mut s, set_session_name("memory")).unwrap();
        h.push(&mut s, set_track_name(a, "vox")).unwrap();
        h.push(&mut s, set_track_volume_db(a, -3.0)).unwrap();
        h.push(&mut s, set_track_volume_db(a, -6.0)).unwrap();
        assert_eq!(h.undo_len(), 3);
        assert!(h.push(&mut s, set_track_volume_db(a, f32::NAN)).is_err());
        assert!(h.push(&mut s, set_track_pan(a, 1.5)).is_err());
        h.push(&mut s, set_track_pan(a, -0.5)).unwrap();
        h.push(&mut s, set_track_mute(a, true)).unwrap();
        h.push(&mut s, set_track_solo(a, true)).unwrap();
        h.push(&mut s, set_track_arm(a, true)).unwrap();
        h.push(&mut s, set_track_color(a, Some(0x123456))).unwrap();
        h.push(&mut s, set_clip_name(clip, "take")).unwrap();
        h.push(&mut s, set_clip_gain_db(clip, -2.0)).unwrap();
        h.push(&mut s, set_clip_mute(clip, true)).unwrap();
        let t = s.track(a).unwrap();
        assert_eq!(
            (
                t.name.as_str(),
                t.state.volume_db,
                t.state.pan,
                t.state.mute,
                t.state.solo,
                t.state.arm,
                t.color
            ),
            ("vox", -6.0, -0.5, true, true, true, Some(0x123456))
        );
        let i = s.clip(clip).unwrap();
        assert_eq!(
            (i.name.as_str(), i.state.gain_db, i.state.mute),
            ("take", -2.0, true)
        );
        assert_eq!(s.name(), "memory");
        while h.undo(&mut s).unwrap() {}
        let t = s.track(a).unwrap();
        assert_eq!(
            (
                t.name.as_str(),
                t.state.volume_db,
                t.state.pan,
                t.state.mute,
                t.state.solo,
                t.state.arm,
                t.color
            ),
            ("a", 0.0, 0.0, false, false, false, None)
        );
        let i = s.clip(clip).unwrap();
        assert_eq!(
            (i.name.as_str(), i.state.gain_db, i.state.mute),
            ("i", 0.0, false)
        );
        assert_eq!(s.name(), "t");
        while h.redo(&mut s).unwrap() {}
        assert_eq!(s.track(a).unwrap().state.volume_db, -6.0);
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
            (send.state.level_db, send.state.enabled, send.pre_fader),
            (-9.0, false, true)
        );
        h.undo(&mut s).unwrap();
        h.undo(&mut s).unwrap();
        h.undo(&mut s).unwrap();
        let send = s.send(id).unwrap();
        assert_eq!(
            (send.state.level_db, send.state.enabled, send.pre_fader),
            (-3.0, true, false),
            "level edits were one step"
        );
        while h.redo(&mut s).unwrap() {}
        assert_eq!(s.send(id).unwrap().state.level_db, -9.0);
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
        assert_eq!(s.send(id).unwrap().state.level_db, -9.0);
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
        let chain = |s: &Memory| -> Vec<InsertId> {
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

        h.push(&mut s, set_insert_bypass(eq, true)).unwrap();
        assert!(s.insert(eq).unwrap().state.bypass);
        h.push(&mut s, RemoveInsert::new(gate)).unwrap();
        assert_eq!(chain(&s), [comp, eq]);
        h.undo(&mut s).unwrap();
        assert_eq!(chain(&s), [comp, gate, eq], "restored at its old position");
        assert!(s.insert(eq).unwrap().state.bypass);
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
