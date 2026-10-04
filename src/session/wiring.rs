//! How a document is wired into a console: which strip each track got, and
//! a source node for every clip.
//!
//! [`Wiring::wire`] binds strips and buses through the
//! `MixerBridge` (internal), mirrors routing and strip settings, then attaches
//! one node per audible clip: [`BufferPlayerNode`] for audio the
//! [`AudioCache`] holds (a missing entry plays as silence), and
//! [`ScheduledSynthNode`] for MIDI notes converted through the tempo map.
//! The mixer is then ready to be prepared and run; `session::steps` does that
//! for the live engine and `export` for an offline one.

use std::sync::Arc;

use crate::primitives::graph::nodes::{BufferPlayerNode, ScheduledNoteEvent, ScheduledSynthNode};
use crate::primitives::io::cache::AudioCache;
use crate::primitives::mixer::Mixer;
use crate::primitives::session::{Clip, ClipSource, Document, MidiNote, TrackKind};
use crate::primitives::transport::TempoMap;
use crate::primitives::{BeatPosition, ClipId, NodeId, Result, SendId, TrackId};

use crate::session::bridge::{MixerBinding, MixerBridge};

/// One clip wired into the console: the node that plays it and the strip
/// it feeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WiredClip {
    /// Document clip.
    pub clip: ClipId,
    /// Document track that owns the clip.
    pub track: TrackId,
    /// Mixer track the source feeds.
    pub mixer_track: TrackId,
    /// The node playing the clip (a buffer player or a scheduled synth).
    pub node: NodeId,
}

/// The map from a document onto the console built for it.
///
/// Owns the `MixerBridge` side table (track → strip, send → send)
/// plus the clip nodes created. A structural edit to the document means a
/// fresh [`wire`](Wiring::wire) of a fresh mixer.
#[derive(Clone, Debug)]
pub struct Wiring {
    bridge: MixerBridge,
    clips: Vec<WiredClip>,
}

impl Wiring {
    /// Binds strips and buses, mirrors strip settings and routing (track →
    /// bus outputs, sends), and attaches one source node per non-muted,
    /// non-empty clip that can be rendered.
    ///
    /// * [`ClipSource::Empty`] — no node (silence).
    /// * [`ClipSource::Audio`] — [`BufferPlayerNode`] when `cache` has the
    ///   path; otherwise skipped.
    /// * [`ClipSource::Midi`] — [`ScheduledSynthNode`] with notes converted
    ///   through the document tempo map.
    ///
    /// Bus tracks never receive clip sources. The mixer must be prepared
    /// (by the engine) before it renders.
    pub fn wire(document: &Document, mixer: &mut Mixer, cache: &AudioCache) -> Result<Self> {
        let mut bridge = MixerBridge::new();
        bridge.bind(document, mixer);
        bridge.apply_routing(document, mixer)?;
        bridge.apply_strip_params(document, mixer);

        let channels = mixer.channels();
        let tempo = document.tempo_map();
        let mut clips = Vec::new();

        for track in document.tracks() {
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
                clips.push(WiredClip {
                    clip: clip.id(),
                    track: track.id(),
                    mixer_track,
                    node,
                });
            }
        }

        Ok(Self { bridge, clips })
    }

    /// The strip or bus a document track became, if it is in the mixer.
    pub fn strip(&self, track: TrackId) -> Option<MixerBinding> {
        self.bridge.binding(track)
    }

    /// Every document track and the strip or bus it became.
    pub fn strips(&self) -> &[(TrackId, MixerBinding)] {
        self.bridge.entries()
    }

    /// The mixer send a document send became, if any.
    pub fn send(&self, send: SendId) -> Option<SendId> {
        self.bridge.mixer_send(send)
    }

    /// Every document send and the mixer send it became.
    pub fn sends(&self) -> &[(SendId, SendId)] {
        self.bridge.send_entries()
    }

    /// The side table itself, for the steps.
    pub(crate) fn bridge(&self) -> &MixerBridge {
        &self.bridge
    }

    /// The clips that got a node.
    pub fn clips(&self) -> &[WiredClip] {
        &self.clips
    }
}

/// Converts clip-relative MIDI notes into absolute-sample
/// [`ScheduledNoteEvent`]s, clamped to the clip's sample range.
///
/// Note-on/off pairs whose onset falls outside `[clip.start, clip.end)` are
/// dropped. Off events are clamped to the clip end so notes cannot ring
/// past the clip.
pub(crate) fn schedule_midi_notes(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::io::{offline, Engine};
    use crate::primitives::session::TrackKind;
    use crate::primitives::transport::Transport;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use std::path::PathBuf;

    /// Wires a fresh mixer and renders `frames` from `start` offline, the
    /// way `export::render` does.
    fn render(
        document: &Document,
        cache: &AudioCache,
        start: TimePosition,
        frames: usize,
        block: usize,
    ) -> PlanarBuffer {
        let mut mixer = Mixer::new(2);
        Wiring::wire(document, &mut mixer, cache).unwrap();
        let mut transport = Transport::with_tempo_map(document.tempo_map().clone());
        transport.seek(start);
        transport.play();
        let mut engine = Engine::new(mixer, transport);
        offline::render(&mut engine, document.sample_rate(), block, frames).unwrap()
    }

    fn tone_buffer(frames: usize, value: f32) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(2, frames);
        b.fill(value);
        b
    }

    #[test]
    fn wire_skips_empty_and_missing_audio_attaches_cached() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let audio = document.add_track("vox", TrackKind::Audio);
        let empty_clip = Clip::new(
            "e",
            TimePosition::ZERO,
            TimeDuration::from_samples(100),
            ClipSource::Empty,
        )
        .unwrap();
        document.add_clip(audio, empty_clip).unwrap();
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
        document.add_clip(audio, missing).unwrap();
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
        document.add_clip(audio, present).unwrap();

        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(100, 0.5));

        let mut mixer = Mixer::new(2);
        let wiring = Wiring::wire(&document, &mut mixer, &cache).unwrap();
        assert_eq!(wiring.clips().len(), 1);
        assert!(wiring.bridge().mixer_track(audio).is_some());
    }

    #[test]
    fn wired_audio_clip_plays_in_its_place() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let track = document.add_track("vox", TrackKind::Audio);
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
        document.add_clip(track, clip).unwrap();

        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(200, 0.25));

        let audio = render(&document, &cache, TimePosition::ZERO, 400, 64);
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
    fn wired_midi_clip_plays_through_the_scheduled_synth() {
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let track = document.add_track("keys", TrackKind::Midi);
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
        document.add_clip(track, clip).unwrap();

        let audio = render(
            &document,
            &AudioCache::new(),
            TimePosition::ZERO,
            12_000,
            128,
        );
        assert!(audio.peak() > 0.2, "peak {}", audio.peak());
        // After the note (6000 samples) plus a little: near silence
        let tail = &audio.channel(0)[7000..];
        assert!(tail.iter().all(|&s| s.abs() < 1e-5), "tail not silent");
    }

    fn tone_session(rate: SampleRate) -> (Document, AudioCache, TrackId) {
        let mut document = Document::new("routing", rate);
        let vox = document.add_track("vox", TrackKind::Audio);
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
        document.add_clip(vox, clip).unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(400, 0.5));
        (document, cache, vox)
    }

    fn steady_level(document: &Document, cache: &AudioCache) -> f32 {
        let audio = render(document, cache, TimePosition::ZERO, 400, 64);
        // Last sample: gain ramps have settled.
        audio.channel(0)[399].abs()
    }

    #[test]
    fn wiring_honours_bus_routing_and_sends() {
        let (mut document, cache, vox) = tone_session(SampleRate::HZ_48K);
        let direct = steady_level(&document, &cache);
        assert!(direct > 0.1, "direct {direct}");

        // Route through a bus at -6.02 dB: half the level.
        let sub = document.add_track("sub", TrackKind::Bus);
        document.track_mut(sub).unwrap().state.volume_db = -6.0206;
        document.set_track_output(vox, Some(sub)).unwrap();
        let through_bus = steady_level(&document, &cache);
        assert!(
            (through_bus - direct * 0.5).abs() < 1e-3,
            "bus {through_bus} vs direct {direct}"
        );

        // Add a unity post-fader send to a second bus on top: 1.5×.
        let fx = document.add_track("fx", TrackKind::Bus);
        let send = document
            .add_send(vox, crate::primitives::session::TrackSend::new(fx))
            .unwrap();
        let with_send = steady_level(&document, &cache);
        assert!(
            (with_send - direct * 1.5).abs() < 1e-3,
            "send {with_send} vs direct {direct}"
        );

        // Disabled send contributes nothing; a bus feeding a muted bus is silent.
        document.send_mut(send).unwrap().state.enabled = false;
        assert!((steady_level(&document, &cache) - through_bus).abs() < 1e-3);
        document.track_mut(fx).unwrap().state.mute = true;
        document.send_mut(send).unwrap().state.enabled = true;
        assert!((steady_level(&document, &cache) - through_bus).abs() < 1e-3);
        let wiring = {
            let mut mixer = Mixer::new(2);
            Wiring::wire(&document, &mut mixer, &cache).unwrap()
        };
        assert!(wiring.bridge().mixer_send(send).is_some());
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
        let mut document = Document::new("demo", SampleRate::HZ_48K);
        let track = document.add_track("vox", TrackKind::Audio);
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
        document.add_clip(track, clip).unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(100, 1.0));
        let mut mixer = Mixer::new(2);
        let wiring = Wiring::wire(&document, &mut mixer, &cache).unwrap();
        assert!(wiring.clips().is_empty());
    }
}
