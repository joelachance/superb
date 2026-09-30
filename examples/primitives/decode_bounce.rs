//! Offline bounce → decode → resample → export, all without hardware.
//!
//! What you'll learn:
//! - Mixer + track strip vs raw graph nodes
//! - `bounce_wav` (render engine → WAV) and `decode` (WAV → AudioFile)
//! - Resampling 48 kHz → CD rate (44.1 kHz) with the `files` feature
//! - `Result<()>` / `?` through a short I/O pipeline
//!
//! ```text
//! cargo run --example decode_bounce
//! ```
//!
//! Writes `decode_bounce_48k.wav` (the bounce) and `decode_bounce_44k1.wav`
//! (the same audio decoded back and converted to CD rate) in the working
//! directory. Everything file-related lives behind `superb`'s `files`
//! feature, which this example enables in its `Cargo.toml`.

use superb::prelude::*;
use superb::primitives::io::files::{bounce_wav, decode, resampled_frames};
use superb::primitives::io::wav::WavFormat;

fn main() -> Result<()> {
    // SampleRate is a newtype around Hz — HZ_48K is the usual DAW default.
    let rate = SampleRate::HZ_48K;

    // Mixer owns an AudioGraph plus track "strips" (gain/pan/mute meters).
    // `2` = stereo master channel count.
    let mut mixer = Mixer::new(2);
    let track = mixer.add_track("tone");

    // graph_mut() borrows the mixer's graph so we can add DSP nodes.
    // SineNode(freq_hz, amplitude, channels); Box::new for the Node trait object.
    let osc = mixer
        .graph_mut()
        .add_node(Box::new(SineNode::new(330.0, 0.6, 2)));
    // Wire the oscillator into this track's input (not straight to master).
    mixer.connect_source(osc, track)?;
    // track_params returns a handle to the strip; set_gain_db is a fader in dB.
    mixer.track_params(track)?.set_gain_db(-3.0);

    // Engine = graph/mixer processor + Transport (tempo/playhead).
    let mut engine = Engine::new(mixer, Transport::new(rate, 120.0));
    engine.transport_mut().play();

    let bounced = "decode_bounce_48k.wav";
    // bounce_wav: render `frames` of audio in `block`-sized chunks, write WAV.
    // rate.hz() as usize * 2 ≈ two seconds at 48 kHz.
    let rendered = bounce_wav(
        &mut engine,
        rate,
        256,
        rate.hz() as usize * 2,
        bounced,
        WavFormat::Float32,
    )?;
    println!(
        "bounced  {bounced}: {} frames, {} ch, peak {:.3}",
        rendered.frames(),
        rendered.channels(),
        rendered.peak()
    );

    // decode reads the file back into an AudioFile (buffer + sample rate metadata).
    let file = decode(bounced)?;
    println!(
        "decoded  {bounced}: {} frames @ {} ({:.2} s), peak {:.3}",
        file.frames(),
        file.sample_rate,
        file.duration_seconds(),
        file.buffer.peak()
    );

    // SampleRate::CD is 44_100 Hz. resampled_to returns a new AudioFile.
    let target = SampleRate::CD;
    let converted = file.resampled_to(target)?;
    // Sanity-check frame math against the helper used by the resampler.
    assert_eq!(
        converted.frames(),
        resampled_frames(file.frames(), file.sample_rate, target)
    );
    let out = "decode_bounce_44k1.wav";
    // Pcm16 = 16-bit integer WAV (what CD-style files usually use).
    converted.write_wav(out, WavFormat::Pcm16)?;
    println!(
        "exported {out}: {} frames @ {}, peak {:.3}",
        converted.frames(),
        converted.sample_rate,
        converted.buffer.peak()
    );

    // Round-trip check: decode the CD-rate file and print its rate.
    let check = decode(out)?;
    println!(
        "re-read  {out}: {} frames @ {}",
        check.frames(),
        check.sample_rate
    );
    Ok(())
}
