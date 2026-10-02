//! `mix`: faders, pan, mute and solo, a reverb-style bus with sends, meters.
//!
//! What you'll learn:
//! - Level changes go two places at once: the document (undoable) and the live
//!   strip (heard on the next block, no rebuild)
//! - Buses collect tracks; `route` sends a track's output into one
//! - Sends tap a copy of a track into a bus, pre- or post-fader
//! - Peak meters are read (and reset) per strip
//! - A fader drag is one undo step: moves within half a second merge.
//!   Switches (mute, solo) are saved but never undo steps.
//!
//! ```text
//! cargo run --example mix_session
//! ```

use superb::prelude::*;
use superb::{mix, session, track, transport};

/// Renders four blocks and returns the peak of the last one, so ramps have
/// settled: what a meter would show.
fn level(session: &mut Session) -> Result<f32> {
    let audio = session::steps::render_blocks(session, 4)?;
    let last = audio.frames() - session.engine.max_frames();
    Ok(audio.channel(0)[last..]
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs())))
}

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let mut session = session::create("mix", rate, 128)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let gtr = track::add(&mut session, "gtr", TrackKind::Audio)?;

    // Two steady tones so levels are easy to read: 0.25 on each track.
    let mut tone = PlanarBuffer::new(2, rate.hz() as usize * 4);
    tone.fill(0.25);
    session.cache.insert("tone.wav", tone);
    for t in [vox, gtr] {
        session::add_audio_clip(
            &mut session,
            t,
            "tone.wav",
            TimePosition::ZERO,
            TimeDuration::from_seconds(4.0, rate),
        )?;
    }
    transport::start(&mut session)?;
    // Strips default to a -3 dB centre pan law; use the plain "balance" law
    // here so the arithmetic below is exact. This is a live-strip primitive:
    // the flows do not wrap it, and you can still reach it.
    for (_, strip) in &session.live.strips {
        strip.set_pan_law(PanLaw::Balance);
    }
    println!(
        "both tracks at 0 dB:     {:.3}  (0.25 + 0.25)",
        level(&mut session)?
    );

    // A fader drag: several moves, one undo step.
    for db in [-2.0, -4.0, -6.0206] {
        track::volume(&mut session, gtr, db)?;
    }
    // Quick successive moves merged above; sealing ends the step at once
    // (a pause of half a second would too).
    session.history.seal();
    println!(
        "gtr at -6 dB:            {:.3}  (0.25 + 0.125)",
        level(&mut session)?
    );
    println!("undo steps so far: {}", session.history.undo_len());

    // Pan and mute/solo are heard immediately as well.
    track::pan(&mut session, vox, -1.0)?; // hard left
    let audio = session::steps::render_blocks(&mut session, 4)?;
    let n = audio.frames() - 1;
    println!(
        "vox hard left:           L {:.3}  R {:.3}",
        audio.channel(0)[n],
        audio.channel(1)[n]
    );
    track::pan(&mut session, vox, 0.0)?;
    track::mute(&mut session, gtr, true)?;
    println!("gtr muted:               {:.3}", level(&mut session)?);
    track::mute(&mut session, gtr, false)?;
    track::solo(&mut session, gtr, true)?;
    println!(
        "gtr soloed:              {:.3}  (vox silenced)",
        level(&mut session)?
    );
    track::solo(&mut session, gtr, false)?;

    // A bus fed by sends: the classic reverb return, minus the reverb.
    // Adding the bus and the sends changes the console's shape, so these
    // mark the session for a rebuild; `start`/`drive` rebuild for you, or call
    // `session::steps::rebuild` yourself to control when the swap happens.
    let room = mix::add_bus(&mut session, "room")?;
    let vox_send = mix::add_send(&mut session, vox, room, -6.0206, SendTap::PostFader)?;
    let _gtr_send = mix::add_send(&mut session, gtr, room, -6.0206, SendTap::PostFader)?;
    println!("needs rebuild: {}", session.needs_rebuild);
    session::steps::rebuild(&mut session)?;
    for (_, strip) in &session.live.strips {
        strip.set_pan_law(PanLaw::Balance);
    }
    println!(
        "plus bus with two sends: {:.3}  (tracks + half of each)",
        level(&mut session)?
    );
    mix::set_send_level_db(&mut session, vox_send, -12.04)?;
    println!("vox send at -12 dB:      {:.3}", level(&mut session)?);
    mix::set_send_enabled(&mut session, vox_send, false)?;
    println!("vox send off:            {:.3}", level(&mut session)?);

    // Route gtr *through* the bus instead of sending a copy, and turn the
    // bus down: the bus fader now scales gtr.
    track::route(&mut session, gtr, Some(room))?;
    session::steps::rebuild(&mut session)?;
    for (_, strip) in &session.live.strips {
        strip.set_pan_law(PanLaw::Balance);
    }
    track::volume(&mut session, room, -6.0206)?;
    println!("gtr via bus at -6 dB:    {:.3}", level(&mut session)?);

    // Meters: loudest sample per channel since the last read.
    if let Some([l, r]) = mix::peak(&session, room) {
        println!("room bus meter:          L {l:.3}  R {r:.3}");
    }

    // Everything above is in the document. Undo it all and the document is back
    // to two tracks at 0 dB; the console follows on the next rebuild.
    while session.history.undo_len() > 4 {
        // 4 = the two tracks and two clips we started with
        session::undo(&mut session)?;
    }
    println!(
        "after undo: gtr {} dB, {} tracks, gtr sends {}",
        session.document.track(gtr)?.gain_db,
        session.document.track_count(),
        session.document.track(gtr)?.sends().len()
    );
    Ok(())
}
