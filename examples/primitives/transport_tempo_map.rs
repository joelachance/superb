//! `primitives::transport`: musical time vs sample time, and how a block
//! is cut into segments when the loop wraps.
//!
//! What you'll learn:
//! - `TempoMap` converts beats <-> samples with tempo and meter changes
//! - Bars/beats/ticks (`Bbt`) for display
//! - `Transport` holds the playhead and loop; `advance` returns the
//!   `Segment`s a block covers, so nodes can render across a loop point
//!
//! ```text
//! cargo run --example transport_tempo_map
//! ```

use superb::prelude::*;

fn main() -> Result<()> {
    let rate = SampleRate::HZ_48K;
    // 120 bpm in 4/4 to start; slow to 90 bpm at bar 3 (beat 8); 3/4 from bar 4.
    let mut map = TempoMap::new(rate, 120.0);
    map.add_tempo(BeatPosition::new(8.0), 90.0)?;
    map.add_meter(4, TimeSignature::new(3, 4)?)?;

    println!("beat   bar.beat.tick   seconds   samples   bpm");
    for beat in [0.0, 4.0, 7.5, 8.0, 12.0, 15.0] {
        let b = BeatPosition::new(beat);
        println!(
            "{:>4}   {:<14}  {:>7.3}   {:>7}   {:>3}",
            beat,
            map.beat_to_bbt(b).to_string(),
            map.beat_to_seconds(b),
            map.beat_to_samples(b).samples(),
            map.tempo_at(b)
        );
    }
    // And back: a sample position to a beat.
    let two_seconds = TimePosition::from_seconds(2.0, rate);
    println!(
        "2.000 s = beat {:.3}",
        map.samples_to_beat(two_seconds).beats()
    );

    // A transport on that map, looping bar 2 (beats 4..8).
    let mut transport = Transport::with_tempo_map(map);
    let range = LoopRange::new(
        transport
            .tempo_map()
            .beat_to_samples(BeatPosition::new(4.0)),
        transport
            .tempo_map()
            .beat_to_samples(BeatPosition::new(8.0)),
    )?;
    transport.set_loop(Some(range));
    transport.set_loop_enabled(true);
    // Start 100 frames before the loop end.
    transport.seek(range.end().offset(-100));
    transport.play();

    // One 256-frame block straddles the loop end: two segments.
    println!("\nblock of 256 frames across the loop point:");
    for seg in transport.advance(256) {
        println!(
            "  offset {:>3}, {:>3} frames, from sample {}{}",
            seg.block_offset,
            seg.frames,
            seg.start.samples(),
            if seg.wrapped {
                " (wrapped to loop start)"
            } else {
                ""
            }
        );
    }
    println!(
        "playhead now at beat {:.3}",
        transport.beat_position().beats()
    );

    // Stopped transports still yield one segment so nodes see a full block.
    transport.stop();
    let segs: Vec<_> = transport.advance(256).collect();
    println!(
        "stopped: {} segment(s) of {} frames, position unchanged",
        segs.len(),
        segs[0].frames
    );
    Ok(())
}
