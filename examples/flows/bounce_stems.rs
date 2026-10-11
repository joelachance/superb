//! `session::Layout`, `session::Root` and `export::stems`: where files go,
//! and every strip bounced to its own file in one pass.
//!
//! What you'll learn:
//! - Sessions live side by side under a root, `~/aura` by default; an
//!   application that keeps them elsewhere names its own root
//! - Every session folder has the same shape, and `session::layout` names
//!   each part of it, so nothing hard-codes a path
//! - A stem set is one file per strip plus the mix, all starting at the
//!   same sample and running the same length, with a manifest that says
//!   what each file is and how loud
//!
//! ```text
//! cargo run --example bounce_stems
//! ```

use aura::export::{self, Bounce, StemSource, Stems};
use aura::prelude::*;
use aura::session::{Layout, Root};
use aura::{clip, session, track};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;

    // Where sessions live. The default is `aura` in your home folder; this
    // example keeps its session in the temp folder instead, the way an app
    // would honour a folder the user picked.
    println!(
        "sessions live in {} by default",
        Root::default().path().display()
    );
    let root = Root::new(std::env::temp_dir().join("aura-example"));
    let folder = root.folder("Bounce Stems");
    let _ = std::fs::remove_dir_all(&folder); // a fresh run every time
    let mut session = session::create(&folder, rate, 256)?;
    println!(
        "sessions under {}: {:?}",
        root.path().display(),
        root.sessions()?
    );

    // Every part of the session has its place.
    let layout: Layout = session::layout(&session);
    println!("memory  {}", layout.memory().display());
    println!(
        "state   {} (written once session::state_file names it)",
        layout.state().display()
    );
    println!("audio   {}", layout.audio().display());
    println!("bounces {}", layout.bounces().display());
    println!("stems   {}", layout.stems().display());

    // A small show: two voices to the master, music into a bus.
    let host = track::add(&mut session, "host", TrackKind::Audio)?;
    let guest = track::add(&mut session, "guest", TrackKind::Audio)?;
    let music = track::add(&mut session, "music", TrackKind::Audio)?;
    let beds = track::add(&mut session, "beds", TrackKind::Bus)?;
    track::route(&mut session, music, Some(beds))?;
    for (name, hz, level) in [
        ("host", 180.0, 0.4),
        ("guest", 240.0, 0.3),
        ("music", 440.0, 0.2),
    ] {
        let frames = rate.hz() as usize; // a second each
        let mut tone = PlanarBuffer::new(1, frames);
        for (i, s) in tone.channel_mut(0).iter_mut().enumerate() {
            *s = level * (core::f32::consts::TAU * hz * i as f32 / rate.hz() as f32).sin();
        }
        session.audio.insert(format!("{name}.wav"), tone);
    }
    clip::add(&mut session, host, "host.wav", TimePosition::ZERO)?;
    clip::add(
        &mut session,
        guest,
        "guest.wav",
        TimePosition::from_seconds(0.5, rate),
    )?;
    clip::add(&mut session, music, "music.wav", TimePosition::ZERO)?;

    // A mix into bounces/, and a stem set into stems/final/.
    export::bounce(&mut session, layout.bounce("rough mix"), Bounce::default())?;
    let set = export::stems(&mut session, "final", Stems::default())?;
    println!(
        "\nstem set {:?}: {} Hz, {}-bit, {} frames from sample {}",
        set.name, set.sample_rate, set.bits, set.frames, set.start
    );
    for stem in set.stems.iter().chain(std::iter::once(&set.mix)) {
        // Where the strip goes: a bus, the master, or (the mix) out.
        let to = match (stem.source, stem.output.as_deref()) {
            (StemSource::Mix, _) => "out",
            (_, Some(bus)) => bus,
            (_, None) => "master",
        };
        println!(
            "  {:<10} {:<5} to {:<7} peak {:>6.1} dBFS, true peak {:>6.1} dBTP, {}",
            stem.file,
            format!("{:?}", stem.source),
            to,
            stem.peak_dbfs.unwrap_or(f32::NEG_INFINITY),
            stem.true_peak_dbtp.unwrap_or(f32::NEG_INFINITY),
            stem.loudness_lufs
                .map_or("too short for LUFS".to_string(), |l| format!("{l:.1} LUFS"))
        );
    }
    println!(
        "manifest {}",
        layout.stem_set("final").join("manifest.json").display()
    );
    Ok(())
}
