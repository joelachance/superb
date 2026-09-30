//! Tracks and the items (clips) they hold.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use superb_core::{ClipId, Result, SendId, SuperbError, TimeDuration, TimePosition, TrackId};

/// What a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    /// Audio items.
    Audio,
    /// MIDI items feeding an instrument.
    Midi,
    /// A submix bus; holds no items.
    Bus,
}

/// One note inside a MIDI item. Times are in beats relative to the item start.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiNote {
    /// Onset in beats from the item start.
    pub beat: f64,
    /// Duration in beats.
    pub length: f64,
    /// MIDI note number `0..=127`.
    pub note: u8,
    /// Velocity `1..=127`.
    pub velocity: u8,
    /// MIDI channel `0..=15`.
    pub channel: u8,
}

/// Where an item's content comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ItemSource {
    /// A placeholder with no content (renders silence).
    Empty,
    /// A region of an audio file starting `offset` into the file.
    Audio {
        /// Path to the file (relative paths are resolved by the application).
        path: PathBuf,
        /// Where in the file the item starts.
        #[serde(with = "crate::snapshot::duration_samples")]
        offset: TimeDuration,
    },
    /// Inline MIDI notes.
    Midi {
        /// The notes, in any order.
        notes: Vec<MidiNote>,
    },
}

/// A clip on a track: a placed region of some [`ItemSource`].
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub(crate) id: ClipId,
    /// Display name.
    pub name: String,
    pub(crate) start: TimePosition,
    pub(crate) length: TimeDuration,
    /// Content.
    pub source: ItemSource,
    /// Item gain in dB (0 = unity).
    pub gain_db: f32,
    /// Muted items are not rendered.
    pub muted: bool,
    /// Fade-in length (`0` for none).
    pub fade_in: TimeDuration,
    /// Fade-out length (`0` for none).
    pub fade_out: TimeDuration,
}

impl Item {
    /// Creates an item with no ID yet (the session assigns one on
    /// [`Session::add_item`](crate::Session::add_item)). Fails unless
    /// `length > 0`.
    pub fn new(name: impl Into<String>, start: TimePosition, length: TimeDuration, source: ItemSource) -> Result<Self> {
        Self::with_id(ClipId::INVALID, name, start, length, source)
    }

    /// Creates an item with a pre-allocated ID (see
    /// [`Session::allocate_clip_id`](crate::Session::allocate_clip_id)).
    pub fn with_id(
        id: ClipId,
        name: impl Into<String>,
        start: TimePosition,
        length: TimeDuration,
        source: ItemSource,
    ) -> Result<Self> {
        check_length(length)?;
        Ok(Self {
            id,
            name: name.into(),
            start,
            length,
            source,
            gain_db: 0.0,
            muted: false,
            fade_in: TimeDuration::ZERO,
            fade_out: TimeDuration::ZERO,
        })
    }

    /// The item's ID (`ClipId::INVALID` until added to a session).
    pub fn id(&self) -> ClipId {
        self.id
    }

    /// First frame of the item.
    pub fn start(&self) -> TimePosition {
        self.start
    }

    /// Length (always `> 0`).
    pub fn length(&self) -> TimeDuration {
        self.length
    }

    /// One past the last frame.
    pub fn end(&self) -> TimePosition {
        self.start.offset(self.length.samples())
    }

    /// Moves the item (use [`Session::move_item`](crate::Session::move_item)
    /// once the item lives in a session, so ordering is maintained).
    pub fn set_start(&mut self, start: TimePosition) {
        self.start = start;
    }

    /// Changes the length. Fails unless `length > 0`.
    pub fn set_length(&mut self, length: TimeDuration) -> Result<()> {
        check_length(length)?;
        self.length = length;
        Ok(())
    }

    /// `true` if `position` lies in `[start, end)`.
    pub fn contains(&self, position: TimePosition) -> bool {
        position >= self.start && position < self.end()
    }

    /// `true` if the two items share at least one frame.
    pub fn overlaps(&self, other: &Item) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

fn check_length(length: TimeDuration) -> Result<()> {
    if length.samples() <= 0 {
        return Err(SuperbError::InvalidArgument { what: "item length (must be > 0)" });
    }
    Ok(())
}

/// A send from an audio/MIDI track to a [`TrackKind::Bus`] track.
///
/// The destination is fixed at creation (remove and re-add to retarget);
/// level, tap point and enable state are plain fields, edited through
/// [`Session::send_mut`](crate::Session::send_mut) or the
/// [`commands`](crate::commands) helpers.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSend {
    pub(crate) id: SendId,
    pub(crate) bus: TrackId,
    /// Send level in dB (0 = unity).
    pub level_db: f32,
    /// Tap before the fader/pan/mute (`true`) or after the strip (`false`).
    pub pre_fader: bool,
    /// Disabled sends pass no signal.
    pub enabled: bool,
}

impl TrackSend {
    /// A unity, post-fader, enabled send to `bus` with no ID yet.
    pub fn new(bus: TrackId) -> Self {
        Self::with_id(SendId::INVALID, bus)
    }

    /// A unity, post-fader, enabled send with a pre-allocated ID (see
    /// [`Session::allocate_send_id`](crate::Session::allocate_send_id)).
    pub fn with_id(id: SendId, bus: TrackId) -> Self {
        Self { id, bus, level_db: 0.0, pre_fader: false, enabled: true }
    }

    /// Sets the level in dB (builder style).
    pub fn level_db(mut self, level_db: f32) -> Self {
        self.level_db = level_db;
        self
    }

    /// Taps before the fader (builder style).
    pub fn pre_fader(mut self) -> Self {
        self.pre_fader = true;
        self
    }

    /// The send's ID (`SendId::INVALID` until added to a session).
    pub fn id(&self) -> SendId {
        self.id
    }

    /// Destination bus track.
    pub fn bus(&self) -> TrackId {
        self.bus
    }
}

/// A track: an ordered lane of items plus mixer-facing properties.
///
/// Items are kept sorted by start position (ties by ID). Properties are
/// plain fields; change them through
/// [`Session::track_mut`](crate::Session::track_mut) so the session's
/// revision counter moves. Routing (`output`, `sends`) is validated, so it
/// goes through [`Session::set_track_output`](crate::Session::set_track_output)
/// and [`Session::add_send`](crate::Session::add_send).
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub(crate) id: TrackId,
    /// Display name.
    pub name: String,
    /// Audio, MIDI or bus.
    pub kind: TrackKind,
    /// Display colour as `0xRRGGBB`.
    pub color: Option<u32>,
    /// Fader gain in dB.
    pub gain_db: f32,
    /// Pan in `-1.0..=1.0`.
    pub pan: f32,
    /// Mute state.
    pub muted: bool,
    /// Solo state.
    pub soloed: bool,
    /// Record-arm state.
    pub armed: bool,
    pub(crate) output: Option<TrackId>,
    pub(crate) sends: Vec<TrackSend>,
    pub(crate) items: Vec<Item>,
}

impl Track {
    /// Creates a track with no ID yet.
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self::with_id(TrackId::INVALID, name, kind)
    }

    /// Creates a track with a pre-allocated ID (see
    /// [`Session::allocate_track_id`](crate::Session::allocate_track_id)).
    pub fn with_id(id: TrackId, name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            color: None,
            gain_db: 0.0,
            pan: 0.0,
            muted: false,
            soloed: false,
            armed: false,
            output: None,
            sends: Vec::new(),
            items: Vec::new(),
        }
    }

    /// The track's ID (`TrackId::INVALID` until added to a session).
    pub fn id(&self) -> TrackId {
        self.id
    }

    /// The bus track this track's output feeds, or `None` for the master.
    pub fn output(&self) -> Option<TrackId> {
        self.output
    }

    /// Sends in creation order (always empty on bus tracks).
    pub fn sends(&self) -> &[TrackSend] {
        &self.sends
    }

    /// Looks up a send on this track.
    pub fn send(&self, id: SendId) -> Option<&TrackSend> {
        self.sends.iter().find(|s| s.id == id)
    }

    /// Items in start order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Looks up an item on this track.
    pub fn item(&self, id: ClipId) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// Items covering `position`.
    pub fn items_at(&self, position: TimePosition) -> impl Iterator<Item = &Item> + '_ {
        self.items.iter().filter(move |i| i.contains(position))
    }

    /// End of the last item, or zero for an empty track.
    pub fn end(&self) -> TimePosition {
        self.items.iter().map(Item::end).max().unwrap_or(TimePosition::ZERO)
    }

    pub(crate) fn sort_items(&mut self) {
        self.items.sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(start: i64, len: i64) -> Item {
        Item::new("i", TimePosition::from_samples(start), TimeDuration::from_samples(len), ItemSource::Empty).unwrap()
    }

    #[test]
    fn item_geometry() {
        let a = item(100, 50);
        assert_eq!(a.end().samples(), 150);
        assert!(a.contains(TimePosition::from_samples(100)));
        assert!(a.contains(TimePosition::from_samples(149)));
        assert!(!a.contains(TimePosition::from_samples(150)));
        assert!(!a.contains(TimePosition::from_samples(99)));
        assert!(a.overlaps(&item(149, 10)));
        assert!(!a.overlaps(&item(150, 10)));
        assert!(a.overlaps(&item(0, 101)));
        assert!(!a.overlaps(&item(0, 100)));
        assert_eq!(a.id(), ClipId::INVALID);
        assert_eq!(a.length().samples(), 50);
    }

    #[test]
    fn item_length_is_validated() {
        assert!(Item::new("x", TimePosition::ZERO, TimeDuration::ZERO, ItemSource::Empty).is_err());
        assert!(Item::new("x", TimePosition::ZERO, TimeDuration::from_samples(-5), ItemSource::Empty).is_err());
        let mut a = item(0, 10);
        assert!(a.set_length(TimeDuration::ZERO).is_err());
        a.set_length(TimeDuration::from_samples(20)).unwrap();
        a.set_start(TimePosition::from_samples(5));
        assert_eq!(a.end().samples(), 25);
    }

    #[test]
    fn send_builders() {
        let bus = TrackId::from_raw(7);
        let s = TrackSend::new(bus);
        assert_eq!(s.id(), SendId::INVALID);
        assert_eq!(s.bus(), bus);
        assert_eq!((s.level_db, s.pre_fader, s.enabled), (0.0, false, true));
        let s = TrackSend::with_id(SendId::from_raw(3), bus).level_db(-6.0).pre_fader();
        assert_eq!(s.id(), SendId::from_raw(3));
        assert_eq!((s.level_db, s.pre_fader, s.enabled), (-6.0, true, true));
    }

    #[test]
    fn track_defaults_and_queries() {
        let mut t = Track::new("lead", TrackKind::Midi);
        assert_eq!(t.id(), TrackId::INVALID);
        assert_eq!(t.kind, TrackKind::Midi);
        assert_eq!(t.gain_db, 0.0);
        assert_eq!(t.output(), None);
        assert!(t.sends().is_empty());
        assert!(t.send(SendId::from_raw(1)).is_none());
        assert_eq!(t.end(), TimePosition::ZERO);
        let mut b = item(50, 10);
        b.id = ClipId::from_raw(2);
        let mut a = item(0, 100);
        a.id = ClipId::from_raw(1);
        t.items.push(b);
        t.items.push(a);
        t.sort_items();
        assert_eq!(t.items()[0].id(), ClipId::from_raw(1));
        assert_eq!(t.items_at(TimePosition::from_samples(55)).count(), 2);
        assert_eq!(t.items_at(TimePosition::from_samples(5)).count(), 1);
        assert_eq!(t.end().samples(), 100);
        assert!(t.item(ClipId::from_raw(2)).is_some());
        assert!(t.item(ClipId::from_raw(3)).is_none());
    }
}
