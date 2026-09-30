//! Effects and instruments: find plugins, put one on a track, turn its knobs.
//!
//! An insert is remembered in the song (plugin, bypass, parameter values,
//! saved state) and turned into a running instance when the console is
//! built. Parameter changes take the two usual paths: into the document
//! through undo and onto the running instance through its wait-free queue.
//!
//! Made of `superb::primitives::plugins` (the host), the session's insert
//! commands, and [`Rig::plugins`] (which loads bundles and keeps them open).

use crate::primitives::plugins::{ParamChange, ParamInfo, PluginDescriptor};
use crate::primitives::session::commands::{self, AddInsert, RemoveInsert};
use crate::primitives::session::{Insert, PluginRef};
use crate::primitives::{InsertId, Result, TrackId};
use crate::rig::Rig;

/// Finds every CLAP plugin in the standard locations and `CLAP_PATH`.
/// Bundles that fail to load are reported in the result, not fatal.
///
/// Made of: `plugins::scan::scan_default`.
#[cfg(feature = "clap")]
pub fn scan() -> Result<crate::primitives::plugins::scan::ScanReport> {
    crate::primitives::plugins::scan::scan_default(crate::primitives::plugins::PluginFormat::Clap)
}

/// Adds `plugin` at the end of a track's (or bus's) insert chain. The
/// instance is created at the next sync; if it cannot be loaded, the sync
/// still succeeds and the failure is listed by [`failed`].
///
/// Made of: `Session::allocate_insert_id` → `Insert::with_id` → push
/// `AddInsert`; marks the rig for a sync.
pub fn insert(rig: &mut Rig, track: TrackId, plugin: &PluginDescriptor) -> Result<InsertId> {
    let id = rig.session.allocate_insert_id();
    let insert = Insert::with_id(id, PluginRef::from(plugin));
    rig.history
        .push(&mut rig.session, AddInsert::new(track, insert))?;
    rig.needs_sync = true;
    Ok(id)
}

/// Removes an insert from its chain.
///
/// Made of: push `RemoveInsert`; marks the rig for a sync.
pub fn remove(rig: &mut Rig, insert: InsertId) -> Result<()> {
    rig.history
        .push(&mut rig.session, RemoveInsert::new(insert))?;
    rig.needs_sync = true;
    Ok(())
}

/// Bypasses an insert (its input passes straight through) or brings it
/// back. A bypassed insert is left out of the console entirely.
///
/// Made of: push `set_insert_bypassed` (its own undo step); marks the rig
/// for a sync.
pub fn set_bypassed(rig: &mut Rig, insert: InsertId, on: bool) -> Result<()> {
    crate::project::switch(rig, commands::set_insert_bypassed(insert, on))?;
    rig.needs_sync = true;
    Ok(())
}

/// Sets a plugin parameter to a plain value in its `[min, max]` range.
/// Consecutive edits of the same parameter are one undo step.
///
/// Made of: push `set_insert_param` → `ParamChange` onto the running
/// instance's queue, when it is loaded.
pub fn set_param(rig: &mut Rig, insert: InsertId, param: u32, value: f64) -> Result<()> {
    rig.history.push(
        &mut rig.session,
        commands::set_insert_param(insert, param, value),
    )?;
    if let Some(live) = rig.live.insert_mut(insert) {
        let _ = live.sender.push(ParamChange { id: param, value });
    }
    Ok(())
}

/// The running instance's parameters, in the plugin's order. Empty before
/// the first sync or if the plugin failed to load.
pub fn params(rig: &Rig, insert: InsertId) -> &[ParamInfo] {
    rig.live
        .insert(insert)
        .map(|i| i.params.as_slice())
        .unwrap_or(&[])
}

/// What the running instance says about itself, once loaded.
pub fn loaded(rig: &Rig, insert: InsertId) -> Option<&PluginDescriptor> {
    rig.live.insert(insert).map(|i| &i.descriptor)
}

/// Inserts that did not load at the last sync, with the reason.
pub fn failed(rig: &Rig) -> &[(InsertId, String)] {
    &rig.live.failed_inserts
}

#[cfg(all(test, feature = "clap"))]
mod tests {
    use super::*;
    use crate::play;
    use crate::primitives::buffers::PlanarBuffer;
    use crate::primitives::plugins::clap::{test_plugin, ClapBundle};
    use crate::primitives::plugins::PluginFormat;
    use crate::primitives::session::TrackKind;
    use crate::primitives::{SampleRate, TimeDuration, TimePosition};
    use crate::project;

    const R: SampleRate = SampleRate::HZ_48K;
    const VIRTUAL: &str = "/virtual/test.clap";

    /// The in-process test plugin, presented as if it lived at `VIRTUAL`.
    fn test_descriptor(rig: &mut Rig) -> PluginDescriptor {
        let bundle = ClapBundle::from_entry(&test_plugin::TEST_ENTRY, VIRTUAL).unwrap();
        let desc = bundle.descriptors().remove(0);
        rig.plugins.register(VIRTUAL, bundle);
        desc
    }

    fn tone_rig() -> (Rig, TrackId) {
        let mut rig = project::create("fx", R, 64).unwrap();
        let vox = project::add_track(&mut rig, "vox", TrackKind::Audio).unwrap();
        let mut tone = PlanarBuffer::new(2, 4_000);
        tone.fill(0.5);
        rig.cache.insert("tone.wav", tone);
        project::add_audio_clip(
            &mut rig,
            vox,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_samples(4_000),
        )
        .unwrap();
        (rig, vox)
    }

    fn settled(rig: &mut Rig) -> f32 {
        let audio = play::advance(rig, 4).unwrap();
        audio.channel(0)[audio.frames() - 1].abs()
    }

    #[test]
    fn an_insert_is_heard_and_its_knobs_turn() {
        let (mut rig, vox) = tone_rig();
        play::start(&mut rig).unwrap();
        let dry = settled(&mut rig);
        let desc = test_descriptor(&mut rig);
        let gain = insert(&mut rig, vox, &desc).unwrap();
        assert!(rig.needs_sync);
        assert!(params(&rig, gain).is_empty(), "not loaded until the sync");
        play::sync(&mut rig).unwrap();
        assert!(failed(&rig).is_empty(), "{:?}", failed(&rig));
        assert_eq!(loaded(&rig, gain).unwrap().name, "Test Gain");
        assert_eq!(params(&rig, gain)[0].name, "Gain");
        assert_eq!(rig.plugins.loaded(), 1);
        // Unity gain: unchanged (the test plugin also reports 16 frames of
        // latency, which the graph compensates).
        assert!((settled(&mut rig) - dry).abs() < 1e-3);

        set_param(&mut rig, gain, 7, 0.5).unwrap();
        set_param(&mut rig, gain, 7, 0.25).unwrap(); // a knob drag: one step
        assert!((settled(&mut rig) - dry * 0.25).abs() < 1e-3, "heard live");
        assert_eq!(rig.session.insert(gain).unwrap().param(7), Some(0.25));
        // A fresh sync re-applies the stored value to the new instance.
        play::sync(&mut rig).unwrap();
        assert!((settled(&mut rig) - dry * 0.25).abs() < 1e-3);

        set_bypassed(&mut rig, gain, true).unwrap();
        play::sync(&mut rig).unwrap();
        assert!(
            loaded(&rig, gain).is_none(),
            "bypassed inserts are not built"
        );
        assert!((settled(&mut rig) - dry).abs() < 1e-3);
        set_bypassed(&mut rig, gain, false).unwrap();
        remove(&mut rig, gain).unwrap();
        assert!(rig.session.insert(gain).is_err());
        play::sync(&mut rig).unwrap();
        assert!((settled(&mut rig) - dry).abs() < 1e-3);
        // Undo the removal and the bypass toggles: the insert is back, active, at 0.25.
        project::undo(&mut rig).unwrap();
        project::undo(&mut rig).unwrap();
        project::undo(&mut rig).unwrap();
        play::sync(&mut rig).unwrap();
        assert!((settled(&mut rig) - dry * 0.25).abs() < 1e-3);
    }

    #[test]
    fn a_missing_plugin_is_reported_and_the_rest_plays() {
        let (mut rig, vox) = tone_rig();
        let mut desc = PluginDescriptor::new(PluginFormat::Clap, "com.example.gone", "Gone");
        desc.path = "/nonexistent/gone.clap".into();
        let gone = insert(&mut rig, vox, &desc).unwrap();
        play::start(&mut rig).unwrap();
        assert_eq!(failed(&rig).len(), 1);
        assert_eq!(failed(&rig)[0].0, gone);
        assert!(settled(&mut rig) > 0.3, "the track still plays");
        let mut vst = PluginDescriptor::new(PluginFormat::Vst3, "x", "X");
        vst.path = "/x.vst3".into();
        let vst = insert(&mut rig, vox, &vst).unwrap();
        play::sync(&mut rig).unwrap();
        assert!(failed(&rig)
            .iter()
            .any(|(id, why)| *id == vst && why.contains("VST3")));
        // Setting a parameter on an unloaded insert still records in the document.
        set_param(&mut rig, gone, 1, 0.5).unwrap();
        assert_eq!(rig.session.insert(gone).unwrap().param(1), Some(0.5));
    }
}
