//! Bounce (offline render → file) and export helpers.
//!
//! MPL-2.0 boundary: symphonia is confined to decode.rs — this file never
//! imports it; it only combines [`offline`](crate::primitives::io::offline) and [`wav`](crate::primitives::io::wav).

use std::path::Path;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::BlockProcessor;
use crate::primitives::{Result, SampleRate};

use crate::primitives::io::wav::{self, WavFormat};
use crate::primitives::io::{offline, Engine};

use super::AudioFile;

/// Renders `total_frames` from `engine` at `sample_rate` in `block_size`
/// blocks and returns the result as an [`AudioFile`] (nothing is written).
///
/// This is [`offline::render`] with the rate attached, so the result can be
/// passed straight to [`export_wav`], [`AudioFile::resampled_to`] or a
/// session. The engine is prepared here; `transport_mut().play()` first if
/// the transport should move.
pub fn bounce<P: BlockProcessor>(
    engine: &mut Engine<P>,
    sample_rate: SampleRate,
    block_size: usize,
    total_frames: usize,
) -> Result<AudioFile> {
    let buffer = offline::render(engine, sample_rate, block_size, total_frames)?;
    Ok(AudioFile {
        buffer,
        sample_rate,
    })
}

/// Renders `total_frames` from `engine` and writes them to `path` as WAV.
///
/// Returns the rendered buffer as well, so callers can meter or reuse it
/// without reading the file back. The file's declared rate is
/// `sample_rate`; no resampling happens here.
pub fn bounce_wav<P: BlockProcessor>(
    engine: &mut Engine<P>,
    sample_rate: SampleRate,
    block_size: usize,
    total_frames: usize,
    path: impl AsRef<Path>,
    format: WavFormat,
) -> Result<PlanarBuffer> {
    let rendered = bounce(engine, sample_rate, block_size, total_frames)?;
    wav::write(path, &rendered.buffer, sample_rate, format)?;
    Ok(rendered.buffer)
}

/// Writes a decoded or rendered [`AudioFile`] to `path` as WAV at its own
/// sample rate. Thin wrapper over [`wav::write`].
pub fn export_wav(path: impl AsRef<Path>, audio: &AudioFile, format: WavFormat) -> Result<()> {
    wav::write(path, &audio.buffer, audio.sample_rate, format)
}
