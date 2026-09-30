//! Integration tests for the `files` feature: WAV round trips through
//! symphonia, the committed FLAC fixture, bounce → decode, and rejection of
//! non-audio bytes. Run with `cargo test -p superb-io --features files`.
#![cfg(feature = "files")]

use std::path::PathBuf;

use superb_buffers::PlanarBuffer;
use superb_core::SampleRate;
use superb_graph::nodes::SineNode;
use superb_graph::AudioGraph;
use superb_io::files::{self, bounce_wav, decode, decode_at_rate, decode_bytes, decode_from, export_wav, AudioFile};
use superb_io::wav::{self, WavFormat};
use superb_io::Engine;
use superb_transport::Transport;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("superb-files-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ramp_fixture(channels: usize, frames: usize) -> PlanarBuffer {
    let mut b = PlanarBuffer::new(channels, frames);
    for ch in 0..channels {
        for (i, s) in b.channel_mut(ch).iter_mut().enumerate() {
            // distinct, deterministic, within ±1
            *s = ((i as f32 * 0.37 + ch as f32 * 100.0).sin() * 0.9).clamp(-1.0, 1.0);
        }
    }
    b
}

fn zero_crossings(x: &[f32]) -> usize {
    x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count()
}

fn fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(name)
}

#[test]
fn float32_wav_decodes_bit_exact() {
    let dir = TempDir::new("f32");
    let src = ramp_fixture(3, 1_234);
    let path = dir.path("ramp.wav");
    wav::write(&path, &src, SampleRate::HZ_96K, WavFormat::Float32).unwrap();

    let file = decode(&path).unwrap();
    assert_eq!(file.sample_rate, SampleRate::HZ_96K);
    assert_eq!(file.channels(), 3);
    assert_eq!(file.frames(), 1_234);
    for ch in 0..3 {
        assert_eq!(file.buffer.channel(ch), src.channel(ch), "channel {ch}");
    }
    assert!((file.duration_seconds() - 1_234.0 / 96_000.0).abs() < 1e-9);
}

#[test]
fn pcm16_wav_decodes_within_quantisation() {
    let dir = TempDir::new("pcm16");
    let src = ramp_fixture(2, 777);
    let path = dir.path("ramp16.wav");
    wav::write(&path, &src, SampleRate::CD, WavFormat::Pcm16).unwrap();

    let file = decode(&path).unwrap();
    assert_eq!(file.sample_rate, SampleRate::CD);
    assert_eq!((file.channels(), file.frames()), (2, 777));
    for ch in 0..2 {
        for (a, b) in file.buffer.channel(ch).iter().zip(src.channel(ch)) {
            assert!((a - b).abs() < 1.0 / 32_000.0, "ch{ch}: {a} vs {b}");
        }
    }
    // Our own reader and symphonia agree exactly on the same bytes.
    let (ours, _) = wav::read(&path).unwrap();
    assert_eq!(ours.channel(0), file.buffer.channel(0));
}

#[test]
fn decode_from_reader_and_bytes_match_path() {
    let dir = TempDir::new("reader");
    let src = ramp_fixture(1, 500);
    let path = dir.path("mono.wav");
    wav::write(&path, &src, SampleRate::HZ_48K, WavFormat::Float32).unwrap();

    let via_path = decode(&path).unwrap();
    let via_reader = decode_from(std::fs::File::open(&path).unwrap()).unwrap();
    let via_bytes = decode_bytes(std::fs::read(&path).unwrap(), Some("wav")).unwrap();
    let mut in_memory = Vec::new();
    wav::write_to(&mut in_memory, &src, SampleRate::HZ_48K, WavFormat::Float32).unwrap();
    let via_cursor = decode_from(std::io::Cursor::new(in_memory)).unwrap();

    for f in [&via_reader, &via_bytes, &via_cursor] {
        assert_eq!(f.sample_rate, via_path.sample_rate);
        assert_eq!(f.buffer.channel(0), via_path.buffer.channel(0));
    }
}

#[test]
fn flac_fixture_decodes() {
    let file = decode(fixture_path("stereo_sine_44k1.flac")).unwrap();
    assert_eq!(file.sample_rate, SampleRate::CD);
    assert_eq!(file.channels(), 2);
    assert_eq!(file.frames(), 8_820); // 0.2 s
    assert!((file.buffer.peak() - 0.5).abs() < 0.01, "peak {}", file.buffer.peak());
    // 440 Hz left / 880 Hz right over 0.2 s: 88 and 176 cycles.
    let left = zero_crossings(file.buffer.channel(0));
    let right = zero_crossings(file.buffer.channel(1));
    assert!((174..=178).contains(&left), "left crossings {left}");
    assert!((350..=354).contains(&right), "right crossings {right}");

    // Extension hints are optional: the content is probed.
    let bytes = std::fs::read(fixture_path("stereo_sine_44k1.flac")).unwrap();
    let unhinted = decode_bytes(bytes, None).unwrap();
    assert_eq!(unhinted.buffer.channel(1), file.buffer.channel(1));
}

#[test]
fn flac_fixture_resampled_to_48k() {
    let file = decode_at_rate(fixture_path("stereo_sine_44k1.flac"), SampleRate::HZ_48K).unwrap();
    assert_eq!(file.sample_rate, SampleRate::HZ_48K);
    assert_eq!(file.frames(), 9_600);
    assert!((file.buffer.peak() - 0.5).abs() < 0.03);
    // Same number of cycles, now over 9_600 frames.
    let left = zero_crossings(&file.buffer.channel(0)[..9_500]);
    assert!((170..=178).contains(&left), "left crossings {left}");
}

#[test]
fn bounce_wav_then_decode_matches_render() {
    let dir = TempDir::new("bounce");
    let mut graph = AudioGraph::new();
    let sine = graph.add_node(Box::new(SineNode::new(220.0, 0.8, 2)));
    graph.set_output(sine).unwrap();
    let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
    engine.transport_mut().play();

    let path = dir.path("bounce.wav");
    let rendered = bounce_wav(&mut engine, SampleRate::HZ_48K, 128, 12_000, &path, WavFormat::Float32).unwrap();
    assert_eq!((rendered.channels(), rendered.frames()), (2, 12_000));
    assert!((rendered.peak() - 0.8).abs() < 1e-3);

    let decoded = decode(&path).unwrap();
    assert_eq!(decoded.sample_rate, SampleRate::HZ_48K);
    assert_eq!(decoded.frames(), 12_000);
    assert_eq!(decoded.buffer.channel(0), rendered.channel(0));
    assert_eq!(decoded.buffer.channel(1), rendered.channel(1));
    let energy = |b: &PlanarBuffer| b.rms();
    assert!((energy(&decoded.buffer) - energy(&rendered)).abs() < 1e-6);

    // `bounce` alone returns an AudioFile that `export_wav` can write later.
    let again = files::bounce(&mut engine, SampleRate::HZ_48K, 128, 3_000).unwrap();
    assert_eq!(again.frames(), 3_000);
    let out = dir.path("again.wav");
    export_wav(&out, &again, WavFormat::Pcm16).unwrap();
    let back = decode(&out).unwrap();
    assert_eq!(back.frames(), 3_000);
    assert!((back.buffer.peak() - 0.8).abs() < 1e-3);
}

#[test]
fn audio_file_helpers() {
    let dir = TempDir::new("helpers");
    let file = AudioFile { buffer: ramp_fixture(2, 4_800), sample_rate: SampleRate::HZ_48K };
    let same = file.resampled_to(SampleRate::HZ_48K).unwrap();
    assert_eq!(same.buffer.channel(0), file.buffer.channel(0));
    let cd = file.resampled_to(SampleRate::CD).unwrap();
    assert_eq!(cd.frames(), 4_410);
    assert_eq!(cd.sample_rate, SampleRate::CD);
    let path = dir.path("cd.wav");
    cd.write_wav(&path, WavFormat::Float32).unwrap();
    assert_eq!(decode(&path).unwrap().sample_rate, SampleRate::CD);
}

#[test]
fn rejects_garbage_and_missing_files() {
    let dir = TempDir::new("garbage");
    let path = dir.path("noise.wav");
    let junk: Vec<u8> = (0..4_096u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    std::fs::write(&path, &junk).unwrap();
    assert!(decode(&path).is_err());
    assert!(decode_bytes(junk.clone(), Some("flac")).is_err());
    assert!(decode_from(std::io::Cursor::new(junk)).is_err());
    assert!(decode_bytes(Vec::new(), None).is_err());
    assert!(decode(dir.path("does-not-exist.wav")).is_err());
    assert!(decode_bytes(b"RIFF\0\0\0\0WAVE".to_vec(), Some("wav")).is_err());
}

#[test]
fn truncated_wav_yields_the_frames_present() {
    let dir = TempDir::new("truncated");
    let src = ramp_fixture(2, 10_000);
    let path = dir.path("full.wav");
    wav::write(&path, &src, SampleRate::CD, WavFormat::Float32).unwrap();
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(44 + 10_000 * 2 * 4 / 2); // header + half the data
    let file = decode_bytes(bytes, Some("wav")).unwrap();
    assert_eq!(file.channels(), 2);
    assert!(file.frames() >= 4_000 && file.frames() <= 5_000, "frames {}", file.frames());
    assert_eq!(&file.buffer.channel(0)[..4_000], &src.channel(0)[..4_000]);
}
