//! `transport`: hear the session, move the playhead, loop a section.
//!
//! What you'll learn:
//! - `transport::start` rebuilds the mixer from the document and starts the transport
//! - Without an audio device, `session::steps::render_blocks` renders blocks and hands you
//!   the audio (tests and headless tools work this way)
//! - The playhead is read from atomics: `playhead`, `state`
//! - A loop range lives in the document (`session::set_loop`); `set_loop_enabled`
//!   turns it on and off while rolling
//! - With feature `devices`, `devices::output` moves the engine onto a
//!   real output stream and the device drives it from then on
//!
//! ```text
//! cargo run --example play_session
//! cargo run --example play_session --features devices   # also plays on speakers
//! ```

use superb::prelude::*;
use superb::{clip, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create("play", rate, 256)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;

    // Half a second of a plucked-sounding tone: a sine with a decay.
    let frames = rate.hz() as usize / 2;
    let mut pluck = PlanarBuffer::new(2, frames);
    for ch in 0..2 {
        for (i, s) in pluck.channel_mut(ch).iter_mut().enumerate() {
            let t = i as f32 / rate.hz() as f32;
            *s = 0.5 * (-6.0 * t).exp() * (t * 330.0 * core::f32::consts::TAU).sin();
        }
    }
    session.audio.insert("pluck.wav", pluck);
    // Three plucks, one per half second; each clip is as long as the audio.
    for n in 0..3 {
        clip::add(
            &mut session,
            vox,
            "pluck.wav",
            TimePosition::from_seconds(0.5 * n as f64, rate),
        )?;
    }

    // Start. Every structural edit above marked the mixer for a rebuild;
    // `start` notices and builds it first.
    transport::start(&mut session)?;

    // Render 0.25 s (47 blocks of 256) and look at what came out. Commands
    // reach the engine at its next block, so state is read after driving.
    let audio = session::steps::render_blocks(&mut session, 47)?;
    println!("state: {:?}", transport::state(&session));
    println!(
        "rendered {} frames, peak {:.3}, playhead now {:.3} s",
        audio.frames(),
        audio.peak(),
        transport::playhead(&session).to_seconds(rate)
    );

    // Jump around. Seeks take effect at the next block.
    transport::seek(&mut session, TimePosition::from_seconds(1.0, rate))?;
    session::steps::render_blocks(&mut session, 1)?;
    println!(
        "after seek: {:.3} s",
        transport::playhead(&session).to_seconds(rate)
    );

    // Loop the middle pluck. The range is part of the document (undoable,
    // saved); enabling is a transport switch.
    let range = LoopRange::new(
        TimePosition::from_seconds(0.5, rate),
        TimePosition::from_seconds(1.0, rate),
    )?;
    session::set_loop(&mut session, Some(range))?;
    transport::set_loop_enabled(&mut session, true)?;
    transport::seek(&mut session, range.start())?;
    // 0.75 s of playback inside a 0.5 s loop: the playhead wraps once.
    session::steps::render_blocks(&mut session, 141)?;
    println!(
        "looping: {:.3} s (inside 0.5..1.0)",
        transport::playhead(&session).to_seconds(rate)
    );

    transport::stop(&mut session)?;
    session::steps::render_blocks(&mut session, 1)?; // the Stop command is applied at the next block
    println!("state: {:?}", transport::state(&session));
    transport::rewind(&mut session)?;
    session::steps::render_blocks(&mut session, 1)?;
    println!(
        "rewound to {:.3} s",
        transport::playhead(&session).to_seconds(rate)
    );

    // On a real device the same session plays through the speakers. From here
    // the device pulls blocks, so `render_blocks` is no longer available.
    #[cfg(feature = "devices")]
    {
        use superb::devices;
        transport::set_loop_enabled(&mut session, false)?;
        devices::output(&mut session, None)?; // the default output, matched to the session
        transport::start(&mut session)?;
        std::thread::sleep(std::time::Duration::from_millis(1600));
        println!(
            "device playback: {:.3} s, load {:.1}%",
            transport::playhead(&session).to_seconds(rate),
            session.engine.load().load() * 100.0
        );
        transport::stop(&mut session)?;
    }
    Ok(())
}
