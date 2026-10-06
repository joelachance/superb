//! The transport bar: start, stop, seek, loop, and where the playhead is.
//!
//! Every function sends a command to the running engine, which applies it
//! at the start of its next block, or reads what the engine published after
//! its last block. Only [`looping`] touches the memory: whether playback
//! loops is saved with the session, like a track's mute.
//!
//! Made of `superb::primitives::io` (`Engine::live`) (`EngineHandle::send`, the
//! position and state atomics).

use crate::primitives::io::EngineCommand;
use crate::primitives::transport::TransportState;
use crate::primitives::{Result, TimePosition};
use crate::session::Session;

/// Starts playing from where the playhead is. Rebuilds the mixer first if
/// the memory changed since it was built.
///
/// Made of: [`session::steps::rebuild`](crate::session::steps::rebuild)
/// when needed → `EngineHandle::send(Play)`.
pub fn start(session: &mut Session) -> Result<()> {
    if session.needs_rebuild {
        crate::session::steps::rebuild(session)?;
    }
    session.engine.send(EngineCommand::Play)
}

/// Stops the transport where it is.
///
/// Made of: `EngineHandle::send(Stop)`.
pub fn stop(session: &mut Session) -> Result<()> {
    session.engine.send(EngineCommand::Stop)
}

/// Moves the playhead. Works while stopped or rolling.
///
/// Made of: `EngineHandle::send(Seek)`.
pub fn seek(session: &mut Session, to: TimePosition) -> Result<()> {
    session.engine.send(EngineCommand::Seek(to))
}

/// Moves the playhead to the start of the session.
///
/// Made of: [`seek`] to zero.
pub fn rewind(session: &mut Session) -> Result<()> {
    seek(session, TimePosition::ZERO)
}

/// Turns looping over the session's loop range on or off (set the range
/// with [`session::set_loop`](crate::session::set_loop)). State, like
/// mute: saved with the session, applied at the engine's next block, not
/// an undo step.
///
/// Made of: `Memory::set_looping` through `session::set_state` →
/// `EngineHandle::send(SetLoopEnabled)`.
pub fn looping(session: &mut Session, on: bool) -> Result<()> {
    crate::session::set_state(session, |m| {
        m.set_looping(on);
        Ok(())
    })?;
    session.engine.send(EngineCommand::SetLoopEnabled(on))
}

/// Where the playhead is: the position after the last block the engine
/// rendered.
pub fn playhead(session: &Session) -> TimePosition {
    session.engine.position()
}

/// The transport state after the last block the engine rendered.
pub fn state(session: &Session) -> TransportState {
    session.engine.state()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::session::TrackKind;
    use crate::primitives::transport::LoopRange;
    use crate::primitives::SampleRate;
    use crate::session;
    use crate::track;

    const R: SampleRate = SampleRate::HZ_48K;

    /// A session with one audio track playing a cached 0.5 tone from 0..400 frames.
    fn tone_rig() -> (Session, crate::primitives::TrackId) {
        let mut session = session::create(session::scratch("tone"), R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 400);
        tone.fill(0.5);
        session.audio.insert("tone.wav", tone);
        crate::clip::add(&mut session, vox, "tone.wav", TimePosition::ZERO).unwrap();
        (session, vox)
    }

    #[test]
    fn start_rebuilds_and_the_clip_is_heard_at_its_place() {
        let (mut session, vox) = tone_rig();
        assert!(session.needs_rebuild);
        start(&mut session).unwrap();
        assert!(!session.needs_rebuild);
        assert!(session.mixer.strip(vox).is_some());
        let audio = session::steps::render_blocks(&mut session, 10).unwrap(); // 640 frames
        assert_eq!(audio.frames(), 640);
        // Constant-power pan at centre: 0.5 * ~0.707 per side once the ramp settled.
        let level = audio.channel(0)[300].abs();
        assert!(level > 0.3 && level < 0.4, "level {level}");
        assert!(
            audio.channel(0)[500..].iter().all(|&s| s.abs() < 1e-6),
            "silent after the clip"
        );
        assert_eq!(playhead(&session), TimePosition::from_samples(640));
        assert_eq!(state(&session), TransportState::Playing);
    }

    #[test]
    fn stop_seek_rewind_and_loop() {
        let (mut session, _) = tone_rig();
        seek(&mut session, TimePosition::from_samples(1_000)).unwrap();
        session::steps::render_blocks(&mut session, 1).unwrap();
        assert_eq!(
            playhead(&session),
            TimePosition::from_samples(1_000),
            "stopped: no motion"
        );
        start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 2).unwrap();
        assert_eq!(playhead(&session), TimePosition::from_samples(1_128));
        stop(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 1).unwrap();
        assert_eq!(state(&session), TransportState::Stopped);
        rewind(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 1).unwrap();
        assert_eq!(playhead(&session), TimePosition::ZERO);

        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(100)).unwrap();
        session::set_loop(&mut session, Some(range)).unwrap();
        looping(&mut session, true).unwrap();
        start(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 3).unwrap(); // 192 frames through a 100-frame loop
        assert_eq!(playhead(&session), TimePosition::from_samples(92));
    }

    #[test]
    fn looping_is_saved_state_and_reaches_the_engine_on_open() {
        let (mut session, _) = tone_rig();
        let dir = session.folder.clone();
        let range = LoopRange::new(TimePosition::ZERO, TimePosition::from_samples(100)).unwrap();
        session::set_loop(&mut session, Some(range)).unwrap();
        let steps = session.history.undo_len();
        looping(&mut session, true).unwrap();
        assert!(session.memory.looping());
        assert_eq!(session.history.undo_len(), steps, "state, not an edit");
        assert!(!session.history.is_clean());
        session::save(&mut session).unwrap();

        // Reopened: the memory remembers, and the first start tells the engine.
        let mut again = session::open(&dir, 64).unwrap();
        assert!(again.memory.looping());
        assert_eq!(again.memory.loop_range(), Some(range));
        start(&mut again).unwrap();
        session::steps::render_blocks(&mut again, 3).unwrap(); // 192 frames through a 100-frame loop
        assert_eq!(playhead(&again), TimePosition::from_samples(92));

        // Back in the original session, undoing the range edit reaches the
        // engine at the next rebuild, which the next render runs.
        assert!(session::undo(&mut session).unwrap());
        assert_eq!(session.memory.loop_range(), None);
        start(&mut session).unwrap();
        rewind(&mut session).unwrap();
        session::steps::render_blocks(&mut session, 3).unwrap();
        assert_eq!(
            playhead(&session),
            TimePosition::from_samples(192),
            "no loop any more"
        );
        looping(&mut session, false).unwrap();
        assert!(!session.memory.looping());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
