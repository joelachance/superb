//! Effects and instruments: find plugins, put one on a track, turn its knobs.
//!
//! An insert is remembered in the document (plugin, bypass, parameter values,
//! saved state) and turned into a running instance when the console is
//! built. Parameter changes take the two usual paths: into the document
//! through undo and onto the running instance through its wait-free queue.
//!
//! Made of `superb::primitives::plugins` (the host), the document's insert
//! commands, and [`Session::plugins`] (which loads bundles and keeps them open).

use crate::primitives::plugins::{ParamChange, ParamInfo, PluginDescriptor};
use crate::primitives::session::commands::{self, AddInsert, RemoveInsert};
use crate::primitives::session::{Insert, PluginRef};
use crate::primitives::{InsertId, Result, TrackId};
use crate::session::Session;

/// Finds every CLAP plugin in the standard locations and `CLAP_PATH`.
/// Bundles that fail to load are reported in the result, not fatal.
///
/// Made of: `plugins::scan::scan_default`.
#[cfg(feature = "clap")]
pub fn scan() -> Result<crate::primitives::plugins::scan::ScanReport> {
    crate::primitives::plugins::scan::scan_default(crate::primitives::plugins::PluginFormat::Clap)
}

/// Adds `plugin` at the end of a track's (or bus's) insert chain. The
/// instance is created at the next rebuild; if it cannot be loaded, the rebuild
/// still succeeds and the failure is listed by [`failures`].
///
/// Made of: `Document::allocate_insert_id` → `Insert::with_id` → push
/// `AddInsert`; marks the session for a rebuild.
pub fn insert(
    session: &mut Session,
    track: TrackId,
    plugin: &PluginDescriptor,
) -> Result<InsertId> {
    let id = session.document.allocate_insert_id();
    let insert = Insert::with_id(id, PluginRef::from(plugin));
    session
        .history
        .push(&mut session.document, AddInsert::new(track, insert))?;
    session.needs_rebuild = true;
    Ok(id)
}

/// Removes an insert from its chain.
///
/// Made of: push `RemoveInsert`; marks the session for a rebuild.
pub fn remove(session: &mut Session, insert: InsertId) -> Result<()> {
    session
        .history
        .push(&mut session.document, RemoveInsert::new(insert))?;
    session.needs_rebuild = true;
    Ok(())
}

/// Bypasses an insert (its input passes straight through, the plugin is
/// not run) or brings it back. A switch, not an edit: it is saved with the
/// document but not recorded in the undo history, and it is heard at the
/// next block.
///
/// Made of: `InsertState::bypass` through `session::set_state` → the node's
/// bypass flag in [`LiveInsert::bypass`](crate::session::LiveInsert::bypass).
pub fn bypass(session: &mut Session, insert: InsertId, on: bool) -> Result<()> {
    crate::session::set_state(session, |s| {
        s.insert_mut(insert)?.state.bypass = on;
        Ok(())
    })?;
    if let Some(live) = session.live.insert(insert) {
        live.bypass.store(on, core::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// Sets a plugin parameter to a plain value in its `[min, max]` range.
/// Consecutive edits of the same parameter are one undo step.
///
/// Made of: push `set_insert_param` → `ParamChange` onto the running
/// instance's queue, when it is loaded.
pub fn param(session: &mut Session, insert: InsertId, param: u32, value: f64) -> Result<()> {
    session.history.push(
        &mut session.document,
        commands::set_insert_param(insert, param, value),
    )?;
    if let Some(live) = session.live.insert_mut(insert) {
        let _ = live.param_queue.push(ParamChange { id: param, value });
    }
    Ok(())
}

/// The running instance's parameters, in the plugin's order. Empty before
/// the first rebuild or if the plugin failed to load.
pub fn params(session: &Session, insert: InsertId) -> &[ParamInfo] {
    session
        .live
        .insert(insert)
        .map(|i| i.params.as_slice())
        .unwrap_or(&[])
}

/// What the running instance says about itself; `None` before the first
/// rebuild or if the plugin failed to load (see [`failures`]).
pub fn info(session: &Session, insert: InsertId) -> Option<&PluginDescriptor> {
    session.live.insert(insert).map(|i| &i.descriptor)
}

/// Inserts that did not load at the last rebuild, with the reason.
pub fn failures(session: &Session) -> &[(InsertId, String)] {
    &session.live.failed_inserts
}

#[cfg(all(test, feature = "clap"))]
mod tests {
    use super::*;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::plugins::clap::{test_plugin, ClapBundle};
    use crate::primitives::plugins::PluginFormat;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use crate::session;
    use crate::track;
    use crate::transport;

    const R: SampleRate = SampleRate::HZ_48K;
    const VIRTUAL: &str = "/virtual/test.clap";

    /// The in-process test plugin, presented as if it lived at `VIRTUAL`.
    fn test_descriptor(session: &mut Session) -> PluginDescriptor {
        let bundle = ClapBundle::from_entry(&test_plugin::TEST_ENTRY, VIRTUAL).unwrap();
        let desc = bundle.descriptors().remove(0);
        session.plugins.register(VIRTUAL, bundle);
        desc
    }

    fn tone_rig() -> (Session, TrackId) {
        let mut session = session::create("fx", R, 64).unwrap();
        let vox = track::add(&mut session, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 4_000);
        tone.fill(0.5);
        session.cache.insert("tone.wav", tone);
        session::add_audio_clip(
            &mut session,
            vox,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_samples(4_000),
        )
        .unwrap();
        (session, vox)
    }

    fn settled(session: &mut Session) -> f32 {
        let audio = session::steps::render_blocks(session, 4).unwrap();
        audio.channel(0)[audio.frames() - 1].abs()
    }

    #[test]
    fn an_insert_is_heard_and_its_knobs_turn() {
        let (mut session, vox) = tone_rig();
        transport::start(&mut session).unwrap();
        let dry = settled(&mut session);
        let desc = test_descriptor(&mut session);
        let gain = insert(&mut session, vox, &desc).unwrap();
        assert!(session.needs_rebuild);
        assert!(
            params(&session, gain).is_empty(),
            "not loaded until the rebuild"
        );
        session::steps::rebuild(&mut session).unwrap();
        assert!(failures(&session).is_empty(), "{:?}", failures(&session));
        assert_eq!(info(&session, gain).unwrap().name, "Test Gain");
        assert_eq!(params(&session, gain)[0].name, "Gain");
        assert_eq!(session.plugins.loaded(), 1);
        // Unity gain: unchanged (the test plugin also reports 16 frames of
        // latency, which the graph compensates).
        assert!((settled(&mut session) - dry).abs() < 1e-3);

        param(&mut session, gain, 7, 0.5).unwrap();
        param(&mut session, gain, 7, 0.25).unwrap(); // a knob drag: one step
        assert!(
            (settled(&mut session) - dry * 0.25).abs() < 1e-3,
            "heard live"
        );
        assert_eq!(session.document.insert(gain).unwrap().param(7), Some(0.25));
        // A fresh rebuild re-applies the stored value to the new instance.
        session::steps::rebuild(&mut session).unwrap();
        assert!((settled(&mut session) - dry * 0.25).abs() < 1e-3);

        // Bypass is a live switch: heard at once, no rebuild, not an undo step.
        let steps = session.history.undo_len();
        bypass(&mut session, gain, true).unwrap();
        assert!(!session.needs_rebuild);
        assert_eq!(session.history.undo_len(), steps);
        assert!(session.document.insert(gain).unwrap().state.bypass);
        assert!(
            (settled(&mut session) - dry).abs() < 1e-3,
            "input passes through"
        );
        assert!(info(&session, gain).is_some(), "the instance stays loaded");
        // A rebuilt console starts bypassed too, from the document.
        session::steps::rebuild(&mut session).unwrap();
        assert!((settled(&mut session) - dry).abs() < 1e-3);
        bypass(&mut session, gain, false).unwrap();
        assert!((settled(&mut session) - dry * 0.25).abs() < 1e-3);
        remove(&mut session, gain).unwrap();
        assert!(session.document.insert(gain).is_err());
        session::steps::rebuild(&mut session).unwrap();
        assert!((settled(&mut session) - dry).abs() < 1e-3);
        // Undo the removal: the insert is back, active, at 0.25.
        session::undo(&mut session).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        assert!((settled(&mut session) - dry * 0.25).abs() < 1e-3);
    }

    #[test]
    fn a_missing_plugin_is_reported_and_the_rest_plays() {
        let (mut session, vox) = tone_rig();
        let mut desc = PluginDescriptor::new(PluginFormat::Clap, "com.example.gone", "Gone");
        desc.path = "/nonexistent/gone.clap".into();
        let gone = insert(&mut session, vox, &desc).unwrap();
        transport::start(&mut session).unwrap();
        assert_eq!(failures(&session).len(), 1);
        assert_eq!(failures(&session)[0].0, gone);
        assert!(settled(&mut session) > 0.3, "the track still plays");
        let mut vst = PluginDescriptor::new(PluginFormat::Vst3, "x", "X");
        vst.path = "/x.vst3".into();
        let vst = insert(&mut session, vox, &vst).unwrap();
        session::steps::rebuild(&mut session).unwrap();
        assert!(failures(&session)
            .iter()
            .any(|(id, why)| *id == vst && why.contains("VST3")));
        // Setting a parameter on an unloaded insert still records in the document.
        param(&mut session, gone, 1, 0.5).unwrap();
        assert_eq!(session.document.insert(gone).unwrap().param(1), Some(0.5));
    }
}
