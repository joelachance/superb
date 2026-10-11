//! `export`: bounce the session to a WAV file.
//!
//! What you'll learn:
//! - A bounce renders offline through its own copy of the mixer, without
//!   touching the live engine
//! - `Bounce::default()` covers the session from zero to the end of the
//!   last clip, at the session's rate, as 16-bit PCM; set `start`,
//!   `length`, `format` for a range or another file format
//! - With feature `files`, `sample_rate` converts the bounce on the way out
//!
//! ```text
//! cargo run --example export_session
//! cargo run --example export_session --features files
//! ```

use aura::export::{self, Bounce};
use aura::prelude::*;
use aura::primitives::io::wav::{self, WavFormat};
use aura::{clip, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create(std::env::temp_dir().join("export_session.aura"), rate, 256)?;
    let lead = track::add(&mut session, "lead", TrackKind::Audio)?;
    let pad = track::add(&mut session, "pad", TrackKind::Audio)?;

    // Two one-second tones, the second starting at 0.5 s: the session ends at 1.5 s.
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
    session.audio.insert("a.wav", a);
    session.audio.insert("b.wav", b);
    clip::add(&mut session, lead, "a.wav", TimePosition::ZERO)?;
    clip::add(
        &mut session,
        pad,
        "b.wav",
        TimePosition::from_seconds(0.5, rate),
    )?;
    track::volume(&mut session, pad, -6.0)?;
    track::pan(&mut session, lead, 0.3)?;

    // The whole session. The live engine is untouched: it stays stopped at zero.
    let dir = std::env::temp_dir();
    let path = dir.join("export_session.wav");
    let audio = export::bounce(&mut session, &path, Bounce::default())?;
    println!(
        "bounced {:.2} s ({} frames) to {}; peak {:.3}",
        audio.frames() as f64 / rate.hz() as f64,
        audio.frames(),
        path.display(),
        audio.peak()
    );
    println!(
        "live playhead still at {:.1} s",
        transport::playhead(&session).to_seconds(rate)
    );

    // Read it back to prove the file is what we rendered.
    let (read, read_rate) = wav::read(&path)?;
    println!(
        "read back: {} frames @ {} Hz, {} channels",
        read.frames(),
        read_rate.hz(),
        read.channels()
    );

    // Just the overlap (0.5 s .. 1.0 s), as 32-bit float.
    let overlap = export::bounce(
        &mut session,
        dir.join("export_overlap.wav"),
        Bounce {
            start: TimePosition::from_seconds(0.5, rate),
            length: Some(TimeDuration::from_seconds(0.5, rate)),
            format: WavFormat::Float32,
            ..Bounce::default()
        },
    )?;
    println!("0.5 s .. 1.0 s as float: peak {:.3}", overlap.peak());

    #[cfg(feature = "files")]
    {
        let cd = dir.join("export_session_44k1.wav");
        let converted = export::bounce(
            &mut session,
            &cd,
            Bounce {
                sample_rate: Some(SampleRate::CD),
                ..Bounce::default()
            },
        )?;
        println!(
            "resampled to 44.1 kHz: {} frames -> {}",
            converted.frames(),
            cd.display()
        );
    }
    Ok(())
}
