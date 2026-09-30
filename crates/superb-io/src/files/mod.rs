//! Audio file decoding, resampling and bounce/export helpers (feature `files`).
//!
//! **MPL-2.0 boundary: symphonia is confined to `decode.rs`.** This module
//! and every other Superb source file only see Superb types
//! ([`PlanarBuffer`], [`SampleRate`], [`SuperbError`]); the symphonia
//! crate (MPL-2.0, file-level copyleft) is imported by exactly one file,
//! `src/files/decode.rs`, which is the only file subject to that licence's
//! per-file obligations. A workspace-wide test in `lib.rs` enforces this.
//!
//! * [`decode`] / [`decode_from`] / [`decode_bytes`] — any container/codec
//!   symphonia was built with (WAV + FLAC by default) into an [`AudioFile`].
//! * [`decode_at_rate`] — decode and resample to a target rate in one call.
//! * [`resample`] — offline sample-rate conversion with rubato (MIT).
//! * [`bounce_wav`] / [`export_wav`] — render an [`Engine`](crate::Engine)
//!   offline and write it, or write an already decoded [`AudioFile`].
//!
//! Everything here allocates freely and is meant for the control / worker
//! thread, never the audio callback.
//!
//! ```
//! use superb_core::SampleRate;
//! use superb_graph::nodes::SineNode;
//! use superb_graph::AudioGraph;
//! use superb_io::files::{bounce_wav, decode_at_rate};
//! use superb_io::wav::WavFormat;
//! use superb_io::Engine;
//! use superb_transport::Transport;
//!
//! let dir = std::env::temp_dir().join(format!("superb-files-doc-{}", std::process::id()));
//! std::fs::create_dir_all(&dir).unwrap();
//! let path = dir.join("bounce.wav");
//!
//! let mut graph = AudioGraph::new();
//! let sine = graph.add_node(Box::new(SineNode::new(440.0, 0.5, 2)));
//! graph.set_output(sine).unwrap();
//! let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
//! engine.transport_mut().play();
//!
//! let rendered = bounce_wav(&mut engine, SampleRate::HZ_48K, 256, 4_800, &path, WavFormat::Float32).unwrap();
//! assert_eq!(rendered.frames(), 4_800);
//!
//! // Read it back through symphonia and convert to CD rate on the way.
//! let file = decode_at_rate(&path, SampleRate::CD).unwrap();
//! assert_eq!(file.sample_rate, SampleRate::CD);
//! assert_eq!(file.channels(), 2);
//! assert_eq!(file.frames(), 4_410);
//! assert!((file.buffer.peak() - 0.5).abs() < 0.05);
//! std::fs::remove_dir_all(&dir).unwrap();
//! ```

mod bounce;
mod decode;
mod resample;

use std::io::Read;
use std::path::Path;

use superb_buffers::PlanarBuffer;
use superb_core::{Result, SampleRate};

pub use bounce::{bounce, bounce_wav, export_wav};
pub use resample::{resample, resampled_frames};

/// A fully decoded audio file: planar `f32` samples plus the rate they were
/// recorded at.
#[derive(Clone, Debug)]
pub struct AudioFile {
    /// The samples, one plane per channel, in file channel order.
    pub buffer: PlanarBuffer,
    /// The sample rate declared by the container / codec.
    pub sample_rate: SampleRate,
}

impl AudioFile {
    /// Number of channels.
    pub fn channels(&self) -> usize {
        self.buffer.channels()
    }

    /// Number of frames (samples per channel).
    pub fn frames(&self) -> usize {
        self.buffer.frames()
    }

    /// Length in seconds at [`sample_rate`](Self::sample_rate).
    pub fn duration_seconds(&self) -> f64 {
        self.buffer.frames() as f64 / self.sample_rate.as_f64()
    }

    /// Returns this file converted to `target`, or an unchanged clone when
    /// the rates already match. See [`resample`] for the frame-count rule.
    pub fn resampled_to(&self, target: SampleRate) -> Result<AudioFile> {
        if target == self.sample_rate {
            return Ok(self.clone());
        }
        Ok(AudioFile { buffer: resample(&self.buffer, self.sample_rate, target)?, sample_rate: target })
    }

    /// Writes this file as WAV; see [`export_wav`].
    pub fn write_wav(&self, path: impl AsRef<Path>, format: crate::wav::WavFormat) -> Result<()> {
        export_wav(path, self, format)
    }
}

/// Decodes the audio file at `path`.
///
/// The container and codec are probed from the content (the extension is
/// only a hint). With the default `files` build that means WAV (PCM 8/16/24/32-bit
/// and float) and FLAC. The first audio track is decoded in full; a stream
/// truncated mid-file yields the frames decoded so far rather than an error,
/// and packets that fail to decode are skipped, while data that is not a
/// recognised audio container fails with [`SuperbError::Message`](superb_core::SuperbError::Message).
pub fn decode(path: impl AsRef<Path>) -> Result<AudioFile> {
    decode::decode_path(path.as_ref())
}

/// Decodes an audio stream from any reader (non-seekable; the whole stream
/// is consumed). Prefer [`decode`] for files so symphonia can seek.
pub fn decode_from<R: Read + Send + Sync>(reader: R) -> Result<AudioFile> {
    decode::decode_reader(reader, None)
}

/// Decodes an in-memory audio file (seekable). `extension_hint`, e.g.
/// `"flac"`, speeds up probing but is not required.
pub fn decode_bytes(bytes: Vec<u8>, extension_hint: Option<&str>) -> Result<AudioFile> {
    decode::decode_vec(bytes, extension_hint)
}

/// [`decode`] followed by [`resample`] to `target` when the file's rate differs.
pub fn decode_at_rate(path: impl AsRef<Path>, target: SampleRate) -> Result<AudioFile> {
    decode(path)?.resampled_to(target)
}
