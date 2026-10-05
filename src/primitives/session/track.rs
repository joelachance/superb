//! Tracks and the clips they hold.

use std::path::{Path, PathBuf};

use super::insert::Insert;
use crate::primitives::{
    ClipId, InsertId, Result, SendId, SuperbError, TimeDuration, TimePosition, TrackId,
};
use serde::{Deserialize, Serialize};

/// What a track carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackKind {
    /// Audio clips.
    Audio,
    /// MIDI clips feeding an instrument.
    Midi,
    /// A submix bus; holds no clips.
    Bus,
}

/// One note inside a MIDI clip. Times are in beats relative to the clip start.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiNote {
    /// Onset in beats from the clip start.
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

/// Where a clip's content comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClipSource {
    /// A placeholder with no content (renders silence).
    Empty,
    /// A region of an audio file starting `offset` into the file.
    Audio {
        /// Path to the file (relative paths are resolved by the application).
        path: PathBuf,
        /// Where in the file the clip starts.
        #[serde(with = "crate::primitives::session::snapshot::duration_samples")]
        offset: TimeDuration,
    },
    /// Inline MIDI notes.
    Midi {
        /// The notes, in any order.
        notes: Vec<MidiNote>,
    },
}

impl From<PathBuf> for ClipSource {
    /// Audio named by `path`, played from its start.
    fn from(path: PathBuf) -> Self {
        ClipSource::Audio {
            path,
            offset: TimeDuration::ZERO,
        }
    }
}

impl From<&Path> for ClipSource {
    fn from(path: &Path) -> Self {
        path.to_path_buf().into()
    }
}

impl From<&str> for ClipSource {
    fn from(path: &str) -> Self {
        PathBuf::from(path).into()
    }
}

impl From<String> for ClipSource {
    fn from(path: String) -> Self {
        PathBuf::from(path).into()
    }
}

impl From<Vec<MidiNote>> for ClipSource {
    fn from(notes: Vec<MidiNote>) -> Self {
        ClipSource::Midi { notes }
    }
}

/// A clip on a track: a placed region of some [`ClipSource`].
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    pub(crate) id: ClipId,
    /// Display name.
    pub name: String,
    pub(crate) start: TimePosition,
    pub(crate) length: TimeDuration,
    /// Content.
    pub source: ClipSource,
    /// Clip gain in dB (0 = unity).
    pub gain_db: f32,
    /// Muted clips are not rendered.
    pub muted: bool,
    /// Fade-in length (`0` for none).
    pub fade_in: TimeDuration,
    /// Fade-out length (`0` for none).
    pub fade_out: TimeDuration,
    /// Archived clips stay in the session with their place and audio but
    /// are not rendered and do not count toward a track's end.
    pub archived: bool,
}

impl Clip {
    /// Creates a clip with no ID yet (the memory assigns one on
    /// [`Memory::add_clip`](crate::primitives::session::Memory::add_clip)). Fails unless
    /// `length > 0`.
    pub fn new(
        name: impl Into<String>,
        start: TimePosition,
        length: TimeDuration,
        source: ClipSource,
    ) -> Result<Self> {
        Self::with_id(ClipId::INVALID, name, start, length, source)
    }

    /// Creates a clip with a pre-allocated ID (see
    /// [`Memory::allocate_clip_id`](crate::primitives::session::Memory::allocate_clip_id)).
    pub fn with_id(
        id: ClipId,
        name: impl Into<String>,
        start: TimePosition,
        length: TimeDuration,
        source: ClipSource,
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
            archived: false,
        })
    }

    /// The clip's ID (`ClipId::INVALID` until added to a memory).
    pub fn id(&self) -> ClipId {
        self.id
    }

    /// First frame of the clip.
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

    /// Moves the clip (use [`Memory::move_clip`](crate::primitives::session::Memory::move_clip)
    /// once the clip lives in a memory, so ordering is maintained).
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

    /// `true` if the two clips share at least one frame.
    pub fn overlaps(&self, other: &Clip) -> bool {
        self.start < other.end() && other.start < self.end()
    }
}

fn check_length(length: TimeDuration) -> Result<()> {
    if length.samples() <= 0 {
        return Err(SuperbError::InvalidArgument {
            what: "clip length (must be > 0)",
        });
    }
    Ok(())
}

/// A track's state: everything its strip shows and a switch or fader can
/// change. Saved with the memory. Mute, solo, arm and monitor are
/// switches; volume and pan are continuous.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackState {
    /// The fader, in dB (0 = unity).
    pub volume_db: f32,
    /// Pan in `-1.0..=1.0`.
    pub pan: f32,
    /// Muted.
    pub mute: bool,
    /// Soloed: while any track is soloed, every unsoloed track is silent.
    pub solo: bool,
    /// Armed: an audio track that receives a clip when a recording stops.
    pub arm: bool,
    /// Monitoring: a MIDI track that carries a live instrument for
    /// incoming MIDI.
    pub monitor: bool,
    /// Which channel(s) of the audio input feed this track when it records.
    /// `None` takes everything the recording captures.
    pub input: Option<Input>,
}

/// Which channels of the audio input a track records from, by zero-based
/// channel index on the device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Input {
    /// One channel, recorded as mono.
    Mono(usize),
    /// Two channels (left, right), recorded as stereo.
    Stereo(usize, usize),
}

impl Input {
    /// How many input channels a recording must capture for this input to
    /// be present: one past the highest index it names.
    pub fn width(self) -> usize {
        match self {
            Input::Mono(c) => c + 1,
            Input::Stereo(l, r) => l.max(r) + 1,
        }
    }
}

impl Default for TrackState {
    fn default() -> Self {
        Self {
            volume_db: 0.0,
            pan: 0.0,
            mute: false,
            solo: false,
            arm: false,
            monitor: false,
            input: None,
        }
    }
}

/// A send's state: its level and whether it is on.
#[derive(Clone, Debug, PartialEq)]
pub struct SendState {
    /// Send level in dB (0 = unity).
    pub level_db: f32,
    /// A disabled send passes no signal.
    pub enabled: bool,
}

impl Default for SendState {
    fn default() -> Self {
        Self {
            level_db: 0.0,
            enabled: true,
        }
    }
}

/// A send from an audio/MIDI track to a [`TrackKind::Bus`] track.
///
/// The destination is fixed at creation (remove and re-add to retarget);
/// tap point and state are plain fields, edited through
/// [`Memory::send_mut`](crate::primitives::session::Memory::send_mut) or the
/// [`commands`](crate::primitives::session::commands) helpers.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackSend {
    pub(crate) id: SendId,
    pub(crate) bus: TrackId,
    /// Tap before the fader/pan/mute (`true`) or after the strip (`false`).
    pub pre_fader: bool,
    /// Level and on/off.
    pub state: SendState,
}

impl TrackSend {
    /// A unity, post-fader, enabled send to `bus` with no ID yet.
    pub fn new(bus: TrackId) -> Self {
        Self::with_id(SendId::INVALID, bus)
    }

    /// A unity, post-fader, enabled send with a pre-allocated ID (see
    /// [`Memory::allocate_send_id`](crate::primitives::session::Memory::allocate_send_id)).
    pub fn with_id(id: SendId, bus: TrackId) -> Self {
        Self {
            id,
            bus,
            pre_fader: false,
            state: SendState::default(),
        }
    }

    /// Sets the level in dB (builder style).
    pub fn level_db(mut self, level_db: f32) -> Self {
        self.state.level_db = level_db;
        self
    }

    /// Taps before the fader (builder style).
    pub fn pre_fader(mut self) -> Self {
        self.pre_fader = true;
        self
    }

    /// The send's ID (`SendId::INVALID` until added to a memory).
    pub fn id(&self) -> SendId {
        self.id
    }

    /// Destination bus track.
    pub fn bus(&self) -> TrackId {
        self.bus
    }
}

/// A track: an ordered lane of clips plus mixer-facing properties.
///
/// Clips are kept sorted by start position (ties by ID). Properties are
/// plain fields; change them through
/// [`Memory::track_mut`](crate::primitives::session::Memory::track_mut) so the memory's
/// revision counter moves. Routing (`output`, `sends`) is validated, so it
/// goes through [`Memory::set_track_output`](crate::primitives::session::Memory::set_track_output)
/// and [`Memory::add_send`](crate::primitives::session::Memory::add_send).
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub(crate) id: TrackId,
    /// Display name.
    pub name: String,
    /// Audio, MIDI or bus.
    pub kind: TrackKind,
    /// Display colour as `0xRRGGBB`.
    pub color: Option<u32>,
    /// What the strip shows: volume, pan, mute, solo, arm, monitor.
    pub state: TrackState,
    pub(crate) output: Option<TrackId>,
    pub(crate) sends: Vec<TrackSend>,
    pub(crate) inserts: Vec<Insert>,
    pub(crate) clips: Vec<Clip>,
}

impl Track {
    /// Creates a track with no ID yet.
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self::with_id(TrackId::INVALID, name, kind)
    }

    /// Creates a track with a pre-allocated ID (see
    /// [`Memory::allocate_track_id`](crate::primitives::session::Memory::allocate_track_id)).
    pub fn with_id(id: TrackId, name: impl Into<String>, kind: TrackKind) -> Self {
        Self {
            id,
            name: name.into(),
            kind,
            color: None,
            state: TrackState::default(),
            output: None,
            sends: Vec::new(),
            inserts: Vec::new(),
            clips: Vec::new(),
        }
    }

    /// The track's ID (`TrackId::INVALID` until added to a memory).
    pub fn id(&self) -> TrackId {
        self.id
    }

    /// The bus track this track's output feeds, or `None` for the master.
    pub fn output(&self) -> Option<TrackId> {
        self.output
    }

    /// The insert chain, in signal order (first effect first).
    pub fn inserts(&self) -> &[Insert] {
        &self.inserts
    }

    /// Looks up an insert on this track.
    pub fn insert(&self, id: InsertId) -> Option<&Insert> {
        self.inserts.iter().find(|i| i.id == id)
    }

    /// Sends in creation order (always empty on bus tracks).
    pub fn sends(&self) -> &[TrackSend] {
        &self.sends
    }

    /// Looks up a send on this track.
    pub fn send(&self, id: SendId) -> Option<&TrackSend> {
        self.sends.iter().find(|s| s.id == id)
    }

    /// Clips in start order.
    pub fn clips(&self) -> &[Clip] {
        &self.clips
    }

    /// Looks up a clip on this track.
    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.iter().find(|i| i.id == id)
    }

    /// Clips covering `position`.
    pub fn clips_at(&self, position: TimePosition) -> impl Iterator<Item = &Clip> + '_ {
        self.clips.iter().filter(move |i| i.contains(position))
    }

    /// End of the last clip that is not archived, or zero for an empty track.
    pub fn end(&self) -> TimePosition {
        self.clips
            .iter()
            .filter(|c| !c.archived)
            .map(Clip::end)
            .max()
            .unwrap_or(TimePosition::ZERO)
    }

    pub(crate) fn sort_clips(&mut self) {
        self.clips
            .sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn clip_geometry() {
        let a = clip(100, 50);
        assert_eq!(a.end().samples(), 150);
        assert!(a.contains(TimePosition::from_samples(100)));
        assert!(a.contains(TimePosition::from_samples(149)));
        assert!(!a.contains(TimePosition::from_samples(150)));
        assert!(!a.contains(TimePosition::from_samples(99)));
        assert!(a.overlaps(&clip(149, 10)));
        assert!(!a.overlaps(&clip(150, 10)));
        assert!(a.overlaps(&clip(0, 101)));
        assert!(!a.overlaps(&clip(0, 100)));
        assert_eq!(a.id(), ClipId::INVALID);
        assert_eq!(a.length().samples(), 50);
    }

    #[test]
    fn clip_length_is_validated() {
        assert!(Clip::new(
            "x",
            TimePosition::ZERO,
            TimeDuration::ZERO,
            ClipSource::Empty
        )
        .is_err());
        assert!(Clip::new(
            "x",
            TimePosition::ZERO,
            TimeDuration::from_samples(-5),
            ClipSource::Empty
        )
        .is_err());
        let mut a = clip(0, 10);
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
        assert_eq!(
            (s.state.level_db, s.pre_fader, s.state.enabled),
            (0.0, false, true)
        );
        let s = TrackSend::with_id(SendId::from_raw(3), bus)
            .level_db(-6.0)
            .pre_fader();
        assert_eq!(s.id(), SendId::from_raw(3));
        assert_eq!(
            (s.state.level_db, s.pre_fader, s.state.enabled),
            (-6.0, true, true)
        );
    }

    #[test]
    fn track_defaults_and_queries() {
        let mut t = Track::new("lead", TrackKind::Midi);
        assert_eq!(t.id(), TrackId::INVALID);
        assert_eq!(t.kind, TrackKind::Midi);
        assert_eq!(t.state.volume_db, 0.0);
        assert_eq!(t.output(), None);
        assert!(t.sends().is_empty());
        assert!(t.send(SendId::from_raw(1)).is_none());
        assert_eq!(t.end(), TimePosition::ZERO);
        let mut b = clip(50, 10);
        b.id = ClipId::from_raw(2);
        let mut a = clip(0, 100);
        a.id = ClipId::from_raw(1);
        t.clips.push(b);
        t.clips.push(a);
        t.sort_clips();
        assert_eq!(t.clips()[0].id(), ClipId::from_raw(1));
        assert_eq!(t.clips_at(TimePosition::from_samples(55)).count(), 2);
        assert_eq!(t.clips_at(TimePosition::from_samples(5)).count(), 1);
        assert_eq!(t.end().samples(), 100);
        assert!(t.clip(ClipId::from_raw(2)).is_some());
        assert!(t.clip(ClipId::from_raw(3)).is_none());
    }
}
