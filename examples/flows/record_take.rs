//! `record`: arm a track, record, get a clip.
//!
//! What you'll learn:
//! - Armed audio tracks receive a clip when a recording stops, holding the
//!   input channel(s) chosen with `track::input`
//! - `record::start` takes a `Source`: a hardware input (feature `devices`),
//!   gathered on a thread, or a capture queue you fill yourself, which is
//!   what this example does
//! - A monitoring audio track plays the session's audio input through its
//!   strip, so you hear yourself while recording
//! - `record::stop` writes the audio as a WAV file into the session folder,
//!   places the clips as one undo step and stops the transport
//!
//! ```text
//! cargo run --example record_take
//! ```

use superb::prelude::*;
use superb::primitives::io::capture::capture_queue;
use superb::primitives::io::{EngineCommand, LiveInput};
use superb::{record, session, track, transport};

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    let block = 256;
    let folder = std::env::temp_dir().join("record_take.superb");
    let mut session = session::create(&folder, rate, block)?;
    let vox = track::add(&mut session, "vox", TrackKind::Audio)?;
    let _bed = track::add(&mut session, "bed", TrackKind::Audio)?;

    // Arm the vocal and say which input feeds it: the first channel of the
    // interface, recorded as mono. Both are track state: saved, not undo
    // steps. A track with no input set records everything captured.
    track::arm(&mut session, vox, true)?;
    track::input(&mut session, vox, Some(Input::Mono(0)))?;
    println!("vox armed: {}", session.memory.track(vox)?.state.arm);

    // Hear yourself: a monitoring audio track plays the session's audio
    // input through its strip. With a real interface `devices::input` opens
    // the input; offline we feed the engine's live input from a queue.
    track::monitor(&mut session, vox, true)?;
    let (mut ear, live) = capture_queue(1, block, 8)?;
    session
        .engine
        .send(EngineCommand::LiveInput(LiveInput::new(live, 1, block)?))?;

    // Roll the transport a second in, then punch in there.
    transport::start(&mut session)?;
    session::steps::render_blocks(&mut session, rate.hz() as usize / block)?;
    println!(
        "punching in at {:.3} s",
        transport::playhead(&session).to_seconds(rate)
    );

    // Our "microphone": a mono capture queue with 64 pre-allocated blocks.
    // `Source::Device` would open a real input and feed the same kind of queue.
    let (mut mic, blocks) = capture_queue(1, block, 64)?;
    record::start(
        &mut session,
        record::Source::Queue {
            blocks,
            channels: 1,
        },
    )?;
    println!("recording: {}", session.recording.is_some());

    // Half a second of "singing" (a 440 Hz tone), one block at a time,
    // while the engine keeps rolling. On a device both sides run on their
    // own threads; here they take turns. The queue is small, so the recording
    // has to be collected as it goes: `session::steps::render_blocks` does that for a
    // manual capture (a device capture has its own collector thread).
    let mut phase = 0.0f32;
    let mut monitored = 0.0f32;
    let mut sung = PlanarBuffer::new(1, block);
    let blocks = rate.hz() as usize / 2 / block;
    for n in 0..blocks {
        for s in sung.channel_mut(0) {
            *s = 0.4 * phase.sin();
            phase += 440.0 * core::f32::consts::TAU / rate.hz() as f32;
        }
        let timestamp_us = (n * block) as u64 * 1_000_000 / rate.hz() as u64;
        mic.push(&sung, timestamp_us);
        ear.push(&sung, timestamp_us);
        let heard = session::steps::render_blocks(&mut session, 1)?;
        monitored = monitored.max(heard.peak());
        if n == blocks / 2 {
            println!(
                "half way ({:?}): {} frames collected, {} dropped",
                transport::state(&session),
                record::collect(&mut session),
                session.recording.as_ref().map_or(0, |r| r.dropped())
            );
        }
    }

    println!("heard while recording: peak {monitored:.3} on the vox strip");

    // Stop: the recording is drained, written to the folder and placed on
    // every armed track, starting at the punch-in point.
    let clips = record::stop(&mut session)?;
    if let Ok(entries) = std::fs::read_dir(folder.join("audio")) {
        for entry in entries.flatten() {
            let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
            println!("written {} ({bytes} bytes)", entry.path().display());
        }
    }
    for id in &clips {
        let clip = session.memory.clip(*id)?;
        println!(
            "clip {:?} on {}: {:.3} s .. {:.3} s (source {:?})",
            clip.name,
            session.memory.track(session.memory.clip_track(*id)?)?.name,
            clip.start().to_seconds(rate),
            (clip.start() + clip.length()).to_seconds(rate),
            clip.source
        );
    }
    println!("undo label: {:?}", session.history.undo_label());

    // Hear it back from the punch-in point.
    transport::seek(&mut session, TimePosition::from_seconds(1.0, rate))?;
    transport::start(&mut session)?;
    let heard = session::steps::render_blocks(&mut session, 8)?;
    println!("playback of the clip, peak {:.3}", heard.peak());

    // Undo removes the clip; the file and the audio in memory stay.
    session::undo(&mut session)?;
    println!(
        "after undo: {} clips, {} recording(s) in memory",
        session.memory.clip_count(),
        session.audio.len()
    );

    // With a real interface:
    #[cfg(feature = "devices")]
    {
        use superb::devices;
        if devices::inputs()?.is_empty() {
            println!("no input device to record from");
        } else {
            track::arm(&mut session, vox, true)?;
            // The default input; a thread gathers the audio.
            record::start(&mut session, record::Source::Device(None))?;
            std::thread::sleep(std::time::Duration::from_millis(500));
            let clips = record::stop(&mut session)?;
            println!("recorded {} clip(s) from the default input", clips.len());
        }
    }
    Ok(())
}
