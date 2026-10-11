//! `primitives::realtime`: how the UI talks to the audio thread without
//! ever making it wait.
//!
//! What you'll learn:
//! - `spsc::channel`: a wait-free queue, one producer, one consumer, fixed
//!   capacity; `push` fails instead of blocking when it is full
//! - `AtomicF32`: a knob both threads can read and write
//! - The audio side pops at the start of each block and never allocates
//!
//! ```text
//! cargo run --example realtime_queue
//! ```

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use aura::prelude::*;
use aura::primitives::realtime::spsc;

/// What the UI sends to the audio thread. Plain data, no heap.
#[derive(Debug, Clone, Copy)]
enum Message {
    Play,
    Stop,
    Seek(i64),
}

fn main() -> Result<()> {
    // Room for 8 messages between blocks; the queue never grows.
    let (mut to_audio, mut from_ui) = spsc::channel::<Message>(8);
    // A fader shared by value: the UI stores, the audio thread loads.
    let fader = Arc::new(AtomicF32::new(1.0));
    let fader_for_audio = Arc::clone(&fader);

    // The "audio thread": 20 blocks, 5 ms apart, applying what has arrived.
    let audio = thread::spawn(move || {
        let mut playing = false;
        let mut position = 0i64;
        for block in 0..20 {
            while let Some(msg) = from_ui.pop() {
                match msg {
                    Message::Play => playing = true,
                    Message::Stop => playing = false,
                    Message::Seek(to) => position = to,
                }
            }
            let gain = fader_for_audio.get();
            if playing {
                position += 256;
            }
            if block % 5 == 4 {
                println!("audio: block {block:>2}  playing={playing}  position={position:>5}  gain={gain:.2}");
            }
            thread::sleep(Duration::from_millis(5));
        }
        from_ui.is_abandoned()
    });

    // The "UI thread".
    to_audio.push(Message::Play).ok();
    thread::sleep(Duration::from_millis(30));
    fader.set(0.5);
    to_audio.push(Message::Seek(48_000)).ok();
    thread::sleep(Duration::from_millis(30));
    to_audio.push(Message::Stop).ok();

    // A full queue refuses the message rather than waiting: the UI decides
    // what to do (drop it, retry next frame, show a warning).
    let mut refused = 0;
    for _ in 0..12 {
        if to_audio.push(Message::Seek(0)).is_err() {
            refused += 1;
        }
    }
    println!(
        "ui: {refused} of 12 rapid messages refused (queue holds {})",
        to_audio.capacity()
    );

    // Dropping the producer is how the audio side learns the UI is gone.
    drop(to_audio);
    let ui_gone_when_audio_finished = audio.join().expect("audio thread");
    println!("audio saw the UI side dropped: {ui_gone_when_audio_finished}");
    Ok(())
}
