//! The engine side of a session: how the document becomes the mixer the
//! engine runs, and how to run that engine by hand.
//!
//! [`rebuild`] turns the [`Document`] into a [`Mixer`] (a strip per track
//! and bus, routing and sends, a node per clip, a live instrument per
//! monitoring MIDI track) and publishes its graph to the engine, which
//! swaps it in between two blocks without a glitch. The structural flows
//! call it for you. [`render_blocks`] runs the engine when no sound card is
//! attached. The two pieces are public so a program can assemble its own:
//! [`build_mixer`] and [`publish`].

use std::sync::Arc;

use crate::primitives::buffers::gain::db_to_linear;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::nodes::{
    BufferPlayerNode, MonoSynthNode, ScheduledNoteEvent, ScheduledSynthNode,
};
use crate::primitives::graph::MidiPort;
use crate::primitives::io::cache::AudioCache;
use crate::primitives::io::live::EngineHandle;
use crate::primitives::mixer::{Mixer, SendParams, SendTap, StripId, StripParams};
use crate::primitives::session::{Clip, ClipSource, Document, MidiNote, TrackKind};
use crate::primitives::transport::TempoMap;
use crate::primitives::{BeatPosition, Result, SendId, SuperbError, TrackId};
use crate::session::{MixerHandles, Session};

/// Rebuilds the mixer from the document and swaps it into the engine.
///
/// The structural flows call this for you, at once while a device drives
/// the engine and otherwise at the next start or render; call it yourself
/// only to pick the moment.
///
/// Made of: `EngineHandle::reclaim` (frees graphs the engine has retired, on
/// this thread) → [`build_mixer`] → [`publish`], and while the engine loop
/// is still in the session, `LiveGraph::apply_pending` so the swap does not
/// wait for a [`render_blocks`]. Afterwards [`Session::mixer`] holds the new
/// strip and send controls.
pub fn rebuild(session: &mut Session) -> Result<()> {
    session.engine.reclaim();
    let (mixer, handles) = build_mixer(&session.document, &session.cache, session.channels)?;
    publish(&mut session.engine, mixer)?;
    if let Some(engine_loop) = session.engine_loop.as_mut() {
        // Nothing is pulling audio yet, so nobody would swap the graph in
        // before the next `render_blocks`; do it now so rebuilds never back up.
        engine_loop.engine_mut().processor_mut().apply_pending();
    }
    session.mixer = handles;
    session.needs_rebuild = false;
    Ok(())
}

/// Renders the next `blocks` blocks of audio by running the engine loop
/// directly, standing in for the sound card. This is how a program with no
/// audio device (a test, a headless renderer) hears the session; once a
/// device has taken the loop (`devices::output`)
/// it fails with a message saying so.
///
/// Made of: [`rebuild`] when needed → `EngineLoop::tick` per block, with
/// [`record::collect`](crate::record::collect) after each while a
/// recording is running.
pub fn render_blocks(session: &mut Session, blocks: usize) -> Result<PlanarBuffer> {
    if session.needs_rebuild {
        rebuild(session)?;
    }
    let block = session.engine.max_frames();
    let engine_loop = session.engine_loop.as_mut().ok_or_else(|| {
        SuperbError::Message("the engine loop is running on a device stream".into())
    })?;
    let mut out = PlanarBuffer::new(session.channels, blocks * block);
    let mut scratch = PlanarBuffer::new(session.channels, block);
    for i in 0..blocks {
        engine_loop.tick(&mut scratch)?;
        out.copy_block_from(i * block, &scratch, block)?;
        if let Some(recording) = session.recording.as_mut() {
            recording.collect();
        }
    }
    session.engine.reclaim();
    Ok(out)
}

/// Builds a fresh mixer for the document and returns it with the handles
/// the control side keeps: a strip per track and bus carrying the track's
/// state, each track's output routed where the document says, a send per
/// document send, a node per audible clip, and a live instrument on every
/// monitoring MIDI track so it sounds when you play.
///
/// The document's and the mixer's ids are separate namespaces; the handles
/// are keyed by the document's, so a flow never sees a mixer id.
///
/// Made of: `Mixer::new` → `Mixer::add_track` / `add_bus` per document
/// track, with `StripParams::set_volume_db` / `set_pan` / `set_mute` /
/// `set_solo` from its state → `Mixer::set_track_output` / `set_bus_output`
/// → `Mixer::add_send` per document send → a [`BufferPlayerNode`] (audio
/// the cache holds) or [`ScheduledSynthNode`] (MIDI notes through the tempo
/// map) per unmuted clip → a [`MonoSynthNode`] with a MIDI edge per
/// monitoring MIDI track.
pub fn build_mixer(
    document: &Document,
    cache: &AudioCache,
    channels: usize,
) -> Result<(Mixer, MixerHandles)> {
    let mut mixer = Mixer::new(channels);
    let strips = add_strips(document, &mut mixer)?;
    route(document, &mut mixer, &strips)?;
    let sends = add_sends(document, &mut mixer, &strips)?;
    add_clip_players(document, &mut mixer, &strips, cache)?;
    add_monitor_instruments(document, &mut mixer, &strips)?;
    let mut handles = MixerHandles {
        strips: Vec::with_capacity(strips.len()),
        sends,
    };
    for (track, strip) in &strips {
        handles.strips.push((*track, strip_params(&mixer, *strip)?));
    }
    Ok((mixer, handles))
}

/// Hands the mixer's graph to the engine, which swaps it in at its
/// next block. The mixer's bookkeeping is consumed; keep the handles
/// from [`build_mixer`] first.
///
/// Made of: `Mixer::into_graph` → `EngineHandle::publish`.
pub fn publish(engine: &mut EngineHandle, mixer: Mixer) -> Result<()> {
    engine.publish(mixer.into_graph())
}

/// Document track → the strip or bus the mixer got for it, in document order.
type Strips = Vec<(TrackId, StripId)>;

fn strip_of(strips: &Strips, track: TrackId) -> Option<StripId> {
    strips.iter().find(|(id, _)| *id == track).map(|(_, s)| *s)
}

fn strip_params(mixer: &Mixer, strip: StripId) -> Result<Arc<StripParams>> {
    match strip {
        StripId::Track(id) => mixer.track_params(id),
        StripId::Bus(id) => mixer.bus_params(id),
    }
}

/// A strip per audio or MIDI track and a bus per bus track, each carrying
/// the track's volume, pan, mute and solo.
fn add_strips(document: &Document, mixer: &mut Mixer) -> Result<Strips> {
    let mut strips = Vec::with_capacity(document.track_count());
    for track in document.tracks() {
        let strip = match track.kind {
            TrackKind::Audio | TrackKind::Midi => {
                StripId::Track(mixer.add_track(track.name.clone()))
            }
            TrackKind::Bus => StripId::Bus(mixer.add_bus(track.name.clone())),
        };
        let params = strip_params(mixer, strip)?;
        params.set_volume_db(track.state.volume_db);
        params.set_pan(track.state.pan);
        params.set_mute(track.state.mute);
        params.set_solo(track.state.solo);
        strips.push((track.id(), strip));
    }
    Ok(strips)
}

/// Every strip feeds the bus its document track outputs to, or the master.
fn route(document: &Document, mixer: &mut Mixer, strips: &Strips) -> Result<()> {
    let master = mixer.master();
    for track in document.tracks() {
        let Some(strip) = strip_of(strips, track.id()) else {
            continue;
        };
        let into = match track.output().and_then(|bus| strip_of(strips, bus)) {
            Some(StripId::Bus(bus)) => bus,
            _ => master,
        };
        match strip {
            StripId::Track(id) => mixer.set_track_output(id, into)?,
            StripId::Bus(id) => mixer.set_bus_output(id, into)?,
        }
    }
    Ok(())
}

/// A mixer send per document send, at its level, tap and on/off.
fn add_sends(
    document: &Document,
    mixer: &mut Mixer,
    strips: &Strips,
) -> Result<Vec<(SendId, Arc<SendParams>)>> {
    let mut sends = Vec::new();
    for track in document.tracks() {
        let Some(StripId::Track(from)) = strip_of(strips, track.id()) else {
            continue;
        };
        for send in track.sends() {
            let Some(StripId::Bus(into)) = strip_of(strips, send.bus()) else {
                continue;
            };
            let tap = if send.pre_fader {
                SendTap::PreFader
            } else {
                SendTap::PostFader
            };
            let id = mixer.add_send(from, into, db_to_linear(send.state.level_db), tap)?;
            let params = mixer.send_params(id)?;
            params.set_enabled(send.state.enabled);
            sends.push((send.id(), params));
        }
    }
    Ok(sends)
}

/// One source node per clip that can sound: a [`BufferPlayerNode`] for
/// audio the cache holds (a clip whose audio is missing plays as silence),
/// a [`ScheduledSynthNode`] for MIDI notes converted through the tempo map.
/// Muted, archived and empty clips get nothing.
fn add_clip_players(
    document: &Document,
    mixer: &mut Mixer,
    strips: &Strips,
    cache: &AudioCache,
) -> Result<()> {
    let channels = mixer.channels();
    let tempo = document.tempo_map();
    for track in document.tracks() {
        let Some(StripId::Track(strip)) = strip_of(strips, track.id()) else {
            continue;
        };
        for clip in track.clips() {
            if clip.muted || clip.archived {
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
                    mixer.graph_mut().add_node(Box::new(player))
                }
                ClipSource::Midi { notes } => {
                    // Clip gain scales note velocities inside `schedule_midi_notes`;
                    // volume, pan and mute stay on the strip.
                    let events = schedule_midi_notes(clip, notes, tempo);
                    let synth = ScheduledSynthNode::new(channels, events);
                    mixer.graph_mut().add_node(Box::new(synth))
                }
            };
            mixer.connect_source(node, strip)?;
        }
    }
    Ok(())
}

/// A [`MonoSynthNode`] fed by the engine's MIDI input on every monitoring
/// MIDI track, so notes you play sound on that track's strip.
fn add_monitor_instruments(document: &Document, mixer: &mut Mixer, strips: &Strips) -> Result<()> {
    let channels = mixer.channels();
    for track in document.tracks() {
        if track.kind != TrackKind::Midi || !track.state.monitor {
            continue;
        }
        let Some(StripId::Track(strip)) = strip_of(strips, track.id()) else {
            continue;
        };
        let synth = mixer
            .graph_mut()
            .add_node(Box::new(MonoSynthNode::new(channels)));
        mixer.connect_source(synth, strip)?;
        mixer.graph_mut().connect_midi(MidiPort::External, synth)?;
    }
    Ok(())
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
    let gain = db_to_linear(clip.gain_db);
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
    use crate::primitives::io::{offline, Engine};
    use crate::primitives::session::{TrackKind, TrackSend};
    use crate::primitives::transport::Transport;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use crate::track;
    use crate::{session, transport};
    use std::path::PathBuf;

    const R: SampleRate = SampleRate::HZ_48K;

    /// A session with one audio track playing a cached 0.5 tone from 0..400 frames.
    fn tone_rig() -> (Session, crate::primitives::TrackId) {
        let mut session = session::create("tone", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.cache.insert("tone.wav", tone);
        crate::clip::add(&mut session, vox, "tone.wav", TimePosition::ZERO).unwrap();
        (session, vox)
    }

    fn tone_buffer(frames: usize, value: f32) -> PlanarBuffer {
        let mut b = PlanarBuffer::new(2, frames);
        b.fill(value);
        b
    }

    fn audio_clip(name: &str, start: i64, length: i64) -> Clip {
        Clip::new(
            name,
            TimePosition::from_samples(start),
            TimeDuration::from_samples(length),
            ClipSource::Audio {
                path: PathBuf::from("tone.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap()
    }

    /// Builds a mixer and renders `frames` from `start` offline, the way
    /// `export::bounce` does.
    fn render(
        document: &Document,
        cache: &AudioCache,
        start: TimePosition,
        frames: usize,
    ) -> PlanarBuffer {
        let (mixer, _) = build_mixer(document, cache, 2).unwrap();
        let mut transport = Transport::with_tempo_map(document.tempo_map().clone());
        transport.seek(start);
        transport.play();
        let mut engine = Engine::new(mixer, transport);
        offline::render(&mut engine, document.sample_rate(), 64, frames).unwrap()
    }

    /// Level of the last sample of a 400-frame render: ramps have settled.
    fn steady_level(document: &Document, cache: &AudioCache) -> f32 {
        render(document, cache, TimePosition::ZERO, 400).channel(0)[399].abs()
    }

    #[test]
    fn a_structural_edit_is_heard_after_the_rebuild() {
        let (mut session, _) = tone_rig();
        transport::start(&mut session).unwrap();
        let before = render_blocks(&mut session, 2).unwrap();
        assert!(before.peak() > 0.3);
        // Mute the track in the document directly: mirrored on rebuild.
        let vox = session.document.tracks()[0].id();
        session
            .history
            .push(
                &mut session.document,
                crate::primitives::session::commands::set_track_mute(vox, true),
            )
            .unwrap();
        transport::rewind(&mut session).unwrap();
        rebuild(&mut session).unwrap();
        let after = render_blocks(&mut session, 3).unwrap();
        assert!(after.channel(0)[150..].iter().all(|&s| s.abs() < 1e-6));
        // Removing the track marks a rebuild; `render_blocks` runs it.
        track::remove(&mut session, vox).unwrap();
        assert!(session.needs_rebuild);
        transport::rewind(&mut session).unwrap();
        let gone = render_blocks(&mut session, 2).unwrap();
        assert!(gone.is_silent());
        assert!(!session.needs_rebuild);
        assert!(session.mixer.strip(vox).is_none());
    }

    #[test]
    fn handles_carry_the_document_state_onto_the_strips_and_sends() {
        let mut document = Document::new("demo", R);
        let lead = document.add_track("lead", TrackKind::Audio);
        let fx = document.add_track("fx", TrackKind::Bus);
        {
            let state = &mut document.track_mut(lead).unwrap().state;
            state.volume_db = -6.0;
            state.pan = -0.5;
            state.mute = true;
        }
        let send = document
            .add_send(lead, TrackSend::new(fx).level_db(-12.0))
            .unwrap();
        document.send_mut(send).unwrap().state.enabled = false;

        let (mixer, handles) = build_mixer(&document, &AudioCache::new(), 2).unwrap();
        assert_eq!(handles.strips.len(), 2, "one handle per track and bus");
        assert_eq!(mixer.tracks().len(), 1);
        assert_eq!(mixer.buses().len(), 2, "fx and the master");
        let strip = handles.strip(lead).unwrap();
        assert!((strip.volume_db() + 6.0).abs() < 1e-6);
        assert!((strip.pan() + 0.5).abs() < 1e-6);
        assert!(strip.is_muted());
        assert!(!handles.strip(fx).unwrap().is_muted());
        let live_send = handles.send(send).unwrap();
        assert!((live_send.level_db() + 12.0).abs() < 1e-4);
        assert!(!live_send.is_enabled());
        assert!(handles.strip(TrackId::from_raw(99)).is_none());
        assert!(handles.send(SendId::from_raw(99)).is_none());
    }

    #[test]
    fn only_unmuted_clips_with_audio_get_a_node() {
        let mut document = Document::new("demo", R);
        let vox = document.add_track("vox", TrackKind::Audio);
        document
            .add_clip(
                vox,
                Clip::new(
                    "empty",
                    TimePosition::ZERO,
                    TimeDuration::from_samples(100),
                    ClipSource::Empty,
                )
                .unwrap(),
            )
            .unwrap();
        let missing = Clip::new(
            "missing",
            TimePosition::from_samples(100),
            TimeDuration::from_samples(100),
            ClipSource::Audio {
                path: PathBuf::from("missing.wav"),
                offset: TimeDuration::ZERO,
            },
        )
        .unwrap();
        document.add_clip(vox, missing).unwrap();
        let mut muted = audio_clip("muted", 200, 100);
        muted.muted = true;
        document.add_clip(vox, muted).unwrap();
        document
            .add_clip(vox, audio_clip("present", 300, 100))
            .unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(100, 0.5));

        let (mixer, _) = build_mixer(&document, &cache, 2).unwrap();
        assert_eq!(
            mixer.graph().node_count(),
            5,
            "master pair + track pair + one player"
        );
    }

    #[test]
    fn an_audio_clip_plays_in_its_place() {
        let mut document = Document::new("demo", R);
        let track = document.add_track("vox", TrackKind::Audio);
        document
            .add_clip(track, audio_clip("take", 100, 200))
            .unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(200, 0.25));

        let audio = render(&document, &cache, TimePosition::ZERO, 400);
        assert_eq!(audio.frames(), 400);
        assert!(
            audio.channel(0)[..100].iter().all(|&s| s == 0.0),
            "silence before"
        );
        // The exact level depends on the constant-power pan law (−3 dB at
        // centre), so only check that the clip is clearly audible.
        let peak = audio.channel(0)[100..300]
            .iter()
            .fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.05, "peak {peak}");
        assert!(
            audio.channel(0)[300..].iter().all(|&s| s.abs() < 1e-6),
            "silence after"
        );
    }

    #[test]
    fn a_midi_clip_plays_through_the_scheduled_synth() {
        let mut document = Document::new("demo", R);
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

        let audio = render(&document, &AudioCache::new(), TimePosition::ZERO, 12_000);
        assert!(audio.peak() > 0.2, "peak {}", audio.peak());
        let tail = &audio.channel(0)[7000..];
        assert!(tail.iter().all(|&s| s.abs() < 1e-5), "tail not silent");
    }

    #[test]
    fn routing_and_sends_follow_the_document() {
        let mut document = Document::new("routing", R);
        let vox = document.add_track("vox", TrackKind::Audio);
        document.add_clip(vox, audio_clip("take", 0, 400)).unwrap();
        let mut cache = AudioCache::new();
        cache.insert("tone.wav", tone_buffer(400, 0.5));
        let direct = steady_level(&document, &cache);
        assert!(direct > 0.1, "direct {direct}");

        // Through a bus at -6.02 dB: half the level.
        let sub = document.add_track("sub", TrackKind::Bus);
        document.track_mut(sub).unwrap().state.volume_db = -6.0206;
        document.set_track_output(vox, Some(sub)).unwrap();
        let through_bus = steady_level(&document, &cache);
        assert!(
            (through_bus - direct * 0.5).abs() < 1e-3,
            "bus {through_bus} vs direct {direct}"
        );

        // Plus a unity post-fader send into a second bus: 1.5×.
        let fx = document.add_track("fx", TrackKind::Bus);
        let send = document.add_send(vox, TrackSend::new(fx)).unwrap();
        let with_send = steady_level(&document, &cache);
        assert!(
            (with_send - direct * 1.5).abs() < 1e-3,
            "send {with_send} vs direct {direct}"
        );

        // A send that is off adds nothing; a muted bus is silent.
        document.send_mut(send).unwrap().state.enabled = false;
        assert!((steady_level(&document, &cache) - through_bus).abs() < 1e-3);
        document.send_mut(send).unwrap().state.enabled = true;
        document.track_mut(fx).unwrap().state.mute = true;
        assert!((steady_level(&document, &cache) - through_bus).abs() < 1e-3);

        // A bus chain, and a pre-fader send ignores the track fader.
        let room = document.add_track("room", TrackKind::Bus);
        document.set_track_output(sub, Some(room)).unwrap();
        document.track_mut(room).unwrap().state.volume_db = -6.0206;
        assert!((steady_level(&document, &cache) - direct * 0.25).abs() < 1e-3);
        document.track_mut(fx).unwrap().state.mute = false;
        document.send_mut(send).unwrap().pre_fader = true;
        document.track_mut(vox).unwrap().state.volume_db = -100.0;
        // The pre-fader tap skips the track's fader and its constant-power
        // pan law; the bus's balance law is unity at centre, so the raw tone
        // comes through at 0.5.
        let pre = steady_level(&document, &cache);
        assert!(
            (pre - 0.5).abs() < 1e-2,
            "only the pre-fader send is heard: {pre}"
        );
        let (mixer, handles) = build_mixer(&document, &cache, 2).unwrap();
        assert_eq!(mixer.sends().len(), 1);
        assert_eq!(handles.sends.len(), 1);
    }

    #[test]
    fn monitoring_midi_tracks_get_a_live_instrument() {
        let mut session = session::create("keys", R, 64).unwrap();
        let keys = track::add(&mut session, "keys", TrackKind::Midi).unwrap();
        transport::start(&mut session).unwrap();
        let (mixer, _) = build_mixer(&session.document, &session.cache, 2).unwrap();
        assert_eq!(
            mixer.graph().node_count(),
            4,
            "master pair + track pair, no instrument"
        );
        session.document.track_mut(keys).unwrap().state.monitor = true;
        let (mixer, _) = build_mixer(&session.document, &session.cache, 2).unwrap();
        assert_eq!(mixer.graph().node_count(), 5, "plus the live synth");
        assert_eq!(mixer.graph().midi_edges().len(), 1);
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
        let tempo = TempoMap::new(R, 120.0);
        // One beat at 120 bpm is 24 000 samples: clamped to the clip end.
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
        assert_eq!(events[1].sample, 6000);
    }

    #[test]
    fn render_blocks_fails_once_the_loop_has_left() {
        let (mut session, _) = tone_rig();
        session.engine_loop = None;
        assert!(matches!(
            render_blocks(&mut session, 1),
            Err(SuperbError::Message(_))
        ));
    }
}
