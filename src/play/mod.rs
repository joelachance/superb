//! Playing the song: build the console from the document, hand it to the
//! running engine, and drive the transport.
//!
//! The engine never sees the document. [`sync`] turns the session into a
//! [`Mixer`](crate::primitives::mixer::Mixer) (tracks, buses, sends, clip players, live instruments, plugin
//! inserts) and publishes its graph to the engine, which swaps it in
//! between two blocks without a glitch. Flows that start audio call `sync`
//! when the rig says it is needed; call it yourself to hear an edit at once.
//!
//! Made of `superb::primitives::io::live` (the running engine),
//! `superb::primitives::mixer` (the console) and the two modules below:
//! [`bridge`] maps session objects onto console objects and [`playback`]
//! attaches a player per clip. Each function lists the steps it is made of;
//! the steps are public in [`steps`].

pub mod bridge;
pub mod playback;

pub use bridge::{MixerBinding, SessionMixerBridge};
pub use playback::{
    render_region, schedule_midi_notes, AudioCache, PlaybackSource, RenderOptions, SessionPlayer,
};

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::io::live::EngineCommand;
use crate::primitives::transport::TransportState;
use crate::primitives::{Result, SuperbError, TimePosition};
use crate::rig::{LiveHandles, Rig};

/// Rebuilds the console from the song and swaps it into the engine.
///
/// Made of: [`steps::build_console`] → [`steps::attach_inserts`] →
/// [`steps::collect_handles`] → [`steps::publish`]. Afterwards
/// [`Rig::live`] points at the new strips, sends and plugins, and
/// [`Rig::needs_sync`] is clear. Inserts whose plugin failed to load are
/// listed in `live.failed_inserts`; the rest of the console runs.
pub fn sync(rig: &mut Rig) -> Result<()> {
    let (mut mixer, player) = steps::build_console(&rig.session, &rig.cache, rig.channels)?;
    let (inserts, failed_inserts) =
        steps::attach_inserts(&rig.session, &mut rig.plugins, &mut mixer, &player);
    let (strips, sends) = steps::collect_handles(&mixer, &player);
    steps::publish(&mut rig.engine, mixer)?;
    rig.live = LiveHandles {
        strips,
        sends,
        inserts,
        player: Some(player),
        failed_inserts,
    };
    rig.needs_sync = false;
    Ok(())
}

/// Starts the transport from where it is. Syncs first if the song changed.
///
/// Made of: [`sync`] when needed → `LiveEngine::send(Play)`.
pub fn start(rig: &mut Rig) -> Result<()> {
    if rig.needs_sync {
        sync(rig)?;
    }
    rig.engine.send(EngineCommand::Play)
}

/// Stops the transport where it is.
///
/// Made of: `LiveEngine::send(Stop)`.
pub fn stop(rig: &mut Rig) -> Result<()> {
    rig.engine.send(EngineCommand::Stop)
}

/// Moves the playhead. Works while stopped or rolling.
///
/// Made of: `LiveEngine::send(Seek)`.
pub fn seek(rig: &mut Rig, to: TimePosition) -> Result<()> {
    rig.engine.send(EngineCommand::Seek(to))
}

/// Moves the playhead to the start of the song.
///
/// Made of: [`seek`] to zero.
pub fn rewind(rig: &mut Rig) -> Result<()> {
    seek(rig, TimePosition::ZERO)
}

/// Turns looping over the song's loop range on or off (set the range with
/// [`project::set_loop`](crate::project::set_loop)).
///
/// Made of: `LiveEngine::send(SetLoopEnabled)`.
pub fn set_loop_enabled(rig: &mut Rig, on: bool) -> Result<()> {
    rig.engine.send(EngineCommand::SetLoopEnabled(on))
}

/// The playhead after the last block the engine rendered.
pub fn position(rig: &Rig) -> TimePosition {
    rig.engine.position()
}

/// The transport state after the last block the engine rendered.
pub fn state(rig: &Rig) -> TransportState {
    rig.engine.state()
}

/// `true` if the engine was playing or recording at its last block.
pub fn is_rolling(rig: &Rig) -> bool {
    rig.engine.is_rolling()
}

/// Renders `blocks` blocks by driving the engine loop directly and returns
/// the audio. This is how a program without an audio device (a test, a
/// headless renderer) hears the rig; once the loop has been moved into a
/// device stream it fails with a message saying so.
///
/// Made of: [`sync`] when needed → `EngineLoop::tick` per block.
pub fn advance(rig: &mut Rig, blocks: usize) -> Result<PlanarBuffer> {
    if rig.needs_sync {
        sync(rig)?;
    }
    let block = rig.engine.max_frames();
    let engine_loop = rig.engine_loop.as_mut().ok_or_else(|| {
        SuperbError::Message("the engine loop is running on a device stream".into())
    })?;
    let mut out = PlanarBuffer::new(rig.channels, blocks * block);
    let mut scratch = PlanarBuffer::new(rig.channels, block);
    for i in 0..blocks {
        engine_loop.tick(&mut scratch)?;
        out.copy_block_from(i * block, &scratch, block)?;
    }
    rig.engine.reclaim();
    Ok(out)
}

/// Moves the engine loop into a real output stream and starts it. From then
/// on the device drives the engine and [`advance`] is unavailable.
///
/// Made of: [`sync`] when needed → `EngineLoop::run_on_device`; the stream
/// is kept in [`Rig::output`] and stops when dropped.
#[cfg(feature = "devices")]
pub fn start_output(
    rig: &mut Rig,
    config: &crate::primitives::io::device::OutputConfig,
) -> Result<()> {
    if rig.needs_sync {
        sync(rig)?;
    }
    let engine_loop = rig.engine_loop.take().ok_or_else(|| {
        SuperbError::Message("the engine loop is already running on a device stream".into())
    })?;
    match engine_loop.run_on_device(config) {
        Ok(stream) => {
            rig.output = Some(stream);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// The steps [`sync`] is made of, for building your own.
pub mod steps {
    use crate::play::{MixerBinding, SessionPlayer};
    use crate::primitives::graph::nodes::MonoSynthNode;
    use crate::primitives::graph::MidiPort;
    use crate::primitives::io::live::LiveEngine;
    use crate::primitives::mixer::{Mixer, StripId};
    use crate::primitives::plugins::PluginNode;
    use crate::primitives::session::{Session, TrackKind};
    use crate::primitives::{InsertId, Result};
    use crate::rig::{LiveInsert, PluginLoader, SendHandles, StripHandles};

    use super::AudioCache;

    /// Builds a fresh console for the song: a strip per track and bus,
    /// routing and sends, a player per audible clip, and a live instrument
    /// on every armed MIDI track so it sounds when you play.
    ///
    /// Made of: `Mixer::new` → `SessionPlayer::wire` → one `MonoSynthNode`
    /// plus a MIDI edge per armed MIDI track.
    pub fn build_console(
        session: &Session,
        cache: &AudioCache,
        channels: usize,
    ) -> Result<(Mixer, SessionPlayer)> {
        let mut mixer = Mixer::new(channels);
        let player = SessionPlayer::wire(session, &mut mixer, cache)?;
        for track in session.tracks() {
            if track.kind != TrackKind::Midi || !track.armed {
                continue;
            }
            let Some(MixerBinding::Track(strip)) = player.bridge().binding(track.id()) else {
                continue;
            };
            let synth = mixer
                .graph_mut()
                .add_node(Box::new(MonoSynthNode::new(channels)));
            mixer.connect_source(synth, strip)?;
            mixer.graph_mut().connect_midi(MidiPort::External, synth)?;
        }
        Ok((mixer, player))
    }

    /// Loads every non-bypassed insert in the song and splices it into its
    /// strip's chain. Failures are collected, not fatal.
    ///
    /// Made of: `PluginLoader::instantiate` → `PluginNode::new` →
    /// `Mixer::add_insert`, per insert.
    pub fn attach_inserts(
        session: &Session,
        loader: &mut PluginLoader,
        mixer: &mut Mixer,
        player: &SessionPlayer,
    ) -> (Vec<LiveInsert>, Vec<(InsertId, String)>) {
        let mut live = Vec::new();
        let mut failed = Vec::new();
        for track in session.tracks() {
            let strip: StripId = match player.bridge().binding(track.id()) {
                Some(MixerBinding::Track(id)) => id.into(),
                Some(MixerBinding::Bus(id)) => id.into(),
                None => continue,
            };
            for insert in track.inserts() {
                if insert.bypassed {
                    continue;
                }
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
                let sender = node
                    .take_param_sender()
                    .expect("a fresh node has its sender");
                match mixer.add_insert(strip, Box::new(node)) {
                    Ok(_) => live.push(LiveInsert {
                        id: insert.id(),
                        descriptor,
                        params,
                        sender,
                    }),
                    Err(e) => failed.push((insert.id(), e.to_string())),
                }
            }
        }
        (live, failed)
    }

    /// Collects the atomics the control side keeps after the mixer's graph
    /// has been handed to the engine: one strip handle per session track or
    /// bus, one send handle per session send.
    pub fn collect_handles(mixer: &Mixer, player: &SessionPlayer) -> (StripHandles, SendHandles) {
        let strips = player
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
        let sends = player
            .bridge()
            .send_entries()
            .iter()
            .filter_map(|(session_send, mixer_send)| {
                Some((*session_send, mixer.send_params(*mixer_send).ok()?))
            })
            .collect();
        (strips, sends)
    }

    /// Hands the console's graph to the engine, which swaps it in at its
    /// next block. The mixer's bookkeeping is consumed; keep the handles
    /// from [`collect_handles`] first.
    ///
    /// Made of: `Mixer::into_graph` → `LiveEngine::publish`.
    pub fn publish(engine: &mut LiveEngine, mixer: Mixer) -> Result<()> {
        engine.publish(mixer.into_graph())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::TrackKind;
    use crate::primitives::transport::LoopRange;
    use crate::primitives::{SampleRate, TimeDuration};
    use crate::project;

    const R: SampleRate = SampleRate::HZ_48K;

    /// A song with one audio track playing a cached 0.5 tone from 0..400 frames.
    fn tone_rig() -> (Rig, crate::primitives::TrackId) {
        let mut rig = project::create("tone", R, 64).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        rig.cache.insert("tone.wav", tone);
        project::add_audio_clip(
            &mut rig,
            vox,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_samples(400),
        )
        .unwrap();
        (rig, vox)
    }

    #[test]
    fn start_syncs_and_the_clip_is_heard_at_its_place() {
        let (mut rig, vox) = tone_rig();
        assert!(rig.needs_sync);
        start(&mut rig).unwrap();
        assert!(!rig.needs_sync);
        assert!(rig.live.strip(vox).is_some());
        assert!(rig.live.player.is_some());
        let audio = advance(&mut rig, 10).unwrap(); // 640 frames
        assert_eq!(audio.frames(), 640);
        // Constant-power pan at centre: 0.5 * ~0.707 per side once the ramp settled.
        let level = audio.channel(0)[300].abs();
        assert!(level > 0.3 && level < 0.4, "level {level}");
        assert!(
            audio.channel(0)[500..].iter().all(|&s| s.abs() < 1e-6),
            "silent after the clip"
        );
        assert_eq!(position(&rig), TimePosition::from_samples(640));
        assert!(is_rolling(&rig));
        assert_eq!(state(&rig), TransportState::Playing);
    }

    #[test]
    fn stop_seek_rewind_and_loop() {
        let (mut rig, _) = tone_rig();
        seek(&mut rig, TimePosition::from_samples(1_000)).unwrap();
        advance(&mut rig, 1).unwrap();
        assert_eq!(
            position(&rig),
            TimePosition::from_samples(1_000),
            "stopped: no motion"
        );
        start(&mut rig).unwrap();
        advance(&mut rig, 2).unwrap();
        assert_eq!(position(&rig), TimePosition::from_samples(1_128));
        stop(&mut rig).unwrap();
        advance(&mut rig, 1).unwrap();
        assert_eq!(state(&rig), TransportState::Stopped);
        rewind(&mut rig).unwrap();
        advance(&mut rig, 1).unwrap();
        assert_eq!(position(&rig), TimePosition::ZERO);

        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(100)).unwrap();
        project::set_loop(&mut rig, Some(range)).unwrap();
        set_loop_enabled(&mut rig, true).unwrap();
        start(&mut rig).unwrap();
        advance(&mut rig, 3).unwrap(); // 192 frames through a 100-frame loop
        assert_eq!(position(&rig), TimePosition::from_samples(92));
    }

    #[test]
    fn a_structural_edit_is_heard_after_the_next_sync() {
        let (mut rig, _) = tone_rig();
        start(&mut rig).unwrap();
        let before = advance(&mut rig, 2).unwrap();
        assert!(before.peak() > 0.3);
        // Mute the track in the document: parameter-only, mirrored on sync.
        let vox = rig.session.tracks()[0].id();
        rig.history
            .push(
                &mut rig.session,
                crate::primitives::session::commands::set_track_mute(vox, true),
            )
            .unwrap();
        rewind(&mut rig).unwrap();
        sync(&mut rig).unwrap();
        let after = advance(&mut rig, 3).unwrap();
        assert!(after.channel(0)[150..].iter().all(|&s| s.abs() < 1e-6));
        // Removing the track marks a sync; `advance` runs it.
        project::remove_track(&mut rig, vox).unwrap();
        assert!(rig.needs_sync);
        rewind(&mut rig).unwrap();
        let gone = advance(&mut rig, 2).unwrap();
        assert!(gone.is_silent());
        assert!(!rig.needs_sync);
        assert!(rig.live.strip(vox).is_none());
    }

    #[test]
    fn armed_midi_tracks_get_a_live_instrument() {
        let mut rig = project::create("keys", R, 64).unwrap();
        let keys = project::add_track(&mut rig, "keys", TrackKind::Midi).unwrap();
        start(&mut rig).unwrap();
        let (mixer, player) = steps::build_console(&rig.session, &rig.cache, 2).unwrap();
        assert_eq!(
            mixer.graph().node_count(),
            4,
            "master pair + track pair, no instrument"
        );
        drop(player);
        rig.session.track_mut(keys).unwrap().armed = true;
        let (mixer, _) = steps::build_console(&rig.session, &rig.cache, 2).unwrap();
        assert_eq!(mixer.graph().node_count(), 5, "plus the live synth");
        assert_eq!(mixer.graph().midi_edges().len(), 1);
    }

    #[test]
    fn advance_fails_once_the_loop_has_left() {
        let (mut rig, _) = tone_rig();
        rig.engine_loop = None;
        assert!(matches!(advance(&mut rig, 1), Err(SuperbError::Message(_))));
    }
}
