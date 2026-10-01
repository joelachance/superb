//! The engine side of a session: how the document becomes the mixer the
//! engine runs, and how to run that engine by hand.
//!
//! [`rebuild`] turns the [`Document`] into a [`Mixer`] (strips, buses,
//! sends, clip players, live instruments, plugin inserts) and publishes its
//! graph to the engine, which swaps it in between two blocks without a
//! glitch. The structural flows call it for you. [`render_blocks`] runs the
//! engine when no sound card is attached. The pieces are public so a
//! program can assemble its own: [`build_mixer`], [`attach_inserts`],
//! [`collect_handles`], [`publish`], with [`super::bridge`] mapping tracks
//! onto strips and [`super::wiring`] attaching a node per clip.

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::{Result, SuperbError};
use crate::session::{LiveHandles, Session};

/// Rebuilds the mixer from the document and swaps it into the engine.
///
/// The structural flows call this for you (see
/// [`Session::needs_rebuild`]); call it yourself only to pick the moment.
///
/// Made of: `EngineHandle::reclaim` (frees graphs the engine has retired, on
/// this thread) → [`build_mixer`] → [`attach_inserts`] → [`collect_handles`]
/// → [`publish`], and while the engine loop is still in the session,
/// `LiveGraph::apply_pending` so the swap does not wait for a
/// [`render_blocks`]. Afterwards [`Session::live`] points at the new strips,
/// sends and plugins, and [`Session::needs_rebuild`] is clear. Inserts whose
/// plugin failed to load are listed in `live.failed_inserts`; the rest of
/// the mixer runs.
pub fn rebuild(session: &mut Session) -> Result<()> {
    session.engine.reclaim();
    let (mut mixer, wiring) = build_mixer(&session.document, &session.cache, session.channels)?;
    let (inserts, failed_inserts) =
        attach_inserts(&session.document, &mut session.plugins, &mut mixer, &wiring);
    let (strips, sends) = collect_handles(&mixer, &wiring);
    publish(&mut session.engine, mixer)?;
    if let Some(engine_loop) = session.engine_loop.as_mut() {
        // Nothing is pulling audio yet, so nobody would swap the graph in
        // before the next `render_blocks`; do it now so rebuilds never back up.
        engine_loop.engine_mut().processor_mut().apply_pending();
    }
    session.live = LiveHandles {
        strips,
        sends,
        inserts,
        wiring: Some(wiring),
        failed_inserts,
    };
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
/// [`record::steps::collect`](crate::record::steps::collect) after each
/// while a recording is running.
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
            crate::record::steps::collect(&mut recording.capture);
        }
    }
    session.engine.reclaim();
    Ok(out)
}

use crate::primitives::graph::nodes::MonoSynthNode;
use crate::primitives::graph::MidiPort;
use crate::primitives::io::cache::AudioCache;
use crate::primitives::io::live::EngineHandle;
use crate::primitives::mixer::{Mixer, StripId};
use crate::primitives::plugins::PluginNode;
use crate::primitives::session::{Document, TrackKind};
use crate::primitives::InsertId;
use crate::session::{LiveInsert, PluginLoader, SendHandles, StripHandles};
use crate::session::{MixerBinding, Wiring};

/// Builds a fresh mixer for the document: a strip per track and bus,
/// routing and sends, a node per audible clip, and a live instrument
/// on every armed MIDI track so it sounds when you play.
///
/// Made of: `Mixer::new` → `Wiring::wire` → one `MonoSynthNode`
/// plus a MIDI edge per armed MIDI track.
pub fn build_mixer(
    document: &Document,
    cache: &AudioCache,
    channels: usize,
) -> Result<(Mixer, Wiring)> {
    let mut mixer = Mixer::new(channels);
    let wiring = Wiring::wire(document, &mut mixer, cache)?;
    for track in document.tracks() {
        if track.kind != TrackKind::Midi || !track.armed {
            continue;
        }
        let Some(MixerBinding::Track(strip)) = wiring.bridge().binding(track.id()) else {
            continue;
        };
        let synth = mixer
            .graph_mut()
            .add_node(Box::new(MonoSynthNode::new(channels)));
        mixer.connect_source(synth, strip)?;
        mixer.graph_mut().connect_midi(MidiPort::External, synth)?;
    }
    Ok((mixer, wiring))
}

/// Loads every insert in the document and splices it into its strip's chain,
/// with the node's bypass switch set to match the document. Failures
/// are collected, not fatal.
///
/// Made of: `PluginLoader::instantiate` → `PluginNode::new` →
/// `Mixer::add_insert`, per insert.
pub fn attach_inserts(
    document: &Document,
    loader: &mut PluginLoader,
    mixer: &mut Mixer,
    wiring: &Wiring,
) -> (Vec<LiveInsert>, Vec<(InsertId, String)>) {
    let mut live = Vec::new();
    let mut failed = Vec::new();
    for track in document.tracks() {
        let strip: StripId = match wiring.bridge().binding(track.id()) {
            Some(MixerBinding::Track(id)) => id.into(),
            Some(MixerBinding::Bus(id)) => id.into(),
            None => continue,
        };
        for insert in track.inserts() {
            let instance = match loader.instantiate(insert) {
                Ok(instance) => instance,
                Err(e) => {
                    failed.push((insert.id(), e.to_string()));
                    continue;
                }
            };
            let descriptor = instance.descriptor().clone();
            let params = instance.params().to_vec();
            let mut node = PluginNode::new(instance);
            let param_queue = node
                .take_param_sender()
                .expect("a fresh node has its sender");
            let bypass = node.bypass_handle();
            bypass.store(insert.bypassed, core::sync::atomic::Ordering::Relaxed);
            match mixer.add_insert(strip, Box::new(node)) {
                Ok(_) => live.push(LiveInsert {
                    id: insert.id(),
                    descriptor,
                    params,
                    param_queue,
                    bypass,
                }),
                Err(e) => failed.push((insert.id(), e.to_string())),
            }
        }
    }
    (live, failed)
}

/// Collects the atomics the control side keeps after the mixer's graph
/// has been handed to the engine: one strip handle per document track or
/// bus, one send handle per document send.
pub fn collect_handles(mixer: &Mixer, wiring: &Wiring) -> (StripHandles, SendHandles) {
    let strips = wiring
        .bridge()
        .entries()
        .iter()
        .filter_map(|(session_track, binding)| {
            let params = match binding {
                MixerBinding::Track(id) => mixer.track_params(*id).ok()?,
                MixerBinding::Bus(id) => mixer.bus_params(*id).ok()?,
            };
            Some((*session_track, params))
        })
        .collect();
    let sends = wiring
        .bridge()
        .send_entries()
        .iter()
        .filter_map(|(session_send, mixer_send)| {
            Some((*session_send, mixer.send_params(*mixer_send).ok()?))
        })
        .collect();
    (strips, sends)
}

/// Hands the mixer's graph to the engine, which swaps it in at its
/// next block. The mixer's bookkeeping is consumed; keep the handles
/// from [`collect_handles`] first.
///
/// Made of: `Mixer::into_graph` → `EngineHandle::publish`.
pub fn publish(engine: &mut EngineHandle, mixer: Mixer) -> Result<()> {
    engine.publish(mixer.into_graph())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use crate::{session, transport};

    const R: SampleRate = SampleRate::HZ_48K;

    /// A document with one audio track playing a cached 0.5 tone from 0..400 frames.
    fn tone_rig() -> (Session, crate::primitives::TrackId) {
        let mut session = session::create("tone", R, 64).unwrap();
        let vox = session::add_track(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.cache.insert("tone.wav", tone);
        session::add_audio_clip(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_samples(400),
        )
        .unwrap();
        (session, vox)
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
        session::remove_track(&mut session, vox).unwrap();
        assert!(session.needs_rebuild);
        transport::rewind(&mut session).unwrap();
        let gone = render_blocks(&mut session, 2).unwrap();
        assert!(gone.is_silent());
        assert!(!session.needs_rebuild);
        assert!(session.live.strip(vox).is_none());
    }

    #[test]
    fn armed_midi_tracks_get_a_live_instrument() {
        let mut session = session::create("keys", R, 64).unwrap();
        let keys = session::add_track(&mut session, "keys", TrackKind::Midi).unwrap();
        transport::start(&mut session).unwrap();
        let (mixer, wiring) = build_mixer(&session.document, &session.cache, 2).unwrap();
        assert_eq!(
            mixer.graph().node_count(),
            4,
            "master pair + track pair, no instrument"
        );
        drop(wiring);
        session.document.track_mut(keys).unwrap().armed = true;
        let (mixer, _) = build_mixer(&session.document, &session.cache, 2).unwrap();
        assert_eq!(mixer.graph().node_count(), 5, "plus the live synth");
        assert_eq!(mixer.graph().midi_edges().len(), 1);
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
