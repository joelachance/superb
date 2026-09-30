//! Offline playback of session clips onto mixer strips.
//!
//! Builds on [`crate::play::bridge::SessionMixerBridge`]: after strips exist,
//! [`SessionPlayer::wire`] attaches a source node per audible clip —
//! [`BufferPlayerNode`] for cached
//! audio (and empty clips stay silent), [`ScheduledSynthNode`] for MIDI —
//! then [`render_region`] seeks the transport and runs the offline engine.
//!
//! Audio files are **not** decoded here by default. Populate an
//! [`AudioCache`] with planar buffers (tests use synthetic data; apps can
//! fill via `superb::primitives::io::files::decode` behind the `files` feature). Missing
//! cache entries for `ClipSource::Audio` are skipped (treated as silence)
//! so a partial cache still renders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::nodes::{BufferPlayerNode, ScheduledNoteEvent, ScheduledSynthNode};
use crate::primitives::io::{offline, Engine};
use crate::primitives::mixer::Mixer;
use crate::primitives::session::{Clip, ClipSource, MidiNote, Session, TrackKind};
use crate::primitives::transport::{TempoMap, Transport};
#[cfg(feature = "files")]
use crate::primitives::SampleRate;
use crate::primitives::{BeatPosition, ClipId, NodeId, Result, SuperbError, TimePosition, TrackId};

use crate::play::bridge::{MixerBinding, SessionMixerBridge};

/// In-memory path → planar buffer map used by audio clip players.
#[derive(Clone, Debug, Default)]
pub struct AudioCache {
    entries: HashMap<PathBuf, Arc<PlanarBuffer>>,
}

impl AudioCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts (or replaces) a buffer for `path`, returning the shared handle.
    pub fn insert(&mut self, path: impl Into<PathBuf>, buffer: PlanarBuffer) -> Arc<PlanarBuffer> {
        let arc = Arc::new(buffer);
        self.entries.insert(path.into(), Arc::clone(&arc));
        arc
    }

    /// Shared buffer for `path`, if present.
    pub fn get(&self, path: &Path) -> Option<Arc<PlanarBuffer>> {
        self.entries.get(path).map(Arc::clone)
    }

    /// Number of cached files.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Decodes `path` at `target_rate` (resampled when needed) and caches it.
    ///
    /// Requires the facade `files` feature (symphonia + rubato).
    #[cfg(feature = "files")]
    pub fn load(
        &mut self,
        path: impl AsRef<Path>,
        target_rate: SampleRate,
    ) -> Result<Arc<PlanarBuffer>> {
        let path = path.as_ref();
        if let Some(existing) = self.get(path) {
            return Ok(existing);
        }
        let file = crate::primitives::io::files::decode_at_rate(path, target_rate)?;
        Ok(self.insert(path.to_path_buf(), file.buffer))
    }
}

/// One wired source: session clip → graph node on a mixer track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackSource {
    /// Session clip.
    pub clip: ClipId,
    /// Session track that owns the clip.
    pub session_track: TrackId,
    /// Mixer track the source feeds.
    pub mixer_track: TrackId,
    /// Graph node id of the player / synth.
    pub node: NodeId,
}

/// Wired session → mixer playback graph.
///
/// Owns the [`SessionMixerBridge`] side table plus the list of source nodes
/// created for clips. Structural edits on the session require a fresh
/// [`wire`](SessionPlayer::wire) (or a later incremental update).
#[derive(Clone, Debug)]
pub struct SessionPlayer {
    bridge: SessionMixerBridge,
    sources: Vec<PlaybackSource>,
}

impl SessionPlayer {
    /// Syncs the mixer to the session, mirrors strip params and routing
    /// (track → bus outputs, sends), and attaches one source node per
    /// non-muted, non-empty clip that can be rendered.
    ///
    /// * [`ClipSource::Empty`] — no node (silence).
    /// * [`ClipSource::Audio`] — [`BufferPlayerNode`] when `cache` has the
    ///   path; otherwise skipped.
    /// * [`ClipSource::Midi`] — [`ScheduledSynthNode`] with notes converted
    ///   through the session tempo map.
    ///
    /// Bus tracks never receive clip sources. Returns the player; the mixer
    /// must be `prepare`d (via the engine) before rendering.
    pub fn wire(session: &Session, mixer: &mut Mixer, cache: &AudioCache) -> Result<Self> {
        let mut bridge = SessionMixerBridge::new();
        bridge.sync(session, mixer);
        bridge.apply_routing(session, mixer)?;
        bridge.apply_strip_params(session, mixer);

        let channels = mixer.channels();
        let tempo = session.tempo_map();
        let mut sources = Vec::new();

        for track in session.tracks() {
            if track.kind == TrackKind::Bus {
                continue;
            }
            let Some(MixerBinding::Track(mixer_track)) = bridge.binding(track.id()) else {
                continue;
            };
            for clip in track.clips() {
                if clip.muted {
                    continue;
                }
                let node = match &clip.source {
                    ClipSource::Empty => continue,
                    ClipSource::Audio { path, offset } => {
                        let Some(buffer) = cache.get(path) else {
                            continue;
                        };
                        let mut player = BufferPlayerNode::new(buffer, channels);
                        player.set_region(clip.start(), clip.length(), *offset);
                        player.set_gain_db(clip.gain_db);
                        player.set_fades(clip.fade_in, clip.fade_out);
                        let id = mixer.graph_mut().add_node(Box::new(player));
                        mixer.connect_source(id, mixer_track)?;
                        id
                    }
                    ClipSource::Midi { notes } => {
                        // Clip gain scales note velocities inside `schedule_midi_notes`;
                        // track-level gain/pan/mute stay on the strip.
                        let events = schedule_midi_notes(clip, notes, tempo);
                        let synth = ScheduledSynthNode::new(channels, events);
                        let id = mixer.graph_mut().add_node(Box::new(synth));
                        mixer.connect_source(id, mixer_track)?;
                        id
                    }
                };
                sources.push(PlaybackSource {
                    clip: clip.id(),
                    session_track: track.id(),
                    mixer_track,
                    node,
                });
            }
        }

        Ok(Self { bridge, sources })
    }

    /// Side table from the last [`wire`](Self::wire).
    pub fn bridge(&self) -> &SessionMixerBridge {
        &self.bridge
    }

    /// Wired clip sources.
    pub fn sources(&self) -> &[PlaybackSource] {
        &self.sources
    }
}

/// Converts clip-relative MIDI notes into absolute-sample
/// [`ScheduledNoteEvent`]s, clamped to the clip's sample range.
///
/// Note-on/off pairs whose onset falls outside `[clip.start, clip.end)` are
/// dropped. Off events are clamped to the clip end so notes cannot ring
/// past the clip.
pub fn schedule_midi_notes(
    clip: &Clip,
    notes: &[MidiNote],
    tempo: &TempoMap,
) -> Arc<[ScheduledNoteEvent]> {
    let clip_start = clip.start();
    let clip_end = clip.end();
    let clip_start_beat = tempo.samples_to_beat(clip_start).beats();
    let gain = crate::primitives::buffers::gain::db_to_linear(clip.gain_db);
    let mut events = Vec::with_capacity(notes.len() * 2);

    for n in notes {
        if n.length <= 0.0 {
            continue;
        }
        let on_beat = BeatPosition::new(clip_start_beat + n.beat);
        let off_beat = BeatPosition::new(clip_start_beat + n.beat + n.length);
        let mut on_sample = tempo.beat_to_samples(on_beat);
        let mut off_sample = tempo.beat_to_samples(off_beat);
        if on_sample < clip_start {
            on_sample = clip_start;
        }
        if on_sample >= clip_end {
            continue;
        }
        if off_sample > clip_end {
            off_sample = clip_end;
        }
        if off_sample <= on_sample {
            continue;
        }
        let velocity = ((n.velocity as f32 * gain).round() as u8).clamp(1, 127);
        let channel = n.channel.min(15);
        events.push(ScheduledNoteEvent {
            sample: on_sample.samples(),
            on: true,
            note: n.note,
            velocity,
            channel,
        });
        events.push(ScheduledNoteEvent {
            sample: off_sample.samples(),
            on: false,
            note: n.note,
            velocity: 0,
            channel,
        });
    }

    events.sort_by(|a, b| a.sample.cmp(&b.sample).then_with(|| a.on.cmp(&b.on)));
    Arc::from(events)
}

/// Options for [`render_region`].
#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// Block size for the offline engine (must be `> 0`).
    pub block_size: usize,
    /// Mixer channel count (2 = stereo).
    pub channels: usize,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            block_size: 256,
            channels: 2,
        }
    }
}

/// Builds a mixer, wires session playback, seeks to `start`, and renders
/// `frames` of audio offline.
///
/// Clear API for “play this session region”:
///
/// ```
/// use superb::play::{render_region, AudioCache, RenderOptions};
/// use superb::prelude::*;
///
/// let mut session = Session::new("demo", SampleRate::HZ_48K);
/// let track = session.add_track("keys", TrackKind::Midi);
/// let notes = vec![superb::primitives::session::MidiNote {
///     beat: 0.0, length: 0.5, note: 69, velocity: 100, channel: 0,
/// }];
/// let clip = Clip::new(
///     "n",
///     TimePosition::ZERO,
///     TimeDuration::from_samples(24_000),
///     ClipSource::Midi { notes },
/// ).unwrap();
/// session.add_clip(track, clip).unwrap();
///
/// let audio = render_region(
///     &session,
///     &AudioCache::new(),
///     TimePosition::ZERO,
///     24_000,
///     &RenderOptions::default(),
/// ).unwrap();
/// assert_eq!(audio.frames(), 24_000);
/// assert!(audio.peak() > 0.1);
/// ```
pub fn render_region(
    session: &Session,
    cache: &AudioCache,
    start: TimePosition,
    frames: usize,
    options: &RenderOptions,
) -> Result<PlanarBuffer> {
    if options.block_size == 0 {
        return Err(SuperbError::InvalidArgument { what: "block_size" });
    }
    if options.channels == 0 {
        return Err(SuperbError::InvalidArgument { what: "channels" });
    }

    let rate = session.sample_rate();
    let mut mixer = Mixer::new(options.channels);
    let _player = SessionPlayer::wire(session, &mut mixer, cache)?;

    let mut transport = Transport::with_tempo_map(session.tempo_map().clone());
    // Offline region render does not auto-enable the session loop;
    // callers that want looping can `set_loop` on a custom engine path.
    if let Some(loop_range) = session.loop_range() {
        transport.set_loop(Some(loop_range));
    }
    transport.seek(start);
    transport.play();

    let mut engine = Engine::new(mixer, transport);
    offline::render(&mut engine, rate, options.block_size, frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration};

    fn tone_buffer(frames: usize, value: f32) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(2, frames);
        b.fill(value);
        b
    }

    #[test]
    fn wire_skips_empty_and_missing_audio_attaches_cached() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let audio = session.add_track("vox", TrackKind::Audio);
        let empty_clip = Clip::new(
            "e",
            TimePosition::ZERO,
            TimeDuration::from_samples(100),
            ClipSource::Empty,
        )
        .unwrap();
        session.add_clip(audio, empty_clip).unwrap();
        let missing = Clip::new(
            "m",
            TimePosition::from_samples(100),
            TimeDuration::from_samples(100),
            ClipSource::Audio {
                path: PathBuf::from("missing.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap();
        session.add_clip(audio, missing).unwrap();
        let present = Clip::new(
            "p",
            TimePosition::from_samples(200),
            TimeDuration::from_samples(100),
            ClipSource::Audio {
                path: PathBuf::from("tone.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap();
        session.add_clip(audio, present).unwrap();

        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(100, 0.5));

        let mut mixer = Mixer::new(2);
        let player = SessionPlayer::wire(&session, &mut mixer, &cache).unwrap();
        assert_eq!(player.sources().len(), 1);
        assert!(player.bridge().mixer_track(audio).is_some());
    }

    #[test]
    fn render_region_plays_cached_audio_clip() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let track = session.add_track("vox", TrackKind::Audio);
        let clip = Clip::new(
            "take",
            TimePosition::from_samples(100),
            TimeDuration::from_samples(200),
            ClipSource::Audio {
                path: PathBuf::from("tone.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap();
        session.add_clip(track, clip).unwrap();

        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(200, 0.25));

        let audio = render_region(
            &session,
            &cache,
            TimePosition::ZERO,
            400,
            &RenderOptions {
                block_size: 64,
                channels: 2,
            },
        )
        .unwrap();
        assert_eq!(audio.frames(), 400);
        // Before the clip: silence
        assert!(audio.channel(0)[..100].iter().all(|&s| s == 0.0));
        // Inside the clip the 0.25 tone is audible. The exact level depends on
        // the strip's constant-power pan law (−3 dB at centre), so only check
        // that it is clearly non-zero.
        let mid = &audio.channel(0)[100..300];
        let peak = mid.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "peak {peak}");
        // After the clip: silence
        assert!(audio.channel(0)[300..].iter().all(|&s| s.abs() < 1e-6));
    }

    #[test]
    fn render_region_plays_midi_clip_through_scheduled_synth() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let track = session.add_track("keys", TrackKind::Midi);
        let notes = vec![MidiNote {
            beat: 0.0,
            length: 0.25,
            note: 69,
            velocity: 127,
            channel: 0,
        }];
        // 0.25 beat at 120 bpm = 0.125 s = 6000 samples at 48 kHz
        let clip = Clip::new(
            "n",
            TimePosition::ZERO,
            TimeDuration::from_samples(12_000),
            ClipSource::Midi { notes },
        )
        .unwrap();
        session.add_clip(track, clip).unwrap();

        let audio = render_region(
            &session,
            &AudioCache::new(),
            TimePosition::ZERO,
            12_000,
            &RenderOptions {
                block_size: 128,
                channels: 2,
            },
        )
        .unwrap();
        assert!(audio.peak() > 0.2, "peak {}", audio.peak());
        // After the note (6000 samples) plus a little: near silence
        let tail = &audio.channel(0)[7000..];
        assert!(tail.iter().all(|&s| s.abs() < 1e-5), "tail not silent");
    }

    fn tone_session(rate: SampleRate) -> (Session, AudioCache, TrackId) {
        let mut session = Session::new("routing", rate);
        let vox = session.add_track("vox", TrackKind::Audio);
        let clip = Clip::new(
            "take",
            TimePosition::ZERO,
            TimeDuration::from_samples(400),
            ClipSource::Audio {
                path: PathBuf::from("tone.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap();
        session.add_clip(vox, clip).unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(400, 0.5));
        (session, cache, vox)
    }

    fn steady_level(session: &Session, cache: &AudioCache) -> f32 {
        let audio = render_region(
            session,
            cache,
            TimePosition::ZERO,
            400,
            &RenderOptions {
                block_size: 64,
                channels: 2,
            },
        )
        .unwrap();
        // Last sample: gain ramps have settled.
        audio.channel(0)[399].abs()
    }

    #[test]
    fn render_region_honours_session_bus_routing_and_sends() {
        let (mut session, cache, vox) = tone_session(SampleRate::HZ_48K);
        let direct = steady_level(&session, &cache);
        assert!(direct > 0.1, "direct {direct}");

        // Route through a bus at -6.02 dB: half the level.
        let sub = session.add_track("sub", TrackKind::Bus);
        session.track_mut(sub).unwrap().gain_db = -6.0206;
        session.set_track_output(vox, Some(sub)).unwrap();
        let through_bus = steady_level(&session, &cache);
        assert!(
            (through_bus - direct * 0.5).abs() < 1e-3,
            "bus {through_bus} vs direct {direct}"
        );

        // Add a unity post-fader send to a second bus on top: 1.5×.
        let fx = session.add_track("fx", TrackKind::Bus);
        let send = session
            .add_send(vox, crate::primitives::session::TrackSend::new(fx))
            .unwrap();
        let with_send = steady_level(&session, &cache);
        assert!(
            (with_send - direct * 1.5).abs() < 1e-3,
            "send {with_send} vs direct {direct}"
        );

        // Disabled send contributes nothing; a bus feeding a muted bus is silent.
        session.send_mut(send).unwrap().enabled = false;
        assert!((steady_level(&session, &cache) - through_bus).abs() < 1e-3);
        session.track_mut(fx).unwrap().muted = true;
        session.send_mut(send).unwrap().enabled = true;
        assert!((steady_level(&session, &cache) - through_bus).abs() < 1e-3);
        let player = {
            let mut mixer = Mixer::new(2);
            SessionPlayer::wire(&session, &mut mixer, &cache).unwrap()
        };
        assert!(player.bridge().mixer_send(send).is_some());
    }

    #[test]
    fn schedule_midi_clamps_to_clip_and_sorts() {
        let clip = Clip::new(
            "n",
            TimePosition::from_samples(1000),
            TimeDuration::from_samples(5000),
            ClipSource::Empty,
        )
        .unwrap();
        let tempo = TempoMap::new(SampleRate::HZ_48K, 120.0);
        // note starts 0 beats into clip, length 1 beat (= 24000 samples) → clamped to clip end
        let notes = [MidiNote {
            beat: 0.0,
            length: 1.0,
            note: 60,
            velocity: 80,
            channel: 0,
        }];
        let events = schedule_midi_notes(&clip, &notes, &tempo);
        assert_eq!(events.len(), 2);
        assert!(events[0].on);
        assert_eq!(events[0].sample, 1000);
        assert!(!events[1].on);
        assert_eq!(events[1].sample, 6000); // clamped to clip end
    }

    #[test]
    fn muted_clip_is_not_wired() {
        let mut session = Session::new("demo", SampleRate::HZ_48K);
        let track = session.add_track("vox", TrackKind::Audio);
        let mut clip = Clip::new(
            "take",
            TimePosition::ZERO,
            TimeDuration::from_samples(100),
            ClipSource::Audio {
                path: PathBuf::from("tone.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap();
        clip.muted = true;
        session.add_clip(track, clip).unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(100, 1.0));
        let mut mixer = Mixer::new(2);
        let player = SessionPlayer::wire(&session, &mut mixer, &cache).unwrap();
        assert!(player.sources().is_empty());
    }
}
