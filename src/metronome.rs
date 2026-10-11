//! The metronome: a click on every beat while the transport rolls, and the
//! count-in before a recording.
//!
//! The click is a node on the master bus, built with the mixer and driven
//! by the memory's metronome state: whether it is on and how loud, both
//! reaching the live node at once, and how many bars of count-in a
//! recording that starts at the playhead gets. During a count-in the click
//! sounds even when the metronome is off, and stops at the punch-in.
//!
//! Made of `aura::primitives::session::MetronomeState` and its command
//! (the memory side) and `aura::primitives::graph::nodes::ClickParams`
//! (the live side).

use crate::primitives::buffers::gain::db_to_linear;
use crate::primitives::session::commands;
use crate::primitives::Result;
use crate::session::Session;

/// Turns the click on or off. State, like mute: saved with the session,
/// heard at the next block, not an undo step.
///
/// Made of: `MetronomeState::enabled` through `session::set_state` →
/// `ClickParams::set_enabled`.
pub fn enable(session: &mut Session, on: bool) -> Result<()> {
    crate::session::set_state(session, |m| {
        m.metronome_mut().enabled = on;
        Ok(())
    })?;
    session.mixer.click.set_enabled(on);
    Ok(())
}

/// Sets the click level in dB. Moves within [`Session::COALESCE_WINDOW`]
/// are one undo step; heard at once.
///
/// Made of: `set_metronome_level_db` through `session::edit` →
/// `ClickParams::set_level`.
pub fn level(session: &mut Session, db: f32) -> Result<()> {
    crate::session::edit(session, commands::set_metronome_level_db(db))?;
    session.mixer.click.set_level(db_to_linear(db));
    Ok(())
}

/// Sets how many bars of count-in a recording gets when it starts at the
/// playhead: `record::start` rolls the transport that many bars early
/// with the click sounding, and drops into record at the playhead. `0`
/// for none. State, saved with the session.
///
/// Made of: `MetronomeState::count_in` through `session::set_state`.
pub fn count_in(session: &mut Session, bars: u32) -> Result<()> {
    crate::session::set_state(session, |m| {
        m.metronome_mut().count_in = bars;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::Memory;
    use crate::primitives::SampleRate;
    use crate::{session, transport};

    #[test]
    fn the_click_sounds_on_the_beat_while_rolling_and_follows_its_switch() {
        let mut session =
            session::create(session::scratch("click"), SampleRate::HZ_48K, 64).unwrap();
        // 600 bpm: a beat every 4 800 samples, so the second beat lands in
        // block 75, well after the first tick (50 ms) has died away.
        session::set_tempo(&mut session, 600.0).unwrap();
        let steps = session.history.undo_len();
        enable(&mut session, true).unwrap();
        assert!(session.memory.metronome().enabled);
        assert_eq!(session.history.undo_len(), steps, "state, not an edit");
        level(&mut session, -6.0206).unwrap();
        assert_eq!(session.history.undo_len(), steps + 1);
        count_in(&mut session, 2).unwrap();
        assert_eq!(session.memory.metronome().count_in, 2);

        // Stopped: silence. Rolling: a tick at beat 0, then at beat 1.
        assert!(session::steps::render_blocks(&mut session, 1)
            .unwrap()
            .is_silent());
        transport::start(&mut session).unwrap();
        let first = session::steps::render_blocks(&mut session, 1).unwrap();
        assert!(first.peak() > 0.3 && first.peak() < 0.6, "{}", first.peak());
        let between = session::steps::render_blocks(&mut session, 74).unwrap();
        assert!(between.channel(0)[between.frames() - 64..]
            .iter()
            .all(|&s| s.abs() < 1e-3));
        let second = session::steps::render_blocks(&mut session, 1).unwrap();
        assert!(second.peak() > 0.2, "{}", second.peak());
        assert!(second.peak() < first.peak(), "beat 1 is not accented");

        // Off: silent at once, no rebuild. The level is an edit and rides
        // memory.json; the switch and the count-in are state and do not.
        enable(&mut session, false).unwrap();
        assert!(session::steps::render_blocks(&mut session, 40)
            .unwrap()
            .is_silent());
        let copy = Memory::from_json(&session.memory.to_json().unwrap()).unwrap();
        assert!((copy.metronome().level_db + 6.0206).abs() < 1e-6);
        assert_eq!(
            copy.metronome().count_in,
            0,
            "state lives in the state file"
        );
        let state = session.memory.state_snapshot();
        assert_eq!(state.count_in, 2);
        assert!(!state.metronome);
    }
}
