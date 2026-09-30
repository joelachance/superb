//! Minimal RIFF/WAVE writer and reader.
//!
//! Supports 16-bit PCM and 32-bit IEEE float, interleaved, any channel
//! count. Enough for bouncing test renders and reading them back; for
//! 24-bit, extensible headers, FLAC and other codecs use `files::decode`
//! (feature `files`), which wraps a full decoder.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

use crate::primitives::buffers::convert::f32_to_i16_saturating;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::{Result, SampleRate, SuperbError};

/// Sample encoding used on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    /// 16-bit signed integer PCM.
    Pcm16,
    /// 32-bit IEEE float.
    Float32,
}

impl WavFormat {
    fn bytes_per_sample(self) -> u16 {
        match self {
            Self::Pcm16 => 2,
            Self::Float32 => 4,
        }
    }

    fn format_tag(self) -> u16 {
        match self {
            Self::Pcm16 => 1,
            Self::Float32 => 3,
        }
    }
}

fn io_err(e: io::Error) -> SuperbError {
    SuperbError::Message(format!("wav io error: {e}"))
}

/// Writes `audio` to `path` in the given format.
pub fn write(
    path: impl AsRef<Path>,
    audio: &PlanarBuffer,
    sample_rate: SampleRate,
    format: WavFormat,
) -> Result<()> {
    let file = File::create(path).map_err(io_err)?;
    let mut w = BufWriter::new(file);
    write_to(&mut w, audio, sample_rate, format)?;
    w.flush().map_err(io_err)
}

/// Writes `audio` to any writer.
pub fn write_to(
    w: &mut impl Write,
    audio: &PlanarBuffer,
    sample_rate: SampleRate,
    format: WavFormat,
) -> Result<()> {
    let channels = audio.channels();
    if channels == 0 || channels > u16::MAX as usize {
        return Err(SuperbError::InvalidArgument {
            what: "wav channels",
        });
    }
    let bps = format.bytes_per_sample();
    let block_align = channels as u16 * bps;
    let data_len = (audio.frames() * block_align as usize) as u32;
    let byte_rate = sample_rate.hz() * block_align as u32;

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&format.format_tag().to_le_bytes());
    header.extend_from_slice(&(channels as u16).to_le_bytes());
    header.extend_from_slice(&sample_rate.hz().to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&(bps * 8).to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    w.write_all(&header).map_err(io_err)?;

    let mut body = Vec::with_capacity(data_len as usize);
    for frame in 0..audio.frames() {
        for ch in 0..channels {
            let s = audio.channel(ch)[frame];
            match format {
                WavFormat::Pcm16 => body.extend_from_slice(&f32_to_i16_saturating(s).to_le_bytes()),
                WavFormat::Float32 => body.extend_from_slice(&s.to_le_bytes()),
            }
        }
    }
    w.write_all(&body).map_err(io_err)
}

/// Reads a WAV file written by [`write()`] (or any plain PCM16 / float32 WAV).
pub fn read(path: impl AsRef<Path>) -> Result<(PlanarBuffer, SampleRate)> {
    let file = File::open(path).map_err(io_err)?;
    read_from(&mut BufReader::new(file))
}

/// Reads a WAV stream.
pub fn read_from(r: &mut impl Read) -> Result<(PlanarBuffer, SampleRate)> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes).map_err(io_err)?;
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(SuperbError::Message("not a RIFF/WAVE file".into()));
    }
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // tag, channels, rate, bits
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let start = pos + 8;
        let end = (start + len).min(bytes.len());
        match id {
            b"fmt " => {
                if len < 16 {
                    return Err(SuperbError::Message("fmt chunk too short".into()));
                }
                let c = &bytes[start..];
                let tag = u16::from_le_bytes([c[0], c[1]]);
                let channels = u16::from_le_bytes([c[2], c[3]]);
                let rate = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
                let bits = u16::from_le_bytes([c[14], c[15]]);
                fmt = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(&bytes[start..end]),
            _ => {}
        }
        pos = start + len + (len & 1); // chunks are word aligned
    }
    let (tag, channels, rate, bits) =
        fmt.ok_or_else(|| SuperbError::Message("missing fmt chunk".into()))?;
    let data = data.ok_or_else(|| SuperbError::Message("missing data chunk".into()))?;
    let format = match (tag, bits) {
        (1, 16) => WavFormat::Pcm16,
        (3, 32) => WavFormat::Float32,
        _ => {
            return Err(SuperbError::Message(format!(
                "unsupported wav format tag {tag} / {bits} bits"
            )))
        }
    };
    if channels == 0 {
        return Err(SuperbError::InvalidArgument {
            what: "wav channels",
        });
    }
    let sample_rate = SampleRate::new(rate)?;
    let bps = format.bytes_per_sample() as usize;
    let frames = data.len() / (bps * channels as usize);
    let mut audio = PlanarBuffer::new(channels as usize, frames);
    let mut i = 0;
    for frame in 0..frames {
        for ch in 0..channels as usize {
            let s = match format {
                WavFormat::Pcm16 => i16::from_le_bytes([data[i], data[i + 1]]) as f32 / 32_768.0,
                WavFormat::Float32 => {
                    f32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]])
                }
            };
            audio.channel_mut(ch)[frame] = s;
            i += bps;
        }
    }
    Ok((audio, sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PlanarBuffer {
        let mut b = PlanarBuffer::new(2, 5);
        b.channel_mut(0)
            .copy_from_slice(&[0.0, 0.5, -0.5, 1.0, -1.0]);
        b.channel_mut(1)
            .copy_from_slice(&[0.25, -0.25, 0.125, 0.0, 0.75]);
        b
    }

    #[test]
    fn float32_round_trip_is_exact() {
        let mut bytes = Vec::new();
        write_to(&mut bytes, &fixture(), SampleRate::CD, WavFormat::Float32).unwrap();
        assert_eq!(bytes.len(), 44 + 5 * 2 * 4);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[36..40], b"data");
        let (audio, rate) = read_from(&mut bytes.as_slice()).unwrap();
        assert_eq!(rate, SampleRate::CD);
        assert_eq!(audio.channels(), 2);
        assert_eq!(audio.channel(0), fixture().channel(0));
        assert_eq!(audio.channel(1), fixture().channel(1));
    }

    #[test]
    fn pcm16_round_trip_is_close_and_saturates() {
        let mut src = fixture();
        src.channel_mut(0)[3] = 2.0; // over
        let mut bytes = Vec::new();
        write_to(&mut bytes, &src, SampleRate::HZ_48K, WavFormat::Pcm16).unwrap();
        assert_eq!(bytes.len(), 44 + 5 * 2 * 2);
        let (audio, _) = read_from(&mut bytes.as_slice()).unwrap();
        assert!((audio.channel(0)[3] - 1.0).abs() < 1e-3);
        assert_eq!(audio.channel(0)[4], -1.0);
        for (a, b) in audio.channel(1).iter().zip(fixture().channel(1)) {
            assert!((a - b).abs() < 1e-4);
        }
    }

    #[test]
    fn file_round_trip() {
        let dir = std::env::temp_dir().join(format!("superb-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.wav");
        write(&path, &fixture(), SampleRate::HZ_96K, WavFormat::Float32).unwrap();
        let (audio, rate) = read(&path).unwrap();
        assert_eq!(rate, SampleRate::HZ_96K);
        assert_eq!(audio.frames(), 5);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(read(dir.join("missing.wav")).is_err());
    }

    #[test]
    fn rejects_garbage_and_unsupported() {
        assert!(read_from(&mut &b"nope"[..]).is_err());
        assert!(read_from(&mut &b"RIFF\0\0\0\0WAVE"[..]).is_err());
        let mut bytes = Vec::new();
        write_to(&mut bytes, &fixture(), SampleRate::CD, WavFormat::Pcm16).unwrap();
        bytes[34] = 24; // bits per sample
        assert!(read_from(&mut bytes.as_slice()).is_err());
        assert!(write_to(
            &mut Vec::new(),
            &PlanarBuffer::new(0, 4),
            SampleRate::CD,
            WavFormat::Pcm16
        )
        .is_err());
    }
}
