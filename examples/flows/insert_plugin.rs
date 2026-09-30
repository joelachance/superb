//! `plugins`: find CLAP plugins, put one on a track, turn a knob.
//!
//! What you'll learn:
//! - `plugins::scan` looks in the standard folders and `CLAP_PATH`
//! - An insert is stored in the song; the running instance is created at
//!   the next sync, so a song with a missing plugin still opens and plays
//! - `set_param` reaches the document and the running instance at once
//! - `failed` tells you which inserts did not load and why
//!
//! ```text
//! cargo run --example insert_plugin
//! CLAP_PATH=/path/to/plugins cargo run --example insert_plugin
//! ```

use superb::prelude::*;
use superb::primitives::plugins::PluginFormat;
use superb::{play, plugins, project};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut rig = project::create("fx", rate, 256)?;
    let vox = project::add_track(&mut rig, "vox", TrackKind::Audio)?;
    let mut tone = PlanarBuffer::new(2, rate.hz() as usize);
    tone.fill(0.25);
    rig.cache.insert("tone.wav", tone);
    project::add_audio_clip(
        &mut rig,
        vox,
        "tone.wav",
        TimePosition::ZERO,
        TimeDuration::from_seconds(1.0, rate),
    )?;

    // What is installed? Failures to load a bundle are listed, not fatal.
    let report = plugins::scan()?;
    println!(
        "{} plugin(s) found, {} bundle(s) failed to load",
        report.plugins.len(),
        report.failures.len()
    );
    for p in report.plugins.iter().take(8) {
        println!("  {} — {} [{}]", p.name, p.vendor, p.features.join(", "));
    }

    // Pick the first audio effect. Instruments make sound on their own and
    // would not show the "knob on a tone" idea as clearly.
    let effect = report
        .plugins
        .iter()
        .find(|p| p.features.iter().any(|f| f == "audio-effect"))
        .cloned();

    match effect {
        Some(desc) => {
            let fx = plugins::insert(&mut rig, vox, &desc)?;
            play::start(&mut rig)?; // syncs: the instance is created here
            match plugins::loaded(&rig, fx) {
                Some(d) => println!("loaded {} v{}", d.name, d.version),
                None => println!("did not load: {:?}", plugins::failed(&rig)),
            }
            let params = plugins::params(&rig, fx).to_vec();
            for p in params.iter().take(6) {
                println!(
                    "  param {:>4} {:<24} {:.3} in [{:.3}, {:.3}]",
                    p.id, p.name, p.default, p.min, p.max
                );
            }
            let dry = play::advance(&mut rig, 4)?.peak();
            // Turn the first automatable knob to its maximum and listen.
            if let Some(p) = params.iter().find(|p| p.automatable) {
                plugins::set_param(&mut rig, fx, p.id, p.max)?;
                let wet = play::advance(&mut rig, 4)?.peak();
                println!("{} at max: peak {dry:.3} -> {wet:.3}", p.name);
                println!(
                    "stored in the song: {:?}",
                    rig.session.insert(fx)?.param(p.id)
                );
            }
            // Bypass leaves the plugin out of the console entirely.
            plugins::set_bypassed(&mut rig, fx, true)?;
            play::sync(&mut rig)?;
            println!("bypassed: loaded = {}", plugins::loaded(&rig, fx).is_some());
        }
        None => {
            // No effect installed: show the failure path instead. The song
            // remembers the insert; the console runs without it.
            let mut desc =
                PluginDescriptor::new(PluginFormat::Clap, "com.example.missing", "Missing FX");
            desc.path = "/nowhere/missing.clap".into();
            let fx = plugins::insert(&mut rig, vox, &desc)?;
            play::start(&mut rig)?;
            println!("no audio effect installed; inserted a missing one instead:");
            for (id, why) in plugins::failed(&rig) {
                println!("  {id} failed: {why}");
            }
            println!(
                "the track still plays: peak {:.3}",
                play::advance(&mut rig, 4)?.peak()
            );
            plugins::remove(&mut rig, fx)?;
        }
    }
    Ok(())
}
