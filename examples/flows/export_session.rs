//! `export`: bounce the document to a WAV file.
//!
//! What you'll learn:
//! - Export renders offline through its own copy of the console, inserts
//!   included, without touching the live engine
//! - `bounce_session` covers start to the end of the last clip; `bounce` takes
//!   a range; `render` gives you the audio without a file
//! - With feature `files`, `bounce_at_rate` converts to another sample rate
//!
//! ```text
//! cargo run --example export_session
//! cargo run --example export_session --features files
//! ```

use superb::prelude::*;
use superb::primitives::io::wav::{self, WavFormat};
use superb::{export, mix, play, session};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut rig = session::create("bounce", rate, 256)?;
    let lead = session::add_track(&mut rig, "lead", TrackKind::Audio)?;
    let pad = session::add_track(&mut rig, "pad", TrackKind::Audio)?;

    // Two one-second tones, the second starting at 0.5 s: the document ends at 1.5 s.
    let mut a = PlanarBuffer::new(2, rate.hz() as usize);
    let mut b = PlanarBuffer::new(2, rate.hz() as usize);
    for i in 0..a.frames() {
        let t = i as f32 / rate.hz() as f32;
        let sa = 0.3 * (t * 440.0 * core::f32::consts::TAU).sin();
        let sb = 0.2 * (t * 110.0 * core::f32::consts::TAU).sin();
        for ch in 0..2 {
            a.channel_mut(ch)[i] = sa;
            b.channel_mut(ch)[i] = sb;
        }
    }
    rig.cache.insert("a.wav", a);
    rig.cache.insert("b.wav", b);
    let one = TimeDuration::from_seconds(1.0, rate);
    session::add_audio_clip(&mut rig, lead, "a.wav", TimePosition::ZERO, one)?;
    session::add_audio_clip(
        &mut rig,
        pad,
        "b.wav",
        TimePosition::from_seconds(0.5, rate),
        one,
    )?;
    mix::set_gain_db(&mut rig, pad, -6.0)?;
    mix::set_pan(&mut rig, lead, 0.3)?;

    // The live engine is untouched by an export: leave it stopped at zero.
    let dir = std::env::temp_dir();
    let path = dir.join("export_session.wav");
    let audio = export::bounce_session(&mut rig, &path, WavFormat::Pcm16)?;
    println!(
        "bounced {:.2} s ({} frames) to {}; peak {:.3}",
        audio.frames() as f64 / rate.hz() as f64,
        audio.frames(),
        path.display(),
        audio.peak()
    );
    println!(
        "live playhead still at {:.1} s",
        play::position(&rig).to_seconds(rate)
    );

    // Read it back to prove the file is what we rendered.
    let (read, read_rate) = wav::read(&path)?;
    println!(
        "read back: {} frames @ {} Hz, {} channels",
        read.frames(),
        read_rate.hz(),
        read.channels()
    );

    // Just the overlap, as 32-bit float, into memory and to a file.
    let overlap = export::render(
        &mut rig,
        TimePosition::from_seconds(0.5, rate),
        rate.hz() as usize / 2,
    )?;
    println!("0.5 s .. 1.0 s in memory: peak {:.3}", overlap.peak());
    export::bounce(
        &mut rig,
        dir.join("export_overlap.wav"),
        TimePosition::from_seconds(0.5, rate),
        rate.hz() as usize / 2,
        WavFormat::Float32,
    )?;

    #[cfg(feature = "files")]
    {
        let cd = dir.join("export_song_44k1.wav");
        let converted = export::bounce_at_rate(
            &mut rig,
            &cd,
            TimePosition::ZERO,
            audio.frames(),
            SampleRate::CD,
            WavFormat::Pcm16,
        )?;
        println!(
            "resampled to 44.1 kHz: {} frames -> {}",
            converted.frames(),
            cd.display()
        );
    }
    Ok(())
}
