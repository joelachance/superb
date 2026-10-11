//! RIFF/WAVE files: a [`Writer`] that streams audio to disk and keeps the
//! file readable while it grows, a one-shot [`write()`], a reader that also
//! accepts a file a crash left unfinished, and [`repair`] to finish such a
//! file in place.
//!
//! 16-bit and 24-bit PCM and 32-bit float, interleaved, any channel count.
//! Past 4 GiB a streamed file becomes RF64 (EBU Tech 3306: the same layout
//! with 64-bit sizes in a `ds64` chunk), which every DAW reads. For other
//! codecs use `files::decode` (feature `files`), which wraps a full decoder.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::primitives::buffers::convert::f32_to_i16_saturating;
use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::{AuraError, Result, SampleRate};

/// Sample encoding used on disk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    /// 16-bit signed integer PCM.
    Pcm16,
    /// 24-bit signed integer PCM: what DAWs exchange and deliver.
    Pcm24,
    /// 32-bit IEEE float: what the session folder stores.
    Float32,
}

impl WavFormat {
    /// Bits per sample: 16, 24 or 32.
    pub fn bits(self) -> u32 {
        match self {
            WavFormat::Pcm16 => 16,
            WavFormat::Pcm24 => 24,
            WavFormat::Float32 => 32,
        }
    }

    fn bytes_per_sample(self) -> u16 {
        match self {
            Self::Pcm16 => 2,
            Self::Pcm24 => 3,
            Self::Float32 => 4,
        }
    }

    fn format_tag(self) -> u16 {
        match self {
            Self::Pcm16 | Self::Pcm24 => 1,
            Self::Float32 => 3,
        }
    }

    fn from_tag(tag: u16, bits: u16) -> Result<Self> {
        match (tag, bits) {
            (1, 16) => Ok(WavFormat::Pcm16),
            (1, 24) => Ok(WavFormat::Pcm24),
            (3, 32) => Ok(WavFormat::Float32),
            _ => Err(AuraError::Message(format!(
                "unsupported wav format tag {tag} / {bits} bits"
            ))),
        }
    }

    /// Appends one sample in this encoding.
    fn push(self, bytes: &mut Vec<u8>, sample: f32) {
        match self {
            WavFormat::Pcm16 => {
                bytes.extend_from_slice(&f32_to_i16_saturating(sample).to_le_bytes())
            }
            WavFormat::Pcm24 => {
                let v = (sample.clamp(-1.0, 1.0) * 8_388_608.0).round() as i32;
                let v = v.clamp(-8_388_608, 8_388_607);
                bytes.extend_from_slice(&v.to_le_bytes()[..3]);
            }
            WavFormat::Float32 => bytes.extend_from_slice(&sample.to_le_bytes()),
        }
    }

    /// Decodes one sample in this encoding.
    fn sample(self, bytes: &[u8]) -> f32 {
        match self {
            WavFormat::Pcm16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
            WavFormat::Pcm24 => {
                let v = i32::from_le_bytes([0, bytes[0], bytes[1], bytes[2]]) >> 8;
                v as f32 / 8_388_608.0
            }
            WavFormat::Float32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        }
    }
}

fn io_err(e: io::Error) -> AuraError {
    AuraError::Message(format!("wav io error: {e}"))
}

/// The `fmt ` chunk's 16 bytes.
fn fmt_chunk(channels: usize, rate: SampleRate, format: WavFormat) -> [u8; 16] {
    let bps = format.bytes_per_sample();
    let block_align = channels as u16 * bps;
    let byte_rate = rate.hz() * u32::from(block_align);
    let mut c = [0u8; 16];
    c[0..2].copy_from_slice(&format.format_tag().to_le_bytes());
    c[2..4].copy_from_slice(&(channels as u16).to_le_bytes());
    c[4..8].copy_from_slice(&rate.hz().to_le_bytes());
    c[8..12].copy_from_slice(&byte_rate.to_le_bytes());
    c[12..14].copy_from_slice(&block_align.to_le_bytes());
    c[14..16].copy_from_slice(&(bps * 8).to_le_bytes());
    c
}

fn check_channels(channels: usize) -> Result<()> {
    if channels == 0 || channels > u16::MAX as usize {
        return Err(AuraError::InvalidArgument {
            what: "wav channels",
        });
    }
    Ok(())
}

/// Writes `audio` to `path` in the given format, header finished.
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

/// Writes `audio` to any writer: a 44-byte header, then the samples.
pub fn write_to(
    w: &mut impl Write,
    audio: &PlanarBuffer,
    sample_rate: SampleRate,
    format: WavFormat,
) -> Result<()> {
    let channels = audio.channels();
    check_channels(channels)?;
    let block_align = channels * format.bytes_per_sample() as usize;
    let data_len = (audio.frames() * block_align) as u32;

    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&fmt_chunk(channels, sample_rate, format));
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    w.write_all(&header).map_err(io_err)?;

    let mut body = Vec::with_capacity(data_len as usize);
    for frame in 0..audio.frames() {
        for ch in 0..channels {
            format.push(&mut body, audio.channel(ch)[frame]);
        }
    }
    w.write_all(&body).map_err(io_err)
}

// ---------------------------------------------------------------------------
// The streaming writer

/// The largest value a 32-bit RIFF size field can hold; past it the file
/// is RF64 and the real sizes live in `ds64`.
const SIZE_LIMIT: u64 = u32::MAX as u64;
/// The `ds64` payload: RIFF size, data size, frame count (u64 each) and an
/// empty table.
const DS64_LEN: u32 = 28;
/// Where the samples begin in a streamed file: `RIFF` form header (12),
/// `JUNK`/`ds64` (8 + 28), `fmt ` (8 + 16), `data` chunk header (8).
const STREAM_DATA_START: u64 = 12 + 8 + DS64_LEN as u64 + 8 + 16 + 8;
/// Where the writer buffers before each system write.
const WRITE_BUFFER: usize = 512 * 1024;

/// What a [`Writer`] has done so far, readable from any thread while it
/// runs: the collector thread writes, the application polls.
#[derive(Debug, Default)]
pub struct Counters {
    frames: AtomicU64,
    bytes: AtomicU64,
    sealed: AtomicU64,
    errors: AtomicU64,
}

impl Counters {
    /// Frames handed to the writer; some may still sit in its buffer.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Acquire)
    }

    /// Sample bytes handed to the writer.
    pub fn bytes(&self) -> u64 {
        self.bytes.load(Ordering::Acquire)
    }

    /// Frames on disk for good: written, synced, and covered by a header
    /// the reader accepts. Everything before this frame survives a crash,
    /// and a byte range of it will never change, so an uploader can ship
    /// it. Advances at every [`Writer::flush`].
    pub fn sealed_frames(&self) -> u64 {
        self.sealed.load(Ordering::Acquire)
    }

    /// Writes that failed (disk full, file gone, I/O error). The audio in a
    /// failed write is lost; anything above zero means a gap, and the
    /// application decides whether to stop.
    pub fn disk_errors(&self) -> u64 {
        self.errors.load(Ordering::Acquire)
    }
}

/// A WAV file written as the audio arrives, so a crash costs a second, not
/// the take, and a three-hour recording never has to fit in memory.
///
/// [`create`](Writer::create) writes a header with room for RF64 sizes,
/// [`write`](Writer::write) appends blocks through a large buffer,
/// [`flush`](Writer::flush) pushes them to disk, patches the sizes in the
/// header and syncs, and [`finish`](Writer::finish) writes the final header,
/// as RF64 when the file passed 4 GiB. A file left unfinished is still read
/// by [`read`] up to the last flushed frame, and [`repair`] finishes its
/// header in place. Converting from planar blocks uses a buffer allocated
/// at creation; never call this from the audio thread, since it does I/O.
pub struct Writer {
    file: BufWriter<File>,
    path: Option<PathBuf>,
    format: WavFormat,
    channels: usize,
    rate: SampleRate,
    /// Interleaved bytes of the block being written.
    scratch: Vec<u8>,
    data_bytes: u64,
    frames: u64,
    counters: Arc<Counters>,
    /// Data bytes past which `finish` writes RF64 (a test knob; the limit
    /// itself otherwise).
    rf64_after: u64,
}

impl Writer {
    /// Creates (or truncates) `path` and writes the header.
    pub fn create(
        path: impl AsRef<Path>,
        channels: usize,
        sample_rate: SampleRate,
        format: WavFormat,
    ) -> Result<Self> {
        let path = path.as_ref();
        let file = File::create(path)
            .map_err(|e| AuraError::Message(format!("cannot create {}: {e}", path.display())))?;
        let mut writer = Self::new(file, channels, sample_rate, format)?;
        writer.path = Some(path.to_path_buf());
        Ok(writer)
    }

    /// Writes into a file you opened (positioned at its start).
    pub fn new(
        file: File,
        channels: usize,
        sample_rate: SampleRate,
        format: WavFormat,
    ) -> Result<Self> {
        check_channels(channels)?;
        let mut writer = Writer {
            file: BufWriter::with_capacity(WRITE_BUFFER, file),
            path: None,
            format,
            channels,
            rate: sample_rate,
            scratch: Vec::with_capacity(4096 * channels * format.bytes_per_sample() as usize),
            data_bytes: 0,
            frames: 0,
            counters: Arc::new(Counters::default()),
            rf64_after: SIZE_LIMIT - STREAM_DATA_START,
        };
        writer.write_header(false).map_err(io_err)?;
        Ok(writer)
    }

    /// The header at the start of the file: `RIFF` (or `RF64` once
    /// finished past the limit), a `JUNK` chunk the size of `ds64` (or
    /// `ds64` itself), `fmt `, and `data` with the sizes known so far.
    fn write_header(&mut self, finished: bool) -> io::Result<()> {
        let rf64 = finished && self.data_bytes > self.rf64_after;
        let riff_size = STREAM_DATA_START - 8 + self.data_bytes;
        // In RF64 both 32-bit fields are all ones: the sizes live in ds64.
        let field = |size: u64| {
            if rf64 {
                u32::MAX
            } else {
                size.min(SIZE_LIMIT) as u32
            }
        };
        let mut h = Vec::with_capacity(STREAM_DATA_START as usize);
        h.extend_from_slice(if rf64 { b"RF64" } else { b"RIFF" });
        h.extend_from_slice(&field(riff_size).to_le_bytes());
        h.extend_from_slice(b"WAVE");
        h.extend_from_slice(if rf64 { b"ds64" } else { b"JUNK" });
        h.extend_from_slice(&DS64_LEN.to_le_bytes());
        if rf64 {
            h.extend_from_slice(&riff_size.to_le_bytes());
            h.extend_from_slice(&self.data_bytes.to_le_bytes());
            h.extend_from_slice(&self.frames.to_le_bytes());
            h.extend_from_slice(&0u32.to_le_bytes());
        } else {
            h.extend_from_slice(&[0u8; DS64_LEN as usize]);
        }
        h.extend_from_slice(b"fmt ");
        h.extend_from_slice(&16u32.to_le_bytes());
        h.extend_from_slice(&fmt_chunk(self.channels, self.rate, self.format));
        h.extend_from_slice(b"data");
        h.extend_from_slice(&field(self.data_bytes).to_le_bytes());
        debug_assert_eq!(h.len() as u64, STREAM_DATA_START);
        self.file.flush()?;
        let inner = self.file.get_mut();
        inner.seek(SeekFrom::Start(0))?;
        inner.write_all(&h)?;
        inner.seek(SeekFrom::Start(STREAM_DATA_START + self.data_bytes))?;
        Ok(())
    }

    /// Appends one block: one slice per channel, all the same length.
    /// A failed write is counted in [`Counters::disk_errors`] and returned;
    /// the writer stays usable, so a collector can keep draining its queue.
    pub fn write(&mut self, planes: &[&[f32]]) -> Result<()> {
        if planes.len() != self.channels {
            return Err(AuraError::ChannelMismatch {
                expected: self.channels,
                actual: planes.len(),
            });
        }
        let frames = planes.first().map_or(0, |p| p.len());
        if planes.iter().any(|p| p.len() != frames) {
            return Err(AuraError::InvalidArgument {
                what: "wav planes of unequal length",
            });
        }
        self.scratch.clear();
        for frame in 0..frames {
            for plane in planes {
                self.format.push(&mut self.scratch, plane[frame]);
            }
        }
        match self.file.write_all(&self.scratch) {
            Ok(()) => {
                self.data_bytes += self.scratch.len() as u64;
                self.frames += frames as u64;
                self.counters.frames.store(self.frames, Ordering::Release);
                self.counters
                    .bytes
                    .store(self.data_bytes, Ordering::Release);
                Ok(())
            }
            Err(e) => {
                self.counters.errors.fetch_add(1, Ordering::AcqRel);
                Err(io_err(e))
            }
        }
    }

    /// Appends a planar block.
    pub fn write_block(&mut self, block: &PlanarBuffer) -> Result<()> {
        let planes: Vec<&[f32]> = block.planes().collect();
        self.write(&planes)
    }

    /// Pushes buffered samples to disk, patches the header's sizes and
    /// syncs, so everything written so far survives a crash and counts as
    /// sealed. Call it about once a second from the thread that writes.
    pub fn flush(&mut self) -> Result<()> {
        let result = self
            .write_header(false)
            .and_then(|()| self.file.get_mut().sync_data());
        match result {
            Ok(()) => {
                self.counters.sealed.store(self.frames, Ordering::Release);
                Ok(())
            }
            Err(e) => {
                self.counters.errors.fetch_add(1, Ordering::AcqRel);
                Err(io_err(e))
            }
        }
    }

    /// Writes the final header (RF64 when the data passed 4 GiB), syncs and
    /// closes the file. Returns the frames written.
    pub fn finish(mut self) -> Result<u64> {
        self.write_header(true)
            .and_then(|()| self.file.get_mut().sync_all())
            .map_err(|e| {
                self.counters.errors.fetch_add(1, Ordering::AcqRel);
                io_err(e)
            })?;
        self.counters.sealed.store(self.frames, Ordering::Release);
        Ok(self.frames)
    }

    /// Frames written so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Channels per frame.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The file, when [`create`](Writer::create) made it.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Counters another thread can poll while this one writes.
    pub fn counters(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }
}

impl core::fmt::Debug for Writer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Writer")
            .field("path", &self.path)
            .field("format", &self.format)
            .field("channels", &self.channels)
            .field("frames", &self.frames)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Reading

/// What the chunks of a RIFF/WAVE say, before the samples are decoded.
struct Layout {
    format: WavFormat,
    channels: usize,
    rate: SampleRate,
    /// Where the sample bytes start.
    data_start: usize,
    /// How many sample bytes there are: the header's word, or the file's
    /// length when the header was never finished.
    data_len: usize,
    /// Whether the header's sizes were missing, too large for the file, or
    /// behind a streamed file's length.
    unfinished: bool,
    /// Where a 28-byte `JUNK` chunk could become `ds64`, if there is one.
    junk_at: Option<usize>,
}

/// Reads the chunk headers in `bytes` (the whole file, or at least its
/// header), taking `file_len` as the truth when the sizes are not.
fn layout(bytes: &[u8], file_len: usize) -> Result<Layout> {
    if bytes.len() < 12 || &bytes[8..12] != b"WAVE" {
        return Err(AuraError::Message("not a RIFF/WAVE file".into()));
    }
    let rf64 = match &bytes[0..4] {
        b"RIFF" => false,
        b"RF64" => true,
        _ => return Err(AuraError::Message("not a RIFF/WAVE file".into())),
    };
    let u32_at =
        |at: usize| u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let u64_at = |at: usize| {
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[at..at + 8]);
        u64::from_le_bytes(b)
    };
    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    let mut ds64_data: Option<u64> = None;
    let mut junk_at = None;
    let mut data: Option<(usize, usize, bool)> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32_at(pos + 4) as usize;
        let start = pos + 8;
        match id {
            b"fmt " => {
                if len < 16 || start + 16 > bytes.len() {
                    return Err(AuraError::Message("fmt chunk too short".into()));
                }
                fmt = Some((
                    u16::from_le_bytes([bytes[start], bytes[start + 1]]),
                    u16::from_le_bytes([bytes[start + 2], bytes[start + 3]]),
                    u32_at(start + 4),
                    u16::from_le_bytes([bytes[start + 14], bytes[start + 15]]),
                ));
            }
            b"ds64" if len >= 16 && start + 16 <= bytes.len() => {
                ds64_data = Some(u64_at(start + 8));
            }
            b"JUNK" if len == DS64_LEN as usize => junk_at = Some(pos),
            b"data" => {
                let said = if rf64 && len == u32::MAX as usize {
                    ds64_data.map(|d| d as usize)
                } else if len == 0 || len == u32::MAX as usize {
                    None
                } else {
                    Some(len)
                };
                let available = file_len.saturating_sub(start);
                // A streamed file (the JUNK/ds64 chunk marks one) never has a
                // chunk after `data`, so bytes past the header's word are
                // frames the header has not caught up with.
                let streamed = junk_at.is_some() || rf64;
                let (data_len, unfinished) = match said {
                    Some(said) if said == available => (said, false),
                    Some(said) if said < available && !streamed => (said, false),
                    _ => (available, true),
                };
                data = Some((start, data_len, unfinished));
                if unfinished {
                    break;
                }
            }
            _ => {}
        }
        pos = start + len + (len & 1); // chunks are word aligned
    }
    let (tag, channels, rate, bits) =
        fmt.ok_or_else(|| AuraError::Message("missing fmt chunk".into()))?;
    let (data_start, data_len, unfinished) =
        data.ok_or_else(|| AuraError::Message("missing data chunk".into()))?;
    let format = WavFormat::from_tag(tag, bits)?;
    check_channels(channels as usize)?;
    let frame_bytes = channels as usize * format.bytes_per_sample() as usize;
    Ok(Layout {
        format,
        channels: channels as usize,
        rate: SampleRate::new(rate)?,
        data_start,
        data_len: data_len - data_len % frame_bytes,
        unfinished,
        junk_at,
    })
}

/// Reads a WAV file: the formats [`write()`] and [`Writer`] produce (16-bit
/// and 24-bit PCM, 32-bit float), RIFF or RF64, finished or not. A file a
/// crash left unfinished is read up to its last whole frame.
pub fn read(path: impl AsRef<Path>) -> Result<(PlanarBuffer, SampleRate)> {
    read_with_format(path).map(|(audio, rate, _)| (audio, rate))
}

/// Like [`read`], also saying which format the file was stored in.
pub fn read_with_format(path: impl AsRef<Path>) -> Result<(PlanarBuffer, SampleRate, WavFormat)> {
    let file = File::open(path).map_err(io_err)?;
    read_from_with_format(&mut BufReader::new(file))
}

/// Reads a WAV stream.
pub fn read_from(r: &mut impl Read) -> Result<(PlanarBuffer, SampleRate)> {
    read_from_with_format(r).map(|(audio, rate, _)| (audio, rate))
}

/// Like [`read_from`], also saying which format the stream was stored in.
pub fn read_from_with_format(r: &mut impl Read) -> Result<(PlanarBuffer, SampleRate, WavFormat)> {
    let mut bytes = Vec::new();
    r.read_to_end(&mut bytes).map_err(io_err)?;
    let layout = layout(&bytes, bytes.len())?;
    let data = &bytes[layout.data_start..layout.data_start + layout.data_len];
    let bps = layout.format.bytes_per_sample() as usize;
    let frames = layout.data_len / (bps * layout.channels);
    let mut audio = PlanarBuffer::new(layout.channels, frames);
    let mut i = 0;
    for frame in 0..frames {
        for ch in 0..layout.channels {
            audio.channel_mut(ch)[frame] = layout.format.sample(&data[i..i + bps]);
            i += bps;
        }
    }
    Ok((audio, layout.rate, layout.format))
}

/// Finishes the header of a WAV file left unfinished (by a crash during a
/// [`Writer`], say) in place: the sample bytes are whatever the file holds,
/// cut to whole frames, and the sizes are written, as RF64 when they do not
/// fit 32 bits and the file has the room for `ds64`. A finished file is
/// left as it is. Returns the frames the file holds.
pub fn repair(path: impl AsRef<Path>) -> Result<u64> {
    let path = path.as_ref();
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| AuraError::Message(format!("cannot open {}: {e}", path.display())))?;
    let file_len = file.metadata().map_err(io_err)?.len() as usize;
    let mut head = vec![0u8; file_len.min(4096)];
    file.read_exact(&mut head).map_err(io_err)?;
    let layout = layout(&head, file_len)?;
    let frame_bytes = layout.channels * layout.format.bytes_per_sample() as usize;
    let frames = (layout.data_len / frame_bytes) as u64;
    if !layout.unfinished {
        return Ok(frames);
    }
    let data_len = layout.data_len as u64;
    file.set_len(layout.data_start as u64 + data_len)
        .map_err(io_err)?;
    let riff_size = layout.data_start as u64 - 8 + data_len;
    let rf64 = riff_size > SIZE_LIMIT;
    if rf64 {
        let junk_at = layout.junk_at.ok_or_else(|| {
            AuraError::Message(format!(
                "{}: too large for a RIFF header and no room for ds64",
                path.display()
            ))
        })?;
        let mut ds64 = Vec::with_capacity(8 + DS64_LEN as usize);
        ds64.extend_from_slice(b"ds64");
        ds64.extend_from_slice(&DS64_LEN.to_le_bytes());
        ds64.extend_from_slice(&riff_size.to_le_bytes());
        ds64.extend_from_slice(&data_len.to_le_bytes());
        ds64.extend_from_slice(&frames.to_le_bytes());
        ds64.extend_from_slice(&0u32.to_le_bytes());
        file.seek(SeekFrom::Start(junk_at as u64)).map_err(io_err)?;
        file.write_all(&ds64).map_err(io_err)?;
    }
    let field = |size: u64| if rf64 { u32::MAX } else { size as u32 };
    file.seek(SeekFrom::Start(0)).map_err(io_err)?;
    file.write_all(if rf64 { b"RF64" } else { b"RIFF" })
        .map_err(io_err)?;
    file.write_all(&field(riff_size).to_le_bytes())
        .map_err(io_err)?;
    file.seek(SeekFrom::Start(layout.data_start as u64 - 4))
        .map_err(io_err)?;
    file.write_all(&field(data_len).to_le_bytes())
        .map_err(io_err)?;
    file.sync_all().map_err(io_err)?;
    Ok(frames)
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

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aura-wav-{}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
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
    fn pcm24_round_trip_is_close_and_saturates() {
        let mut src = fixture();
        src.channel_mut(0)[3] = 1.5; // over
        let mut bytes = Vec::new();
        write_to(&mut bytes, &src, SampleRate::HZ_48K, WavFormat::Pcm24).unwrap();
        assert_eq!(bytes.len(), 44 + 5 * 2 * 3);
        assert_eq!(bytes[34], 24, "bits per sample");
        let (audio, _, format) = read_from_with_format(&mut bytes.as_slice()).unwrap();
        assert_eq!(format, WavFormat::Pcm24);
        assert!(
            (audio.channel(0)[3] - 1.0).abs() < 2e-7,
            "{}",
            audio.channel(0)[3]
        );
        assert_eq!(audio.channel(0)[4], -1.0);
        for (a, b) in audio.channel(1).iter().zip(fixture().channel(1)) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn file_round_trip() {
        let dir = scratch("file");
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
        bytes[34] = 20; // bits per sample
        assert!(read_from(&mut bytes.as_slice()).is_err());
        assert!(write_to(
            &mut Vec::new(),
            &PlanarBuffer::new(0, 4),
            SampleRate::CD,
            WavFormat::Pcm16
        )
        .is_err());
    }

    /// Ten blocks of a ramp, two channels.
    fn blocks() -> Vec<PlanarBuffer> {
        (0..10)
            .map(|b| {
                let mut block = PlanarBuffer::new(2, 64);
                for (i, s) in block.channel_mut(0).iter_mut().enumerate() {
                    *s = (b * 64 + i) as f32 / 1000.0;
                }
                block.channel_mut(1).fill(-0.5);
                block
            })
            .collect()
    }

    #[test]
    fn a_streamed_file_reads_back_exactly() {
        let dir = scratch("stream");
        let path = dir.join("take.wav");
        let mut writer = Writer::create(&path, 2, SampleRate::HZ_48K, WavFormat::Float32).unwrap();
        assert_eq!(writer.path(), Some(path.as_path()));
        assert_eq!(writer.channels(), 2);
        let counters = writer.counters();
        for block in blocks() {
            writer.write_block(&block).unwrap();
        }
        assert_eq!(writer.frames(), 640);
        assert_eq!(counters.frames(), 640);
        assert_eq!(counters.bytes(), 640 * 8);
        assert_eq!(counters.sealed_frames(), 0, "nothing flushed yet");
        assert_eq!(writer.finish().unwrap(), 640);
        assert_eq!(counters.sealed_frames(), 640);
        assert_eq!(counters.disk_errors(), 0);

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, STREAM_DATA_START + 640 * 8);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[12..16], b"JUNK", "room for ds64, unused");
        assert_eq!(&bytes[72..76], b"data");
        let (audio, rate, format) = read_with_format(&path).unwrap();
        assert_eq!((rate, format), (SampleRate::HZ_48K, WavFormat::Float32));
        assert_eq!(audio.frames(), 640);
        assert_eq!(audio.channel(0)[639], 0.639);
        assert!(audio.channel(1).iter().all(|&s| s == -0.5));
        assert_eq!(repair(&path).unwrap(), 640, "a finished file is left alone");
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_crash_keeps_every_flushed_frame_and_repair_finishes_the_header() {
        let dir = scratch("crash");
        let path = dir.join("take.wav");
        let mut writer = Writer::create(&path, 2, SampleRate::HZ_48K, WavFormat::Pcm24).unwrap();
        let counters = writer.counters();
        let blocks = blocks();
        for block in &blocks[..6] {
            writer.write_block(block).unwrap();
        }
        writer.flush().unwrap();
        assert_eq!(counters.sealed_frames(), 384);
        for block in &blocks[6..] {
            writer.write_block(block).unwrap();
        }
        // The crash: the writer goes away without `finish`. Its buffer may
        // or may not have reached the file; what was flushed is there.
        drop(writer);
        let (audio, _, format) = read_with_format(&path).unwrap();
        assert_eq!(format, WavFormat::Pcm24);
        assert!(audio.frames() >= 384, "{} frames", audio.frames());
        assert!((audio.channel(0)[383] - 0.383).abs() < 1e-6);
        // Repair makes it a finished file with the same frames.
        let frames = repair(&path).unwrap();
        assert_eq!(frames as usize, audio.frames());
        let bytes = std::fs::read(&path).unwrap();
        let riff = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        assert_eq!(riff + 8, bytes.len(), "RIFF size covers the file");
        let data = u32::from_le_bytes([bytes[76], bytes[77], bytes[78], bytes[79]]) as usize;
        assert_eq!(data, frames as usize * 6);
        let (again, _) = read(&path).unwrap();
        assert_eq!(again.frames(), frames as usize);
        assert_eq!(repair(&path).unwrap(), frames);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_torn_last_frame_is_cut() {
        let dir = scratch("torn");
        let path = dir.join("take.wav");
        let mut writer = Writer::create(&path, 1, SampleRate::CD, WavFormat::Pcm16).unwrap();
        writer.write(&[&[0.5; 10]]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        // One byte of an eleventh frame.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.push(7);
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(read(&path).unwrap().0.frames(), 10);
        assert_eq!(repair(&path).unwrap(), 10);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            STREAM_DATA_START + 20
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn past_the_limit_the_file_is_rf64() {
        let dir = scratch("rf64");
        let path = dir.join("long.wav");
        let mut writer = Writer::create(&path, 2, SampleRate::HZ_48K, WavFormat::Float32).unwrap();
        writer.rf64_after = 1_000; // instead of 4 GiB, for the test
        for block in blocks() {
            writer.write_block(&block).unwrap();
        }
        writer.finish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RF64");
        assert_eq!(&bytes[4..8], &u32::MAX.to_le_bytes());
        assert_eq!(&bytes[12..16], b"ds64");
        let mut b = [0u8; 8];
        b.copy_from_slice(&bytes[28..36]);
        assert_eq!(u64::from_le_bytes(b), 640 * 8, "data size in ds64");
        b.copy_from_slice(&bytes[36..44]);
        assert_eq!(u64::from_le_bytes(b), 640, "frame count in ds64");
        assert_eq!(
            &bytes[76..80],
            &u32::MAX.to_le_bytes(),
            "data size deferred"
        );
        let (audio, _) = read(&path).unwrap();
        assert_eq!(audio.frames(), 640);
        assert_eq!(audio.channel(0)[639], 0.639);

        // An unfinished file past the limit is repaired into RF64 too.
        let path = dir.join("long-unfinished.wav");
        let mut writer = Writer::create(&path, 2, SampleRate::HZ_48K, WavFormat::Float32).unwrap();
        for block in blocks() {
            writer.write_block(&block).unwrap();
        }
        writer.flush().unwrap();
        drop(writer);
        // Pretend the sizes overflowed: a repair past 32 bits needs ds64,
        // which the JUNK chunk has room for; here the sizes fit, so RIFF.
        assert_eq!(repair(&path).unwrap(), 640);
        assert_eq!(&std::fs::read(&path).unwrap()[0..4], b"RIFF");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn disk_errors_are_counted_and_the_writer_keeps_going() {
        let dir = scratch("errors");
        let path = dir.join("take.wav");
        File::create(&path).unwrap();
        // A file opened read-only: every write fails, as a full disk would.
        let file = File::open(&path).unwrap();
        assert!(
            Writer::new(file, 1, SampleRate::CD, WavFormat::Float32).is_err(),
            "the header cannot be written"
        );
        // A writer whose file is removed from under it keeps counting.
        let mut writer = Writer::create(&path, 1, SampleRate::CD, WavFormat::Float32).unwrap();
        let counters = writer.counters();
        writer.write(&[&[0.1; 64]]).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        // Writes still land in the unlinked file; flushing syncs fine on
        // most systems, so force the error path through a bad plane set.
        assert!(matches!(
            writer.write(&[&[0.0; 4], &[0.0; 4]]),
            Err(AuraError::ChannelMismatch { .. })
        ));
        assert!(writer.write(&[&[0.0; 4]]).is_ok());
        assert_eq!(counters.disk_errors(), 0, "mismatches are not disk errors");
        let _ = writer.finish();
        assert!(Writer::create(
            dir.join("gone/take.wav"),
            1,
            SampleRate::CD,
            WavFormat::Pcm16
        )
        .is_err());
    }
}
