//! The [`Session`] document.

use crate::primitives::transport::{LoopRange, TempoMap};
use crate::primitives::{
    ClipId, InsertId, MarkerId, RawId, Result, SampleRate, SendId, SuperbError, TimeDuration,
    TimePosition, TrackId,
};

use crate::primitives::session::{Clip, Insert, Marker, Track, TrackKind, TrackSend};

/// A session: tracks, clips, markers, tempo map and loop range.
///
/// All IDs are allocated from one per-session counter and never reused, so
/// a `TrackId`, `ClipId`, `MarkerId` or `SendId` stays valid for the life
/// of the document (or until its object is removed). Objects can also be
/// inserted with pre-allocated IDs (`allocate_*_id` + `insert_*`), which is
/// how undo/redo restores removed objects under their original IDs.
///
/// Routing is part of the document: every track has an
/// [`output`](Track::output) (a bus track, or `None` for the master) and
/// audio/MIDI tracks carry [`TrackSend`]s to bus tracks. Both are validated
/// here (targets must be buses; bus → bus chains may not form a cycle), and
/// removing a bus detaches everything routed into it.
///
/// Every mutating method bumps [`revision`](Session::revision), so views can
/// cheaply detect "something changed".
#[derive(Clone, Debug)]
pub struct Session {
    name: String,
    sample_rate: SampleRate,
    tempo_map: TempoMap,
    loop_range: Option<LoopRange>,
    tracks: Vec<Track>,
    markers: Vec<Marker>,
    next_id: RawId,
    revision: u64,
}

impl Session {
    /// Creates an empty session at 120 bpm in 4/4.
    pub fn new(name: impl Into<String>, sample_rate: SampleRate) -> Self {
        Self {
            name: name.into(),
            sample_rate,
            tempo_map: TempoMap::new(sample_rate, 120.0),
            loop_range: None,
            tracks: Vec::new(),
            markers: Vec::new(),
            next_id: 1,
            revision: 0,
        }
    }

    // ------------------------------------------------------------------
    // Document-level properties
    // ------------------------------------------------------------------

    /// Session name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Renames the session.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
        self.touch();
    }

    /// Session sample rate.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// Changes the sample rate. Positions stay in samples (they are *not*
    /// rescaled); the tempo map follows the new rate.
    pub fn set_sample_rate(&mut self, rate: SampleRate) {
        self.sample_rate = rate;
        self.tempo_map.set_sample_rate(rate);
        self.touch();
    }

    /// The tempo/meter map.
    pub fn tempo_map(&self) -> &TempoMap {
        &self.tempo_map
    }

    /// Mutable tempo map (bumps the revision).
    pub fn tempo_map_mut(&mut self) -> &mut TempoMap {
        self.touch();
        &mut self.tempo_map
    }

    /// The loop range, if any.
    pub fn loop_range(&self) -> Option<LoopRange> {
        self.loop_range
    }

    /// Sets or clears the loop range.
    pub fn set_loop_range(&mut self, range: Option<LoopRange>) {
        self.loop_range = range;
        self.touch();
    }

    /// Monotonic counter bumped by every mutation.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// End of the last clip on any track (zero for an empty session).
    pub fn end_position(&self) -> TimePosition {
        self.tracks
            .iter()
            .map(Track::end)
            .max()
            .unwrap_or(TimePosition::ZERO)
    }

    /// Total number of clips across all tracks.
    pub fn clip_count(&self) -> usize {
        self.tracks.iter().map(|t| t.clips.len()).sum()
    }

    // ------------------------------------------------------------------
    // IDs
    // ------------------------------------------------------------------

    /// Reserves a track ID for a later [`insert_track`](Self::insert_track).
    pub fn allocate_track_id(&mut self) -> TrackId {
        self.allocate()
    }

    /// Reserves a clip ID for a later [`add_clip`](Self::add_clip).
    pub fn allocate_clip_id(&mut self) -> ClipId {
        self.allocate()
    }

    /// Reserves a marker ID for a later [`insert_marker`](Self::insert_marker).
    pub fn allocate_marker_id(&mut self) -> MarkerId {
        self.allocate()
    }

    /// Reserves a send ID for a later [`add_send`](Self::add_send).
    pub fn allocate_send_id(&mut self) -> SendId {
        self.allocate()
    }

    /// Reserves an insert ID for a later [`add_insert`](Self::add_insert).
    pub fn allocate_insert_id(&mut self) -> InsertId {
        self.allocate()
    }

    /// Next raw ID the session will hand out.
    pub fn next_raw_id(&self) -> RawId {
        self.next_id
    }

    fn allocate<T: From<RawId>>(&mut self) -> T {
        let id = self.next_id;
        self.next_id += 1;
        T::from(id)
    }

    /// Makes sure a pre-existing raw ID is never handed out again.
    pub(crate) fn reserve(&mut self, raw: RawId) {
        if raw >= self.next_id {
            self.next_id = raw + 1;
        }
    }

    fn touch(&mut self) {
        self.revision += 1;
    }

    // ------------------------------------------------------------------
    // Tracks
    // ------------------------------------------------------------------

    /// Appends a new track and returns its ID.
    pub fn add_track(&mut self, name: impl Into<String>, kind: TrackKind) -> TrackId {
        let id = self.allocate_track_id();
        self.insert_track(self.tracks.len(), Track::with_id(id, name, kind))
            .expect("fresh id, valid index")
    }

    /// Inserts `track` at `index`. A track without an ID gets a fresh one;
    /// a track with an ID must not collide with an existing track, and its
    /// clips and sends must carry unique IDs as well. The track's
    /// [`output`](Track::output) and send destinations must be existing bus
    /// tracks (bus tracks carry no clips and no sends). Returns the track's
    /// ID.
    pub fn insert_track(&mut self, index: usize, mut track: Track) -> Result<TrackId> {
        if index > self.tracks.len() {
            return Err(SuperbError::InvalidArgument {
                what: "track index",
            });
        }
        if track.id.is_valid() && self.tracks.iter().any(|t| t.id == track.id) {
            return Err(SuperbError::InvalidArgument {
                what: "duplicate track id",
            });
        }
        // Validate everything before touching the ID counter so a rejected
        // insert leaves the document untouched.
        if track.kind == TrackKind::Bus && !track.clips.is_empty() {
            return Err(SuperbError::InvalidArgument {
                what: "bus tracks hold no clips",
            });
        }
        if track.kind == TrackKind::Bus && !track.sends.is_empty() {
            return Err(SuperbError::InvalidArgument {
                what: "bus tracks have no sends",
            });
        }
        if let Some(bus) = track.output {
            self.check_bus(bus)?;
        }
        for i in 0..track.clips.len() {
            let id = track.clips[i].id;
            if id.is_valid()
                && (self.find_clip(id).is_some() || track.clips[..i].iter().any(|o| o.id == id))
            {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate clip id",
                });
            }
        }
        for i in 0..track.sends.len() {
            let id = track.sends[i].id;
            self.check_bus(track.sends[i].bus)?;
            if id.is_valid()
                && (self.find_send(id).is_some() || track.sends[..i].iter().any(|o| o.id == id))
            {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate send id",
                });
            }
        }
        for i in 0..track.inserts.len() {
            let id = track.inserts[i].id;
            if id.is_valid()
                && (self.find_insert(id).is_some() || track.inserts[..i].iter().any(|o| o.id == id))
            {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate insert id",
                });
            }
        }

        if track.id.is_valid() {
            self.reserve(track.id.raw());
        } else {
            track.id = self.allocate_track_id();
        }
        for clip in &mut track.clips {
            if clip.id.is_valid() {
                self.reserve(clip.id.raw());
            } else {
                clip.id = self.allocate_clip_id();
            }
        }
        for send in &mut track.sends {
            if send.id.is_valid() {
                self.reserve(send.id.raw());
            } else {
                send.id = self.allocate_send_id();
            }
        }
        for insert in &mut track.inserts {
            if insert.id.is_valid() {
                self.reserve(insert.id.raw());
            } else {
                insert.id = self.allocate_insert_id();
            }
        }
        track.sort_clips();
        let id = track.id;
        self.tracks.insert(index, track);
        self.touch();
        Ok(id)
    }

    /// Removes a track (with its clips and sends) and returns it.
    ///
    /// Removing a bus detaches its inputs: tracks whose
    /// [`output`](Track::output) was the bus fall back to the master
    /// (`None`) and sends into it are dropped. Capture them first with
    /// [`tracks_routed_to`](Self::tracks_routed_to) /
    /// [`sends_to`](Self::sends_to) if you need to restore them (the
    /// [`RemoveTrack`](crate::primitives::session::commands::RemoveTrack) command does).
    pub fn remove_track(&mut self, id: TrackId) -> Result<Track> {
        let idx = self.track_index(id)?;
        for t in &mut self.tracks {
            if t.output == Some(id) {
                t.output = None;
            }
            t.sends.retain(|s| s.bus != id);
        }
        self.touch();
        Ok(self.tracks.remove(idx))
    }

    /// Moves a track to `index` in the track order.
    pub fn move_track(&mut self, id: TrackId, index: usize) -> Result<()> {
        let from = self.track_index(id)?;
        if index >= self.tracks.len() {
            return Err(SuperbError::InvalidArgument {
                what: "track index",
            });
        }
        if from != index {
            let t = self.tracks.remove(from);
            self.tracks.insert(index, t);
            self.touch();
        }
        Ok(())
    }

    /// Position of a track in the track order.
    pub fn track_index(&self, id: TrackId) -> Result<usize> {
        self.tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "track",
                id: id.raw(),
            })
    }

    /// Looks up a track.
    pub fn track(&self, id: TrackId) -> Result<&Track> {
        let i = self.track_index(id)?;
        Ok(&self.tracks[i])
    }

    /// Mutable track access (bumps the revision).
    pub fn track_mut(&mut self, id: TrackId) -> Result<&mut Track> {
        let i = self.track_index(id)?;
        self.touch();
        Ok(&mut self.tracks[i])
    }

    /// All tracks in order.
    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Number of tracks.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    // ------------------------------------------------------------------
    // Routing
    // ------------------------------------------------------------------

    /// Routes a track's (or bus's) output into a bus track, or to the
    /// master with `None`.
    ///
    /// Fails if `bus` is unknown, not a [`TrackKind::Bus`], the track
    /// itself, or would close a bus → bus cycle. A no-op change does not
    /// bump the revision.
    pub fn set_track_output(&mut self, track: TrackId, bus: Option<TrackId>) -> Result<()> {
        let idx = self.track_index(track)?;
        if let Some(bus) = bus {
            self.check_bus(bus)?;
            if bus == track || self.feeds(bus, track) {
                return Err(SuperbError::Cycle);
            }
        }
        if self.tracks[idx].output != bus {
            self.tracks[idx].output = bus;
            self.touch();
        }
        Ok(())
    }

    /// `true` if following `from`'s output chain reaches `target`.
    fn feeds(&self, from: TrackId, target: TrackId) -> bool {
        let mut cursor = Some(from);
        let mut hops = 0;
        while let Some(id) = cursor {
            if id == target {
                return true;
            }
            hops += 1;
            if hops > self.tracks.len() {
                return true; // defensive: an existing cycle counts as reaching anything
            }
            cursor = self.track(id).ok().and_then(Track::output);
        }
        false
    }

    fn check_bus(&self, bus: TrackId) -> Result<()> {
        if self.track(bus)?.kind != TrackKind::Bus {
            return Err(SuperbError::InvalidArgument {
                what: "routing target is not a bus track",
            });
        }
        Ok(())
    }

    /// Tracks (and buses) whose output is `bus`, in track order.
    pub fn tracks_routed_to(&self, bus: TrackId) -> impl Iterator<Item = TrackId> + '_ {
        self.tracks
            .iter()
            .filter(move |t| t.output == Some(bus))
            .map(Track::id)
    }

    /// Adds a send from an audio/MIDI track to the bus named by
    /// `send.bus()`. A send without an ID gets a fresh one; a send with an
    /// ID must not collide. Bus tracks cannot send.
    pub fn add_send(&mut self, track: TrackId, mut send: TrackSend) -> Result<SendId> {
        let ti = self.track_index(track)?;
        if self.tracks[ti].kind == TrackKind::Bus {
            return Err(SuperbError::InvalidArgument {
                what: "bus tracks have no sends",
            });
        }
        self.check_bus(send.bus)?;
        if send.id.is_valid() {
            if self.find_send(send.id).is_some() {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate send id",
                });
            }
            self.reserve(send.id.raw());
        } else {
            send.id = self.allocate_send_id();
        }
        let id = send.id;
        self.tracks[ti].sends.push(send);
        self.touch();
        Ok(id)
    }

    /// Removes a send, returning the track it was on and the send.
    pub fn remove_send(&mut self, id: SendId) -> Result<(TrackId, TrackSend)> {
        let (ti, si) = self.find_send(id).ok_or(SuperbError::NotFound {
            kind: "send",
            id: id.raw(),
        })?;
        let track = self.tracks[ti].id;
        let send = self.tracks[ti].sends.remove(si);
        self.touch();
        Ok((track, send))
    }

    /// Looks up a send on any track.
    pub fn send(&self, id: SendId) -> Result<&TrackSend> {
        let (ti, si) = self.find_send(id).ok_or(SuperbError::NotFound {
            kind: "send",
            id: id.raw(),
        })?;
        Ok(&self.tracks[ti].sends[si])
    }

    /// Mutable send access (bumps the revision). The destination is fixed;
    /// remove and re-add to retarget.
    pub fn send_mut(&mut self, id: SendId) -> Result<&mut TrackSend> {
        let (ti, si) = self.find_send(id).ok_or(SuperbError::NotFound {
            kind: "send",
            id: id.raw(),
        })?;
        self.touch();
        Ok(&mut self.tracks[ti].sends[si])
    }

    /// The track a send belongs to.
    pub fn send_track(&self, id: SendId) -> Result<TrackId> {
        let (ti, _) = self.find_send(id).ok_or(SuperbError::NotFound {
            kind: "send",
            id: id.raw(),
        })?;
        Ok(self.tracks[ti].id)
    }

    /// All sends into `bus` as `(source track, send)` pairs, in track order.
    pub fn sends_to(&self, bus: TrackId) -> impl Iterator<Item = (TrackId, &TrackSend)> + '_ {
        self.tracks.iter().flat_map(move |t| {
            t.sends
                .iter()
                .filter(move |s| s.bus == bus)
                .map(move |s| (t.id, s))
        })
    }

    /// Total number of sends across all tracks.
    pub fn send_count(&self) -> usize {
        self.tracks.iter().map(|t| t.sends.len()).sum()
    }

    fn find_send(&self, id: SendId) -> Option<(usize, usize)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.sends.iter().position(|s| s.id == id).map(|si| (ti, si)))
    }

    // ------------------------------------------------------------------
    // Inserts
    // ------------------------------------------------------------------

    /// Appends an insert to a track's chain. Any track kind may carry
    /// inserts. An insert without an ID gets a fresh one; an insert with an
    /// ID must not collide.
    pub fn add_insert(&mut self, track: TrackId, insert: Insert) -> Result<InsertId> {
        let ti = self.track_index(track)?;
        let end = self.tracks[ti].inserts.len();
        self.place_insert(ti, end, insert)
    }

    /// Puts an insert into a track's chain at `index` (`0` is the first
    /// effect the signal meets).
    pub fn add_insert_at(
        &mut self,
        track: TrackId,
        index: usize,
        insert: Insert,
    ) -> Result<InsertId> {
        let ti = self.track_index(track)?;
        if index > self.tracks[ti].inserts.len() {
            return Err(SuperbError::InvalidArgument {
                what: "insert index",
            });
        }
        self.place_insert(ti, index, insert)
    }

    fn place_insert(&mut self, ti: usize, index: usize, mut insert: Insert) -> Result<InsertId> {
        if insert.id.is_valid() {
            if self.find_insert(insert.id).is_some() {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate insert id",
                });
            }
            self.reserve(insert.id.raw());
        } else {
            insert.id = self.allocate_insert_id();
        }
        let id = insert.id;
        self.tracks[ti].inserts.insert(index, insert);
        self.touch();
        Ok(id)
    }

    /// Removes an insert, returning its track, its position in the chain
    /// and the insert itself.
    pub fn remove_insert(&mut self, id: InsertId) -> Result<(TrackId, usize, Insert)> {
        let (ti, ii) = self.find_insert(id).ok_or(SuperbError::NotFound {
            kind: "insert",
            id: id.raw(),
        })?;
        let track = self.tracks[ti].id;
        let insert = self.tracks[ti].inserts.remove(ii);
        self.touch();
        Ok((track, ii, insert))
    }

    /// Moves an insert to `index` within its track's chain.
    pub fn move_insert(&mut self, id: InsertId, index: usize) -> Result<()> {
        let (ti, ii) = self.find_insert(id).ok_or(SuperbError::NotFound {
            kind: "insert",
            id: id.raw(),
        })?;
        let chain = &mut self.tracks[ti].inserts;
        if index >= chain.len() {
            return Err(SuperbError::InvalidArgument {
                what: "insert index",
            });
        }
        if ii != index {
            let insert = chain.remove(ii);
            chain.insert(index, insert);
            self.touch();
        }
        Ok(())
    }

    /// Looks up an insert on any track.
    pub fn insert(&self, id: InsertId) -> Result<&Insert> {
        let (ti, ii) = self.find_insert(id).ok_or(SuperbError::NotFound {
            kind: "insert",
            id: id.raw(),
        })?;
        Ok(&self.tracks[ti].inserts[ii])
    }

    /// Mutable insert access (bumps the revision).
    pub fn insert_mut(&mut self, id: InsertId) -> Result<&mut Insert> {
        let (ti, ii) = self.find_insert(id).ok_or(SuperbError::NotFound {
            kind: "insert",
            id: id.raw(),
        })?;
        self.touch();
        Ok(&mut self.tracks[ti].inserts[ii])
    }

    /// The track whose chain holds `id`.
    pub fn insert_owner(&self, id: InsertId) -> Result<TrackId> {
        let (ti, _) = self.find_insert(id).ok_or(SuperbError::NotFound {
            kind: "insert",
            id: id.raw(),
        })?;
        Ok(self.tracks[ti].id)
    }

    /// Total number of inserts across all tracks.
    pub fn insert_count(&self) -> usize {
        self.tracks.iter().map(|t| t.inserts.len()).sum()
    }

    fn find_insert(&self, id: InsertId) -> Option<(usize, usize)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.inserts.iter().position(|i| i.id == id).map(|ii| (ti, ii)))
    }

    // ------------------------------------------------------------------
    // Clips
    // ------------------------------------------------------------------

    /// Adds a clip to a track and returns its ID. A clip without an ID
    /// gets a fresh one; a clip with an ID must not collide. Bus tracks
    /// hold no clips.
    pub fn add_clip(&mut self, track: TrackId, mut clip: Clip) -> Result<ClipId> {
        let ti = self.track_index(track)?;
        if self.tracks[ti].kind == TrackKind::Bus {
            return Err(SuperbError::InvalidArgument {
                what: "bus tracks hold no clips",
            });
        }
        if clip.id.is_valid() {
            if self.find_clip(clip.id).is_some() {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate clip id",
                });
            }
            self.reserve(clip.id.raw());
        } else {
            clip.id = self.allocate_clip_id();
        }
        let id = clip.id;
        let t = &mut self.tracks[ti];
        t.clips.push(clip);
        t.sort_clips();
        self.touch();
        Ok(id)
    }

    /// Removes a clip, returning the track it was on and the clip.
    pub fn remove_clip(&mut self, id: ClipId) -> Result<(TrackId, Clip)> {
        let (ti, ii) = self.find_clip(id).ok_or(SuperbError::NotFound {
            kind: "clip",
            id: id.raw(),
        })?;
        let track = self.tracks[ti].id;
        let clip = self.tracks[ti].clips.remove(ii);
        self.touch();
        Ok((track, clip))
    }

    /// Moves a clip to a new start position on its current track.
    pub fn move_clip(&mut self, id: ClipId, start: TimePosition) -> Result<()> {
        let (ti, ii) = self.find_clip(id).ok_or(SuperbError::NotFound {
            kind: "clip",
            id: id.raw(),
        })?;
        let t = &mut self.tracks[ti];
        if t.clips[ii].start != start {
            t.clips[ii].start = start;
            t.sort_clips();
            self.touch();
        }
        Ok(())
    }

    /// Moves a clip to another track, keeping its position.
    pub fn move_clip_to_track(&mut self, id: ClipId, track: TrackId) -> Result<()> {
        let dest = self.track_index(track)?;
        if self.tracks[dest].kind == TrackKind::Bus {
            return Err(SuperbError::InvalidArgument {
                what: "bus tracks hold no clips",
            });
        }
        let (ti, ii) = self.find_clip(id).ok_or(SuperbError::NotFound {
            kind: "clip",
            id: id.raw(),
        })?;
        if ti == dest {
            return Ok(());
        }
        let clip = self.tracks[ti].clips.remove(ii);
        let t = &mut self.tracks[dest];
        t.clips.push(clip);
        t.sort_clips();
        self.touch();
        Ok(())
    }

    /// Changes a clip's length (must be `> 0`).
    pub fn resize_clip(&mut self, id: ClipId, length: TimeDuration) -> Result<()> {
        self.clip_mut(id)?.set_length(length)
    }

    /// Looks up a clip on any track.
    pub fn clip(&self, id: ClipId) -> Result<&Clip> {
        let (ti, ii) = self.find_clip(id).ok_or(SuperbError::NotFound {
            kind: "clip",
            id: id.raw(),
        })?;
        Ok(&self.tracks[ti].clips[ii])
    }

    /// Mutable clip access (bumps the revision). Use
    /// [`move_clip`](Self::move_clip) to change the start so ordering holds.
    pub fn clip_mut(&mut self, id: ClipId) -> Result<&mut Clip> {
        let (ti, ii) = self.find_clip(id).ok_or(SuperbError::NotFound {
            kind: "clip",
            id: id.raw(),
        })?;
        self.touch();
        Ok(&mut self.tracks[ti].clips[ii])
    }

    /// The track a clip lives on.
    pub fn clip_track(&self, id: ClipId) -> Result<TrackId> {
        let (ti, _) = self.find_clip(id).ok_or(SuperbError::NotFound {
            kind: "clip",
            id: id.raw(),
        })?;
        Ok(self.tracks[ti].id)
    }

    fn find_clip(&self, id: ClipId) -> Option<(usize, usize)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.clips.iter().position(|i| i.id == id).map(|ii| (ti, ii)))
    }

    // ------------------------------------------------------------------
    // Markers
    // ------------------------------------------------------------------

    /// Adds a marker and returns its ID.
    pub fn add_marker(&mut self, name: impl Into<String>, position: TimePosition) -> MarkerId {
        let id = self.allocate_marker_id();
        self.insert_marker(Marker::with_id(id, name, position))
            .expect("fresh id")
    }

    /// Inserts a marker (fresh ID if it has none; no collisions otherwise).
    pub fn insert_marker(&mut self, mut marker: Marker) -> Result<MarkerId> {
        if marker.id.is_valid() {
            if self.markers.iter().any(|m| m.id == marker.id) {
                return Err(SuperbError::InvalidArgument {
                    what: "duplicate marker id",
                });
            }
            self.reserve(marker.id.raw());
        } else {
            marker.id = self.allocate_marker_id();
        }
        let id = marker.id;
        self.markers.push(marker);
        self.sort_markers();
        self.touch();
        Ok(id)
    }

    /// Removes a marker and returns it.
    pub fn remove_marker(&mut self, id: MarkerId) -> Result<Marker> {
        let i = self.marker_index(id)?;
        self.touch();
        Ok(self.markers.remove(i))
    }

    /// Moves a marker.
    pub fn move_marker(&mut self, id: MarkerId, position: TimePosition) -> Result<()> {
        let i = self.marker_index(id)?;
        if self.markers[i].position != position {
            self.markers[i].position = position;
            self.sort_markers();
            self.touch();
        }
        Ok(())
    }

    /// Looks up a marker.
    pub fn marker(&self, id: MarkerId) -> Result<&Marker> {
        let i = self.marker_index(id)?;
        Ok(&self.markers[i])
    }

    /// Mutable marker access (bumps the revision). Use
    /// [`move_marker`](Self::move_marker) to change the position.
    pub fn marker_mut(&mut self, id: MarkerId) -> Result<&mut Marker> {
        let i = self.marker_index(id)?;
        self.touch();
        Ok(&mut self.markers[i])
    }

    /// All markers in position order.
    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// The first marker strictly after `position`.
    pub fn next_marker(&self, position: TimePosition) -> Option<&Marker> {
        self.markers.iter().find(|m| m.position > position)
    }

    /// The last marker strictly before `position`.
    pub fn previous_marker(&self, position: TimePosition) -> Option<&Marker> {
        self.markers.iter().rev().find(|m| m.position < position)
    }

    fn marker_index(&self, id: MarkerId) -> Result<usize> {
        self.markers
            .iter()
            .position(|m| m.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "marker",
                id: id.raw(),
            })
    }

    fn sort_markers(&mut self) {
        self.markers
            .sort_by(|a, b| a.position.cmp(&b.position).then(a.id.cmp(&b.id)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::ClipSource;
    use crate::primitives::BeatPosition;

    const R: SampleRate = SampleRate::HZ_48K;

    fn clip(start: i64, len: i64) -> Clip {
        Clip::new(
            "i",
            TimePosition::from_samples(start),
            TimeDuration::from_samples(len),
            ClipSource::Empty,
        )
        .unwrap()
    }

    #[test]
    fn document_properties_and_revision() {
        let mut s = Session::new("session", R);
        assert_eq!(s.name(), "session");
        assert_eq!(s.sample_rate(), R);
        assert_eq!(s.revision(), 0);
        assert!(s.loop_range().is_none());
        s.set_name("tune");
        assert_eq!(s.revision(), 1);
        s.set_sample_rate(SampleRate::CD);
        assert_eq!(s.tempo_map().sample_rate(), SampleRate::CD);
        s.tempo_map_mut().set_initial_tempo(90.0).unwrap();
        assert_eq!(s.tempo_map().tempo_at(BeatPosition::ZERO), 90.0);
        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(10)).unwrap();
        s.set_loop_range(Some(range));
        assert_eq!(s.loop_range(), Some(range));
        assert_eq!(s.revision(), 4);
        assert_eq!(s.end_position(), TimePosition::ZERO);
        assert_eq!(s.clip_count(), 0);
    }

    #[test]
    fn tracks_are_ordered_with_unique_ids() {
        let mut s = Session::new("t", R);
        let a = s.add_track("a", TrackKind::Audio);
        let b = s.add_track("b", TrackKind::Midi);
        let c = s.add_track("c", TrackKind::Bus);
        assert!(a < b && b < c);
        assert_eq!(s.track_count(), 3);
        assert_eq!(s.track_index(b).unwrap(), 1);
        s.move_track(c, 0).unwrap();
        assert_eq!(
            s.tracks().iter().map(|t| t.id()).collect::<Vec<_>>(),
            [c, a, b]
        );
        assert!(s.move_track(c, 3).is_err());
        s.move_track(c, 0).unwrap(); // no-op
        let rev = s.revision();
        s.move_track(c, 0).unwrap();
        assert_eq!(s.revision(), rev, "no-op move does not bump");

        let removed = s.remove_track(a).unwrap();
        assert_eq!(removed.name, "a");
        assert!(s.track(a).is_err());
        assert!(matches!(
            s.remove_track(a),
            Err(SuperbError::NotFound { kind: "track", .. })
        ));
        // Re-inserting under the original id works once, then collides.
        s.insert_track(1, removed.clone()).unwrap();
        assert_eq!(s.track_index(a).unwrap(), 1);
        assert!(s.insert_track(0, removed).is_err());
        assert!(s
            .insert_track(9, Track::new("x", TrackKind::Audio))
            .is_err());
        // A track inserted with a high explicit id pushes the counter past it.
        s.insert_track(
            0,
            Track::with_id(TrackId::from_raw(100), "hi", TrackKind::Audio),
        )
        .unwrap();
        assert!(s.add_track("next", TrackKind::Audio).raw() > 100);
        s.track_mut(b).unwrap().name = "bee".into();
        assert_eq!(s.track(b).unwrap().name, "bee");
    }

    #[test]
    fn clips_stay_sorted_and_move_between_tracks() {
        let mut s = Session::new("t", R);
        let a = s.add_track("a", TrackKind::Audio);
        let b = s.add_track("b", TrackKind::Audio);
        let bus = s.add_track("bus", TrackKind::Bus);
        let late = s.add_clip(a, clip(1_000, 100)).unwrap();
        let early = s.add_clip(a, clip(0, 100)).unwrap();
        assert!(late < early);
        assert_eq!(
            s.track(a)
                .unwrap()
                .clips()
                .iter()
                .map(Clip::id)
                .collect::<Vec<_>>(),
            [early, late]
        );
        assert_eq!(s.clip_track(late).unwrap(), a);
        assert_eq!(s.clip_count(), 2);
        assert_eq!(s.end_position().samples(), 1_100);

        s.move_clip(early, TimePosition::from_samples(5_000))
            .unwrap();
        assert_eq!(s.track(a).unwrap().clips()[1].id(), early);
        assert_eq!(s.end_position().samples(), 5_100);

        s.move_clip_to_track(early, b).unwrap();
        assert_eq!(s.clip_track(early).unwrap(), b);
        assert_eq!(s.track(a).unwrap().clips().len(), 1);
        s.move_clip_to_track(early, b).unwrap(); // same track: no-op
        assert!(s.move_clip_to_track(early, bus).is_err());
        assert!(s.add_clip(bus, clip(0, 1)).is_err());

        s.resize_clip(late, TimeDuration::from_samples(10)).unwrap();
        assert_eq!(s.clip(late).unwrap().end().samples(), 1_010);
        assert!(s.resize_clip(late, TimeDuration::ZERO).is_err());
        s.clip_mut(late).unwrap().muted = true;
        assert!(s.clip(late).unwrap().muted);

        let (from, removed) = s.remove_clip(late).unwrap();
        assert_eq!(from, a);
        assert_eq!(removed.id(), late);
        assert!(s.clip(late).is_err());
        assert!(s.remove_clip(late).is_err());
        assert!(s.move_clip(late, TimePosition::ZERO).is_err());
        // Restore under the same id; duplicates are rejected.
        s.add_clip(a, removed.clone()).unwrap();
        assert!(s.add_clip(b, removed).is_err());
        // Removing a track takes its clips along; their ids stay unique.
        let t = s.remove_track(a).unwrap();
        assert_eq!(t.clips().len(), 1);
        assert!(s.clip(late).is_err());
        s.insert_track(0, t).unwrap();
        assert!(s.clip(late).is_ok());
    }

    #[test]
    fn insert_track_validates_clip_ids() {
        let mut s = Session::new("t", R);
        let a = s.add_track("a", TrackKind::Audio);
        let existing = s.add_clip(a, clip(0, 10)).unwrap();
        let mut t = Track::new("dup", TrackKind::Audio);
        t.clips.push(
            Clip::with_id(
                existing,
                "x",
                TimePosition::ZERO,
                TimeDuration::from_samples(1),
                ClipSource::Empty,
            )
            .unwrap(),
        );
        assert!(s.insert_track(0, t).is_err());
        let mut t = Track::new("dup2", TrackKind::Audio);
        let id = ClipId::from_raw(77);
        t.clips.push(
            Clip::with_id(
                id,
                "x",
                TimePosition::ZERO,
                TimeDuration::from_samples(1),
                ClipSource::Empty,
            )
            .unwrap(),
        );
        t.clips.push(
            Clip::with_id(
                id,
                "y",
                TimePosition::ZERO,
                TimeDuration::from_samples(1),
                ClipSource::Empty,
            )
            .unwrap(),
        );
        assert!(s.insert_track(0, t).is_err());
        let mut t = Track::new("ok", TrackKind::Audio);
        t.clips.push(clip(5, 1));
        t.clips.push(
            Clip::with_id(
                id,
                "x",
                TimePosition::ZERO,
                TimeDuration::from_samples(1),
                ClipSource::Empty,
            )
            .unwrap(),
        );
        let tid = s.insert_track(0, t).unwrap();
        let ids: Vec<_> = s.track(tid).unwrap().clips().iter().map(Clip::id).collect();
        assert_eq!(ids[0], id, "sorted by start");
        assert!(ids[1].is_valid() && ids[1] != id);
        assert!(s.next_raw_id() > 77);
    }

    #[test]
    fn routing_is_validated_and_detached_on_bus_removal() {
        let mut s = Session::new("t", R);
        let a = s.add_track("a", TrackKind::Audio);
        let k = s.add_track("k", TrackKind::Midi);
        let sub = s.add_track("sub", TrackKind::Bus);
        let fx = s.add_track("fx", TrackKind::Bus);
        assert_eq!(s.track(a).unwrap().output(), None);

        s.set_track_output(a, Some(sub)).unwrap();
        assert_eq!(s.track(a).unwrap().output(), Some(sub));
        let rev = s.revision();
        s.set_track_output(a, Some(sub)).unwrap();
        assert_eq!(s.revision(), rev, "no-op routing does not bump");
        assert!(
            matches!(
                s.set_track_output(a, Some(k)),
                Err(SuperbError::InvalidArgument { .. })
            ),
            "target must be a bus"
        );
        assert!(s.set_track_output(a, Some(TrackId::from_raw(999))).is_err());
        assert!(s.set_track_output(TrackId::from_raw(999), None).is_err());
        assert_eq!(s.set_track_output(sub, Some(sub)), Err(SuperbError::Cycle));
        s.set_track_output(sub, Some(fx)).unwrap();
        assert_eq!(s.set_track_output(fx, Some(sub)), Err(SuperbError::Cycle));
        assert_eq!(
            s.track(fx).unwrap().output(),
            None,
            "rejected route leaves the old one"
        );
        assert_eq!(s.tracks_routed_to(sub).collect::<Vec<_>>(), [a]);
        assert_eq!(s.tracks_routed_to(fx).collect::<Vec<_>>(), [sub]);

        // Sends: only from audio/MIDI tracks, only to buses, unique ids.
        let id = s.allocate_send_id();
        assert_eq!(
            s.add_send(a, TrackSend::with_id(id, fx).level_db(-6.0))
                .unwrap(),
            id
        );
        let auto = s.add_send(k, TrackSend::new(fx).pre_fader()).unwrap();
        assert!(auto.is_valid() && auto != id);
        assert!(
            s.add_send(a, TrackSend::with_id(id, fx)).is_err(),
            "duplicate send id"
        );
        assert!(
            s.add_send(sub, TrackSend::new(fx)).is_err(),
            "buses cannot send"
        );
        assert!(
            s.add_send(a, TrackSend::new(k)).is_err(),
            "sends target buses"
        );
        assert!(s
            .add_send(a, TrackSend::new(TrackId::from_raw(999)))
            .is_err());
        assert!(s
            .add_send(TrackId::from_raw(999), TrackSend::new(fx))
            .is_err());
        assert_eq!(s.send_count(), 2);
        assert_eq!(s.send(id).unwrap().level_db, -6.0);
        assert_eq!(s.send_track(auto).unwrap(), k);
        assert!(s.send(auto).unwrap().pre_fader);
        s.send_mut(auto).unwrap().enabled = false;
        assert!(!s.send(auto).unwrap().enabled);
        assert_eq!(
            s.sends_to(fx).map(|(t, s)| (t, s.id())).collect::<Vec<_>>(),
            [(a, id), (k, auto)]
        );
        assert!(s.sends_to(sub).next().is_none());

        let (from, removed) = s.remove_send(auto).unwrap();
        assert_eq!(from, k);
        assert_eq!(removed.id(), auto);
        assert!(s.send(auto).is_err());
        assert!(s.remove_send(auto).is_err());
        assert!(s.send_mut(auto).is_err());
        assert!(s.send_track(auto).is_err());
        s.add_send(k, removed.clone()).unwrap();
        assert!(s.add_send(k, removed).is_err());

        // Removing a bus detaches outputs and sends into it.
        let gone = s.remove_track(fx).unwrap();
        assert_eq!(gone.kind, TrackKind::Bus);
        assert_eq!(s.track(sub).unwrap().output(), None);
        assert_eq!(s.send_count(), 0);
        assert_eq!(
            s.track(a).unwrap().output(),
            Some(sub),
            "unrelated routes survive"
        );

        // Removing a routed track takes its sends and output along; it can
        // come back as long as its targets still exist.
        s.add_send(a, TrackSend::new(sub)).unwrap();
        let track_a = s.remove_track(a).unwrap();
        assert_eq!(track_a.output(), Some(sub));
        assert_eq!(track_a.sends().len(), 1);
        s.insert_track(0, track_a.clone()).unwrap();
        assert_eq!(s.track(a).unwrap().sends().len(), 1);
        assert!(
            s.insert_track(0, track_a.clone()).is_err(),
            "duplicate track id"
        );
        let mut dup = track_a.clone();
        dup.id = TrackId::INVALID;
        assert!(s.insert_track(0, dup).is_err(), "duplicate send id");
        s.remove_track(sub).unwrap();
        assert!(
            s.insert_track(0, track_a.clone()).is_err(),
            "dangling output"
        );
        let mut orphan = track_a;
        orphan.output = None;
        assert!(s.insert_track(0, orphan.clone()).is_err(), "dangling send");
        orphan.sends.clear();
        assert!(
            s.insert_track(0, orphan).is_err(),
            "still a duplicate id (a is present)"
        );
        // Bus tracks may not carry sends.
        let mut bad_bus = Track::new("b", TrackKind::Bus);
        bad_bus.sends.push(TrackSend::new(a));
        assert!(s.insert_track(0, bad_bus).is_err());
        // A rejected insert does not consume ids.
        let next = s.next_raw_id();
        let mut t = Track::new("t", TrackKind::Audio);
        t.output = Some(TrackId::from_raw(999));
        assert!(s.insert_track(0, t).is_err());
        assert_eq!(s.next_raw_id(), next);
    }

    #[test]
    fn inserts_form_an_ordered_chain_per_track() {
        use crate::primitives::plugins::PluginFormat;
        use crate::primitives::session::PluginRef;
        let plugin = |id: &str| PluginRef {
            format: PluginFormat::Clap,
            path: "/plugins/fx.clap".into(),
            id: id.into(),
        };
        let mut s = Session::new("t", R);
        let a = s.add_track("a", TrackKind::Audio);
        let bus = s.add_track("bus", TrackKind::Bus);
        let eq = s.add_insert(a, Insert::new(plugin("eq"))).unwrap();
        let comp = s.add_insert(a, Insert::new(plugin("comp"))).unwrap();
        let pre = s.allocate_insert_id();
        assert_eq!(
            s.add_insert_at(a, 0, Insert::with_id(pre, plugin("gate")))
                .unwrap(),
            pre
        );
        let chain: Vec<_> = s
            .track(a)
            .unwrap()
            .inserts()
            .iter()
            .map(Insert::id)
            .collect();
        assert_eq!(chain, [pre, eq, comp]);
        assert_eq!(s.insert_count(), 3);
        assert_eq!(s.insert_owner(comp).unwrap(), a);
        assert_eq!(s.insert(eq).unwrap().plugin.id, "eq");
        assert!(s.track(a).unwrap().insert(eq).is_some());
        // Buses take inserts too (a reverb bus).
        let verb = s.add_insert(bus, Insert::new(plugin("verb"))).unwrap();
        assert_eq!(s.insert_owner(verb).unwrap(), bus);

        // Reordering and no-op moves.
        s.move_insert(comp, 0).unwrap();
        let chain: Vec<_> = s
            .track(a)
            .unwrap()
            .inserts()
            .iter()
            .map(Insert::id)
            .collect();
        assert_eq!(chain, [comp, pre, eq]);
        let rev = s.revision();
        s.move_insert(comp, 0).unwrap();
        assert_eq!(s.revision(), rev, "no-op move does not bump");
        assert!(s.move_insert(comp, 3).is_err());
        assert!(s.add_insert_at(a, 9, Insert::new(plugin("x"))).is_err());

        // Editing through insert_mut bumps the revision.
        s.insert_mut(eq).unwrap().set_param(1, 0.5);
        assert_eq!(s.insert(eq).unwrap().param(1), Some(0.5));
        assert!(s.revision() > rev);

        // Removal returns the position; re-adding under the same id works once.
        let (track, at, removed) = s.remove_insert(pre).unwrap();
        assert_eq!((track, at), (a, 1));
        assert_eq!(removed.id(), pre);
        assert!(s.insert(pre).is_err());
        assert!(s.remove_insert(pre).is_err());
        assert!(s.insert_mut(pre).is_err());
        assert!(s.insert_owner(pre).is_err());
        s.add_insert_at(a, 1, removed.clone()).unwrap();
        assert!(s.add_insert(a, removed).is_err(), "duplicate insert id");
        assert!(s
            .add_insert(TrackId::from_raw(99), Insert::new(plugin("x")))
            .is_err());

        // Removing a track takes its inserts along and they come back with it.
        let t = s.remove_track(a).unwrap();
        assert_eq!(t.inserts().len(), 3);
        assert_eq!(s.insert_count(), 1);
        s.insert_track(0, t.clone()).unwrap();
        assert_eq!(s.insert_count(), 4);
        assert!(s.insert_track(0, t).is_err(), "duplicate ids");
        let mut dup = Track::new("dup", TrackKind::Audio);
        dup.inserts.push(Insert::with_id(eq, plugin("eq")));
        assert!(s.insert_track(0, dup).is_err(), "insert id already present");
        let mut fresh = Track::new("fresh", TrackKind::Audio);
        fresh.inserts.push(Insert::new(plugin("a")));
        fresh.inserts.push(Insert::new(plugin("b")));
        let id = s.insert_track(0, fresh).unwrap();
        let ids: Vec<_> = s
            .track(id)
            .unwrap()
            .inserts()
            .iter()
            .map(Insert::id)
            .collect();
        assert!(ids[0].is_valid() && ids[1].is_valid() && ids[0] != ids[1]);
    }

    #[test]
    fn markers_are_sorted_and_navigable() {
        let mut s = Session::new("t", R);
        let verse = s.add_marker("verse", TimePosition::from_samples(1_000));
        let intro = s.add_marker("intro", TimePosition::ZERO);
        let chorus = s.add_marker("chorus", TimePosition::from_samples(2_000));
        assert_eq!(
            s.markers().iter().map(Marker::id).collect::<Vec<_>>(),
            [intro, verse, chorus]
        );
        assert_eq!(
            s.next_marker(TimePosition::from_samples(1_000))
                .unwrap()
                .id(),
            chorus
        );
        assert_eq!(
            s.previous_marker(TimePosition::from_samples(1_000))
                .unwrap()
                .id(),
            intro
        );
        assert!(s.next_marker(TimePosition::from_samples(2_000)).is_none());
        assert!(s.previous_marker(TimePosition::ZERO).is_none());
        s.move_marker(intro, TimePosition::from_samples(3_000))
            .unwrap();
        assert_eq!(s.markers().last().unwrap().id(), intro);
        s.marker_mut(intro).unwrap().name = "outro".into();
        assert_eq!(s.marker(intro).unwrap().name, "outro");
        assert_eq!(s.marker(intro).unwrap().position().samples(), 3_000);
        let m = s.remove_marker(intro).unwrap();
        assert!(s.marker(intro).is_err());
        assert!(s.move_marker(intro, TimePosition::ZERO).is_err());
        s.insert_marker(m.clone()).unwrap();
        assert!(s.insert_marker(m).is_err());
        let fresh = s
            .insert_marker(Marker::new("free", TimePosition::ZERO))
            .unwrap();
        assert!(fresh.is_valid());
    }
}
