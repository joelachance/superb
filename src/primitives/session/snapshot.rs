//! Versioned, serialisable copies of a [`Session`].
//!
//! The snapshot types use only primitives (raw IDs, sample counts, Hz) so
//! the file format does not change when in-memory types evolve.
//! [`FORMAT_VERSION`] is written into every file; loading refuses newer
//! versions.

use std::path::Path;

use crate::primitives::transport::{LoopRange, TempoMap};
use crate::primitives::{
    BeatPosition, ClipId, InsertId, MarkerId, RawId, Result, SampleRate, SendId, SuperbError,
    TimeDuration, TimePosition, TimeSignature, TrackId,
};
use serde::{Deserialize, Serialize};

use crate::primitives::plugins::PluginFormat;
use crate::primitives::session::{
    Clip, ClipSource, Insert, Marker, PluginRef, Session, Track, TrackKind, TrackSend,
};

/// Version of the snapshot format written by this crate.
///
/// Version 1 files without the routing fields (`output`, `sends`, added
/// later within version 1) load with master routing and no sends.
pub const FORMAT_VERSION: u32 = 1;

/// Serde helper: `TimeDuration` as a plain sample count.
pub(crate) mod duration_samples {
    use crate::primitives::TimeDuration;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        d: &TimeDuration,
        s: S,
    ) -> core::result::Result<S::Ok, S::Error> {
        s.serialize_i64(d.samples())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> core::result::Result<TimeDuration, D::Error> {
        i64::deserialize(d).map(TimeDuration::from_samples)
    }
}

/// A tempo change.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoPointSnapshot {
    /// Beat where the tempo takes effect.
    pub beat: f64,
    /// Quarter notes per minute.
    pub bpm: f64,
    /// Ramp linearly to the next point (absent in files written before ramps existed).
    #[serde(default)]
    pub ramp: bool,
}

/// A meter change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeterSnapshot {
    /// 1-based bar where the meter starts.
    pub bar: u32,
    /// Beats per bar.
    pub numerator: u8,
    /// Note value per beat.
    pub denominator: u8,
}

/// The tempo map.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoSnapshot {
    /// Tempo points in beat order; the first is at beat zero.
    pub points: Vec<TempoPointSnapshot>,
    /// Meter points in bar order; the first is at bar 1.
    pub meters: Vec<MeterSnapshot>,
}

/// A clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipSnapshot {
    /// Raw clip ID.
    pub id: RawId,
    /// Display name.
    pub name: String,
    /// Start in samples.
    pub start: i64,
    /// Length in samples.
    pub length: i64,
    /// Content.
    pub source: ClipSource,
    /// Gain in dB.
    pub gain_db: f32,
    /// Mute state.
    pub muted: bool,
    /// Fade-in in samples.
    pub fade_in: i64,
    /// Fade-out in samples.
    pub fade_out: i64,
}

/// A send from a track to a bus track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SendSnapshot {
    /// Raw send ID.
    pub id: RawId,
    /// Raw ID of the destination bus track.
    pub bus: RawId,
    /// Level in dB.
    pub level_db: f32,
    /// Pre-fader tap.
    pub pre_fader: bool,
    /// Enabled.
    pub enabled: bool,
}

/// An effect in a track's insert chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InsertSnapshot {
    /// Raw insert ID.
    pub id: RawId,
    /// Plugin API.
    pub format: PluginFormat,
    /// Bundle path.
    pub path: std::path::PathBuf,
    /// The plugin's ID inside the bundle.
    pub plugin: String,
    /// Bypassed.
    pub bypassed: bool,
    /// `(parameter id, value)` pairs the user has set.
    pub params: Vec<(u32, f64)>,
    /// Opaque plugin state, or empty.
    pub state: Vec<u8>,
}

/// A track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackSnapshot {
    /// Raw track ID.
    pub id: RawId,
    /// Display name.
    pub name: String,
    /// Track kind.
    pub kind: TrackKind,
    /// Colour as `0xRRGGBB`.
    pub color: Option<u32>,
    /// Fader gain in dB.
    pub gain_db: f32,
    /// Pan `-1..=1`.
    pub pan: f32,
    /// Mute state.
    pub muted: bool,
    /// Solo state.
    pub soloed: bool,
    /// Record-arm state.
    pub armed: bool,
    /// Raw ID of the bus track the output feeds (`None` = master; absent
    /// in files written before routing existed).
    #[serde(default)]
    pub output: Option<RawId>,
    /// Sends in creation order (absent in files written before routing
    /// existed).
    #[serde(default)]
    pub sends: Vec<SendSnapshot>,
    /// Insert chain in signal order (absent in files written before inserts
    /// existed).
    #[serde(default)]
    pub inserts: Vec<InsertSnapshot>,
    /// Clips in start order. Files written before clips were renamed used
    /// the key `items`; both spellings load.
    #[serde(alias = "items")]
    pub clips: Vec<ClipSnapshot>,
}

/// A marker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MarkerSnapshot {
    /// Raw marker ID.
    pub id: RawId,
    /// Display name.
    pub name: String,
    /// Position in samples.
    pub position: i64,
    /// Colour as `0xRRGGBB`.
    pub color: Option<u32>,
}

/// A whole session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// [`FORMAT_VERSION`] of the writer.
    pub format_version: u32,
    /// Session name.
    pub name: String,
    /// Sample rate in Hz.
    pub sample_rate_hz: u32,
    /// Tempo and meter.
    pub tempo: TempoSnapshot,
    /// Loop range as `[start, end)` samples.
    pub loop_range: Option<(i64, i64)>,
    /// Tracks in order.
    pub tracks: Vec<TrackSnapshot>,
    /// Markers in position order.
    pub markers: Vec<MarkerSnapshot>,
    /// Next raw ID the session will allocate.
    pub next_id: RawId,
}

impl From<&Clip> for ClipSnapshot {
    fn from(i: &Clip) -> Self {
        Self {
            id: i.id.raw(),
            name: i.name.clone(),
            start: i.start.samples(),
            length: i.length.samples(),
            source: i.source.clone(),
            gain_db: i.gain_db,
            muted: i.muted,
            fade_in: i.fade_in.samples(),
            fade_out: i.fade_out.samples(),
        }
    }
}

impl From<&TrackSend> for SendSnapshot {
    fn from(s: &TrackSend) -> Self {
        Self {
            id: s.id.raw(),
            bus: s.bus.raw(),
            level_db: s.level_db,
            pre_fader: s.pre_fader,
            enabled: s.enabled,
        }
    }
}

impl From<&Insert> for InsertSnapshot {
    fn from(i: &Insert) -> Self {
        Self {
            id: i.id.raw(),
            format: i.plugin.format,
            path: i.plugin.path.clone(),
            plugin: i.plugin.id.clone(),
            bypassed: i.bypassed,
            params: i.params.clone(),
            state: i.state.clone(),
        }
    }
}

impl From<&Track> for TrackSnapshot {
    fn from(t: &Track) -> Self {
        Self {
            id: t.id.raw(),
            name: t.name.clone(),
            kind: t.kind,
            color: t.color,
            gain_db: t.gain_db,
            pan: t.pan,
            muted: t.muted,
            soloed: t.soloed,
            armed: t.armed,
            output: t.output.map(TrackId::raw),
            sends: t.sends.iter().map(SendSnapshot::from).collect(),
            inserts: t.inserts.iter().map(InsertSnapshot::from).collect(),
            clips: t.clips.iter().map(ClipSnapshot::from).collect(),
        }
    }
}

impl From<&Marker> for MarkerSnapshot {
    fn from(m: &Marker) -> Self {
        Self {
            id: m.id.raw(),
            name: m.name.clone(),
            position: m.position.samples(),
            color: m.color,
        }
    }
}

impl From<&TempoMap> for TempoSnapshot {
    fn from(map: &TempoMap) -> Self {
        Self {
            points: map
                .tempo_points()
                .iter()
                .map(|p| TempoPointSnapshot {
                    beat: p.beat.beats(),
                    bpm: p.bpm,
                    ramp: p.ramp,
                })
                .collect(),
            meters: map
                .meter_points()
                .iter()
                .map(|m| MeterSnapshot {
                    bar: m.bar,
                    numerator: m.signature.numerator(),
                    denominator: m.signature.denominator(),
                })
                .collect(),
        }
    }
}

impl From<&Session> for SessionSnapshot {
    fn from(s: &Session) -> Self {
        s.snapshot()
    }
}

fn io_err(context: &str, e: impl core::fmt::Display) -> SuperbError {
    SuperbError::Message(format!("{context}: {e}"))
}

impl Session {
    /// Copies the document into a serialisable snapshot.
    pub fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            format_version: FORMAT_VERSION,
            name: self.name().to_owned(),
            sample_rate_hz: self.sample_rate().hz(),
            tempo: TempoSnapshot::from(self.tempo_map()),
            loop_range: self
                .loop_range()
                .map(|r| (r.start().samples(), r.end().samples())),
            tracks: self.tracks().iter().map(TrackSnapshot::from).collect(),
            markers: self.markers().iter().map(MarkerSnapshot::from).collect(),
            next_id: self.next_raw_id(),
        }
    }

    /// Rebuilds a session from a snapshot, validating IDs, lengths, tempo
    /// and meter values. Newer format versions are refused.
    pub fn from_snapshot(snap: &SessionSnapshot) -> Result<Session> {
        if snap.format_version > FORMAT_VERSION {
            return Err(SuperbError::Message(format!(
                "session format version {} is newer than supported version {FORMAT_VERSION}",
                snap.format_version
            )));
        }
        let rate = SampleRate::new(snap.sample_rate_hz)?;
        let mut session = Session::new(snap.name.clone(), rate);

        // Tempo map.
        let first = snap
            .tempo
            .points
            .first()
            .ok_or(SuperbError::InvalidArgument {
                what: "tempo points (empty)",
            })?;
        if first.beat != 0.0 {
            return Err(SuperbError::InvalidArgument {
                what: "first tempo point must be at beat 0",
            });
        }
        {
            let map = session.tempo_map_mut();
            map.set_initial_tempo(first.bpm)?;
            map.set_tempo_ramp(BeatPosition::ZERO, first.ramp)?;
            for p in &snap.tempo.points[1..] {
                if p.ramp {
                    map.add_tempo_ramp(BeatPosition::new(p.beat), p.bpm)?;
                } else {
                    map.add_tempo(BeatPosition::new(p.beat), p.bpm)?;
                }
            }
            for m in &snap.tempo.meters {
                map.add_meter(m.bar, TimeSignature::new(m.numerator, m.denominator)?)?;
            }
        }

        if let Some((start, end)) = snap.loop_range {
            session.set_loop_range(Some(LoopRange::new(
                TimePosition::from_samples(start),
                TimePosition::from_samples(end),
            )?));
        }

        for t in &snap.tracks {
            let mut track = Track::with_id(TrackId::from_raw(t.id), t.name.clone(), t.kind);
            if !track.id.is_valid() {
                return Err(SuperbError::InvalidArgument { what: "track id 0" });
            }
            track.color = t.color;
            track.gain_db = t.gain_db;
            track.pan = t.pan;
            track.muted = t.muted;
            track.soloed = t.soloed;
            track.armed = t.armed;
            if t.kind == TrackKind::Bus && !t.clips.is_empty() {
                return Err(SuperbError::InvalidArgument {
                    what: "bus tracks hold no clips",
                });
            }
            for i in &t.clips {
                let id = ClipId::from_raw(i.id);
                if !id.is_valid() {
                    return Err(SuperbError::InvalidArgument { what: "clip id 0" });
                }
                let mut clip = Clip::with_id(
                    id,
                    i.name.clone(),
                    TimePosition::from_samples(i.start),
                    TimeDuration::from_samples(i.length),
                    i.source.clone(),
                )?;
                clip.gain_db = i.gain_db;
                clip.muted = i.muted;
                clip.fade_in = TimeDuration::from_samples(i.fade_in);
                clip.fade_out = TimeDuration::from_samples(i.fade_out);
                track.clips.push(clip);
            }
            for i in &t.inserts {
                let insert_id = InsertId::from_raw(i.id);
                if !insert_id.is_valid() {
                    return Err(SuperbError::InvalidArgument {
                        what: "insert id 0",
                    });
                }
                let mut insert = Insert::with_id(
                    insert_id,
                    PluginRef {
                        format: i.format,
                        path: i.path.clone(),
                        id: i.plugin.clone(),
                    },
                );
                insert.bypassed = i.bypassed;
                insert.params = i.params.clone();
                insert.state = i.state.clone();
                track.inserts.push(insert);
            }
            let index = session.track_count();
            session.insert_track(index, track)?;
        }

        // Routing is applied once every track exists, so a track may feed a
        // bus that appears later in the track order.
        for t in &snap.tracks {
            let id = TrackId::from_raw(t.id);
            if let Some(bus) = t.output {
                session.set_track_output(id, Some(TrackId::from_raw(bus)))?;
            }
            for s in &t.sends {
                let send_id = SendId::from_raw(s.id);
                if !send_id.is_valid() {
                    return Err(SuperbError::InvalidArgument { what: "send id 0" });
                }
                let mut send = TrackSend::with_id(send_id, TrackId::from_raw(s.bus));
                send.level_db = s.level_db;
                send.pre_fader = s.pre_fader;
                send.enabled = s.enabled;
                session.add_send(id, send)?;
            }
        }

        for m in &snap.markers {
            let id = MarkerId::from_raw(m.id);
            if !id.is_valid() {
                return Err(SuperbError::InvalidArgument {
                    what: "marker id 0",
                });
            }
            let mut marker =
                Marker::with_id(id, m.name.clone(), TimePosition::from_samples(m.position));
            marker.color = m.color;
            session.insert_marker(marker)?;
        }

        session.reserve(snap.next_id.saturating_sub(1));
        Ok(session)
    }

    /// Serialises the session as pretty-printed JSON.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string_pretty(&self.snapshot()).map_err(|e| io_err("serialise session", e))
    }

    /// Parses a session from JSON produced by [`to_json`](Self::to_json).
    pub fn from_json(json: &str) -> Result<Session> {
        let snap: SessionSnapshot =
            serde_json::from_str(json).map_err(|e| io_err("parse session", e))?;
        Session::from_snapshot(&snap)
    }

    /// Writes the session as JSON to `path`.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::write(path, self.to_json()?)
            .map_err(|e| io_err(&format!("write {}", path.display()), e))
    }

    /// Reads a session written by [`save`](Self::save).
    pub fn load(path: impl AsRef<Path>) -> Result<Session> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| io_err(&format!("read {}", path.display()), e))?;
        Session::from_json(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::MidiNote;

    const R: SampleRate = SampleRate::HZ_48K;

    fn rich_session() -> Session {
        let mut s = Session::new("rich", R);
        {
            let map = s.tempo_map_mut();
            map.set_initial_tempo(100.0).unwrap();
            map.add_tempo_ramp(BeatPosition::new(8.0), 140.0).unwrap();
            map.add_tempo(BeatPosition::new(16.0), 90.0).unwrap();
            map.add_meter(3, TimeSignature::new(7, 8).unwrap()).unwrap();
        }
        s.set_loop_range(Some(
            LoopRange::new(
                TimePosition::from_samples(10),
                TimePosition::from_samples(500),
            )
            .unwrap(),
        ));
        let drums = s.add_track("drums", TrackKind::Audio);
        let keys = s.add_track("keys", TrackKind::Midi);
        let bus = s.add_track("fx", TrackKind::Bus);
        // Routing: drums feed a bus declared *after* them in track order; the
        // fx bus feeds the "room" bus; keys keep a pre-fader send into fx.
        let room = s.add_track("room", TrackKind::Bus);
        s.set_track_output(drums, Some(room)).unwrap();
        s.set_track_output(bus, Some(room)).unwrap();
        s.add_send(keys, TrackSend::new(bus).level_db(-9.0).pre_fader())
            .unwrap();
        let disabled = s.add_send(drums, TrackSend::new(room)).unwrap();
        s.send_mut(disabled).unwrap().enabled = false;
        let mut verb = Insert::new(PluginRef {
            format: PluginFormat::Clap,
            path: "/plugins/verb.clap".into(),
            id: "com.example.verb".into(),
        });
        verb.bypassed = true;
        verb.set_param(3, 0.25);
        verb.state = vec![1, 2, 3];
        s.add_insert(room, verb).unwrap();
        s.add_insert(
            drums,
            Insert::new(PluginRef {
                format: PluginFormat::Clap,
                path: "/plugins/comp.clap".into(),
                id: "com.example.comp".into(),
            }),
        )
        .unwrap();
        s.track_mut(drums).unwrap().color = Some(0xFF8800);
        s.track_mut(drums).unwrap().gain_db = -3.5;
        s.track_mut(keys).unwrap().pan = 0.25;
        s.track_mut(keys).unwrap().soloed = true;
        let mut audio = Clip::new(
            "kick",
            TimePosition::from_samples(100),
            TimeDuration::from_samples(1_000),
            ClipSource::Audio {
                path: "samples/kick.wav".into(),
                offset: TimeDuration::from_samples(32),
            },
        )
        .unwrap();
        audio.fade_in = TimeDuration::from_samples(8);
        audio.gain_db = -1.0;
        s.add_clip(drums, audio).unwrap();
        let notes = vec![
            MidiNote {
                beat: 0.0,
                length: 0.5,
                note: 60,
                velocity: 100,
                channel: 0,
            },
            MidiNote {
                beat: 1.0,
                length: 1.0,
                note: 64,
                velocity: 90,
                channel: 1,
            },
        ];
        let mut midi = Clip::new(
            "chord",
            TimePosition::ZERO,
            TimeDuration::from_samples(48_000),
            ClipSource::Midi { notes },
        )
        .unwrap();
        midi.muted = true;
        s.add_clip(keys, midi).unwrap();
        s.add_clip(
            keys,
            Clip::new(
                "empty",
                TimePosition::from_samples(50_000),
                TimeDuration::from_samples(1),
                ClipSource::Empty,
            )
            .unwrap(),
        )
        .unwrap();
        let m = s.add_marker("drop", TimePosition::from_samples(48_000));
        s.marker_mut(m).unwrap().color = Some(0x00FF00);
        s.add_marker("start", TimePosition::ZERO);
        s
    }

    #[test]
    fn json_round_trip_preserves_everything() {
        let s = rich_session();
        let json = s.to_json().unwrap();
        assert!(json.contains("\"format_version\": 1"));
        assert!(json.contains("kick.wav"));
        let back = Session::from_json(&json).unwrap();
        assert_eq!(back.snapshot(), s.snapshot());
        assert_eq!(back.name(), "rich");
        assert_eq!(
            back.tempo_map().tempo_at(BeatPosition::new(12.0)),
            115.0,
            "ramp survives the round trip"
        );
        assert!(back.tempo_map().tempo_points()[1].ramp);
        // Files written before ramps existed have no `ramp` field.
        let legacy = json
            .replace(",\n        \"ramp\": true", "")
            .replace(",\n        \"ramp\": false", "");
        assert!(!legacy.contains("ramp"), "{legacy}");
        let old = Session::from_json(&legacy).unwrap();
        assert!(old.tempo_map().tempo_points().iter().all(|p| !p.ramp));
        // Files written when clips were still called items load unchanged.
        let renamed = json.replace("\"clips\":", "\"items\":");
        assert!(renamed.contains("\"items\":"));
        assert_eq!(
            Session::from_json(&renamed).unwrap().snapshot(),
            s.snapshot()
        );
        // Routing survives, and files written before routing existed load
        // with master routing and no sends.
        let drums = back.tracks()[0].id();
        let room = back.tracks()[3].id();
        assert_eq!(back.track(drums).unwrap().output(), Some(room));
        assert_eq!(back.send_count(), 2);
        assert!(back.tracks()[1].sends()[0].pre_fader);
        assert!(!back.track(drums).unwrap().sends()[0].enabled);
        let verb = &back.track(room).unwrap().inserts()[0];
        assert_eq!(verb.plugin.id, "com.example.verb");
        assert!(verb.bypassed);
        assert_eq!(verb.param(3), Some(0.25));
        assert_eq!(verb.state, [1, 2, 3]);
        assert_eq!(back.insert_count(), 2);
        let mut pre_routing: serde_json::Value = serde_json::from_str(&json).unwrap();
        for t in pre_routing["tracks"].as_array_mut().unwrap() {
            let t = t.as_object_mut().unwrap();
            t.remove("output");
            t.remove("sends");
            t.remove("inserts");
        }
        let old = Session::from_json(&pre_routing.to_string()).unwrap();
        assert!(old
            .tracks()
            .iter()
            .all(|t| t.output().is_none() && t.sends().is_empty() && t.inserts().is_empty()));
        assert_eq!(
            back.tempo_map().meter_at(BeatPosition::new(20.0)),
            TimeSignature::new(7, 8).unwrap()
        );
        assert_eq!(back.loop_range(), s.loop_range());
        assert_eq!(back.next_raw_id(), s.next_raw_id());
        // Fresh ids keep being unique after a reload.
        let mut back = back;
        let t = back.add_track("new", TrackKind::Audio);
        assert!(s.tracks().iter().all(|x| x.id() != t));
        let snap: SessionSnapshot = (&s).into();
        assert_eq!(snap, s.snapshot());
    }

    #[test]
    fn save_and_load_files() {
        let s = rich_session();
        let dir = std::env::temp_dir().join(format!("superb-session-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.superb.json");
        s.save(&path).unwrap();
        let loaded = Session::load(&path).unwrap();
        assert_eq!(loaded.snapshot(), s.snapshot());
        assert!(Session::load(dir.join("missing.json")).is_err());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(Session::from_json("{ not json").is_err());
        assert!(Session::load(&path).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_snapshots_are_rejected() {
        let good = rich_session().snapshot();

        let mut snap = good.clone();
        snap.format_version = FORMAT_VERSION + 1;
        assert!(Session::from_snapshot(&snap).is_err());

        let mut snap = good.clone();
        snap.sample_rate_hz = 5;
        assert!(Session::from_snapshot(&snap).is_err());

        let mut snap = good.clone();
        snap.tempo.points.clear();
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tempo.points[0].beat = 1.0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tempo.points[0].bpm = -1.0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tempo.meters[0].denominator = 3;
        assert!(Session::from_snapshot(&snap).is_err());

        let mut snap = good.clone();
        snap.loop_range = Some((10, 10));
        assert!(Session::from_snapshot(&snap).is_err());

        let mut snap = good.clone();
        snap.tracks[1].id = snap.tracks[0].id;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[0].id = 0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[0].clips[0].length = 0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[0].clips[0].id = 0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        let dup = snap.tracks[0].clips[0].clone();
        snap.tracks[1].clips.push(dup);
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        let clip = snap.tracks[0].clips[0].clone();
        snap.tracks[2].clips.push(clip); // bus
        assert!(Session::from_snapshot(&snap).is_err());

        // Routing: unknown / non-bus targets, cycles, bad send ids, sends on buses.
        let mut snap = good.clone();
        snap.tracks[0].output = Some(999);
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[0].output = Some(snap.tracks[1].id);
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[3].output = Some(snap.tracks[2].id); // room → fx while fx → room
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[1].sends[0].id = 0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[1].sends[0].id = snap.tracks[0].sends[0].id;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.tracks[1].sends[0].bus = snap.tracks[0].id;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        let send = snap.tracks[1].sends[0].clone();
        snap.tracks[2].sends.push(send); // bus
        assert!(Session::from_snapshot(&snap).is_err());

        let mut snap = good.clone();
        snap.markers[1].id = snap.markers[0].id;
        assert!(Session::from_snapshot(&snap).is_err());

        // Inserts: id 0 and duplicate ids are rejected.
        let mut snap = good.clone();
        snap.tracks[3].inserts[0].id = 0;
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        let dup = snap.tracks[3].inserts[0].clone();
        snap.tracks[0].inserts.push(dup);
        assert!(Session::from_snapshot(&snap).is_err());
        let mut snap = good.clone();
        snap.markers[0].id = 0;
        assert!(Session::from_snapshot(&snap).is_err());

        // A stale next_id is corrected from the ids present.
        let mut snap = good.clone();
        snap.next_id = 1;
        let s = Session::from_snapshot(&snap).unwrap();
        let max = snap
            .tracks
            .iter()
            .flat_map(|t| core::iter::once(t.id).chain(t.sends.iter().map(|s| s.id)))
            .chain(snap.markers.iter().map(|m| m.id))
            .max()
            .unwrap();
        assert!(s.next_raw_id() > max);
    }

    #[test]
    fn older_versions_load() {
        let mut snap = rich_session().snapshot();
        snap.format_version = 0;
        assert!(Session::from_snapshot(&snap).is_ok());
    }
}
