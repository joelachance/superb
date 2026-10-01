//! `play`: hear the session, move the playhead, loop a section.
//!
//! What you'll learn:
//! - `play::start` rebuilds the console from the session and starts the transport
//! - Without an audio device, `play::drive` renders blocks and hands you
//!   the audio (tests and headless tools work this way)
//! - The playhead is read from atomics: `position`, `state`, `is_rolling`
//! - A loop range lives in the session (`session::set_loop`); `set_loop_enabled`
//!   turns it on and off while rolling
//! - With feature `devices`, `play::start_output` moves the engine onto a
//!   real output stream and the device drives it from then on
//!
//! ```text
//! cargo run --example play_session
//! cargo run --example play_session --features devices   # also plays on speakers
//! ```

use superb::prelude::*;
use superb::{play, session};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut rig = session::create("play", rate, 256)?;
    let vox = session::add_track(&mut rig, "vox", TrackKind::Audio)?;

    // Half a second of a plucked-sounding tone: a sine with a decay.
    let frames = rate.hz() as usize / 2;
    let mut pluck = PlanarBuffer::new(2, frames);
    for ch in 0..2 {
        for (i, s) in pluck.channel_mut(ch).iter_mut().enumerate() {
            let t = i as f32 / rate.hz() as f32;
            *s = 0.5 * (-6.0 * t).exp() * (t * 330.0 * core::f32::consts::TAU).sin();
        }
    }
    rig.cache.insert("pluck.wav", pluck);
    // Three plucks, one per half second.
    for n in 0..3 {
        session::add_audio_clip(
            &mut rig,
            vox,
            "pluck.wav",
            TimePosition::from_seconds(0.5 * n as f64, rate),
            TimeDuration::from_samples(frames as i64),
        )?;
    }

    // Start. `needs_rebuild` is set by every structural edit above; `start`
    // notices and builds the console first.
    println!("needs rebuild before start: {}", rig.needs_rebuild);
    play::start(&mut rig)?;

    // Render 0.25 s (47 blocks of 256) and look at what came out. Commands
    // reach the engine at its next block, so state is read after driving.
    let audio = play::drive(&mut rig, 47)?;
    println!(
        "state: {:?}, rolling: {}",
        play::state(&rig),
        play::is_rolling(&rig)
    );
    println!(
        "rendered {} frames, peak {:.3}, playhead now {:.3} s",
        audio.frames(),
        audio.peak(),
        play::position(&rig).to_seconds(rate)
    );

    // Jump around. Seeks take effect at the next block.
    play::seek(&mut rig, TimePosition::from_seconds(1.0, rate))?;
    play::drive(&mut rig, 1)?;
    println!("after seek: {:.3} s", play::position(&rig).to_seconds(rate));

    // Loop the middle pluck. The range is part of the session (undoable,
    // saved); enabling is a transport switch.
    let range = LoopRange::new(
        TimePosition::from_seconds(0.5, rate),
        TimePosition::from_seconds(1.0, rate),
    )?;
    session::set_loop(&mut rig, Some(range))?;
    play::set_loop_enabled(&mut rig, true)?;
    play::seek(&mut rig, range.start())?;
    // 0.75 s of playback inside a 0.5 s loop: the playhead wraps once.
    play::drive(&mut rig, 141)?;
    println!(
        "looping: {:.3} s (inside 0.5..1.0)",
        play::position(&rig).to_seconds(rate)
    );

    play::stop(&mut rig)?;
    play::drive(&mut rig, 1)?; // the Stop command is applied at the next block
    println!("state: {:?}", play::state(&rig));
    play::rewind(&mut rig)?;
    play::drive(&mut rig, 1)?;
    println!("rewound to {:.3} s", play::position(&rig).to_seconds(rate));

    // On a real device the same rig plays through the speakers. From here
    // the device pulls blocks, so `drive` is no longer available.
    #[cfg(feature = "devices")]
    {
        use superb::devices;
        play::set_loop_enabled(&mut rig, false)?;
        let speakers = devices::output_config(&rig, None);
        play::start_output(&mut rig, &speakers)?;
        play::start(&mut rig)?;
        std::thread::sleep(std::time::Duration::from_millis(1600));
        println!(
            "device playback: {:.3} s, load {:.1}%",
            play::position(&rig).to_seconds(rate),
            rig.engine.load().load() * 100.0
        );
        play::stop(&mut rig)?;
    }
    Ok(())
}
