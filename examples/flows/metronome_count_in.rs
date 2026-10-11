//! `metronome`: a click while you play, and a count-in before you record.
//!
//! What you'll learn:
//! - The click is a node on the master, built with the mixer; `enable` and
//!   `level` reach it at once, and the first beat of every bar is accented
//! - `count_in` gives a recording that starts at the playhead a run-up:
//!   the transport rolls that many bars early with the click, and drops
//!   into record at the playhead
//! - The clip starts where the recording was meant to, not where the
//!   transport started rolling
//!
//! ```text
//! cargo run --example metronome_count_in
//! ```

use aura::prelude::*;
use aura::primitives::io::capture::capture_queue;
use aura::{metronome, record, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let block = 256;
    let folder = std::env::temp_dir().join("metronome_count_in.aura");
    let mut session = session::create(&folder, rate, block)?;
    session::set_tempo(&mut session, 120.0)?; // a beat is half a second
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    track::arm(&mut session, vox, true)?;

    // Hear the click: one bar of playback, the peak of each beat.
    metronome::enable(&mut session, true)?;
    metronome::level(&mut session, -6.0)?;
    transport::start(&mut session)?;
    let blocks_per_beat = rate.hz() as usize / 2 / block;
    for beat in 1..=4 {
        let audio = session::steps::render_blocks(&mut session, blocks_per_beat)?;
        println!("beat {beat}: click peak {:.3}", audio.peak());
    }
    transport::stop(&mut session)?;
    metronome::enable(&mut session, false)?;

    // Record from bar two with a one-bar count-in. The metronome is off,
    // but the count-in clicks anyway and stops at the punch-in.
    metronome::count_in(&mut session, 1)?;
    transport::seek(&mut session, TimePosition::from_seconds(2.0, rate))?;
    session::steps::render_blocks(&mut session, 1)?;
    let (mut mic, blocks) = capture_queue(1, block, 64)?;
    record::start(
        &mut session,
        record::Source::Queue {
            blocks,
            channels: 1,
        },
    )?;
    let recording = session.recording.as_ref().expect("recording");
    println!(
        "rolling from {:.2} s, recording from {:.2} s",
        recording.started_at().to_seconds(rate),
        recording.punch_in().to_seconds(rate)
    );
    let mut sung = PlanarBuffer::new(1, block);
    sung.fill(0.3);
    let mut dropped_in = None;
    let mut count_in_peak = 0.0f32;
    for n in 0..(3 * rate.hz() as usize / block) {
        mic.push(&sung, (n * block) as u64 * 1_000_000 / rate.hz() as u64);
        let heard = session::steps::render_blocks(&mut session, 1)?;
        if transport::state(&session) == TransportState::Recording {
            dropped_in.get_or_insert(transport::playhead(&session).to_seconds(rate));
        } else {
            count_in_peak = count_in_peak.max(heard.peak());
        }
    }
    println!(
        "count-in click peak {count_in_peak:.3}; dropped into record at {:.2} s",
        dropped_in.unwrap_or_default()
    );
    let clips = record::stop(&mut session)?;
    let clip = session.memory.clip(clips[0])?;
    println!(
        "clip \"{}\" starts at {:.2} s and runs {:.2} s",
        clip.name,
        clip.start().to_seconds(rate),
        clip.length().to_seconds(rate)
    );
    Ok(())
}
