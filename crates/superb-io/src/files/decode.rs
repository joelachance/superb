//! Container probing and packet decoding through symphonia.
//!
//! **MPL-2.0 boundary: symphonia is confined to decode.rs.** This is the
//! only Superb source file that may `use symphonia`; everything it exports
//! is expressed in Superb types so callers (and the rest of this crate)
//! never touch an MPL-licensed type. Keep it that way — `lib.rs` has a test
//! that scans the workspace for stray imports.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource};
use symphonia::core::meta::MetadataOptions;

use superb_buffers::PlanarBuffer;
use superb_core::{Result, SampleRate, SuperbError};

use super::AudioFile;

fn err(context: &str, e: impl std::fmt::Display) -> SuperbError {
    SuperbError::Message(format!("{context}: {e}"))
}

pub(super) fn decode_path(path: &Path) -> Result<AudioFile> {
    let file = File::open(path).map_err(|e| err("open audio file", e))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    decode_source(Box::new(file), &hint)
}

pub(super) fn decode_reader<R: Read + Send + Sync>(reader: R, extension_hint: Option<&str>) -> Result<AudioFile> {
    decode_source(Box::new(ReadOnlySource::new(reader)), &hint_from(extension_hint))
}

pub(super) fn decode_vec(bytes: Vec<u8>, extension_hint: Option<&str>) -> Result<AudioFile> {
    decode_source(Box::new(Cursor::new(bytes)), &hint_from(extension_hint))
}

fn hint_from(extension: Option<&str>) -> Hint {
    let mut hint = Hint::new();
    if let Some(ext) = extension {
        hint.with_extension(ext);
    }
    hint
}

fn decode_source<'s>(source: Box<dyn MediaSource + 's>, hint: &Hint) -> Result<AudioFile> {
    let stream = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
    let mut reader: Box<dyn FormatReader + 's> = symphonia::default::get_probe()
        .probe(hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| err("unrecognised audio container", e))?;

    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| SuperbError::Message("no audio track in file".into()))?;
    let track_id = track.id;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => return Err(SuperbError::Message("audio track has no usable codec parameters".into())),
    };
    let declared_rate = params.sample_rate;
    let declared_frames = track.num_frames.map(|n| n as usize);

    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| err("unsupported audio codec", e))?;

    // Planes are appended packet by packet; `scratch` is reused for the
    // per-packet planar copy so the only growth is the final planes.
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut scratch: Vec<Vec<f32>> = Vec::new();
    let mut rate_hz: Option<u32> = declared_rate;
    let mut packets = 0usize;

    loop {
        let packet = match reader.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof && packets > 0 => break,
            Err(SymError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(err("reading audio packet", e)),
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded: GenericAudioBufferRef<'_> = match decoder.decode(&packet) {
            Ok(buf) => buf,
            Err(SymError::DecodeError(_)) => continue, // corrupt packet: skip, keep going
            Err(SymError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(err("decoding audio packet", e)),
        };
        packets += 1;
        if decoded.frames() == 0 {
            continue;
        }
        if rate_hz.is_none() {
            rate_hz = Some(decoded.spec().rate());
        }
        decoded.copy_to_vecs_planar::<f32>(&mut scratch);
        if planes.is_empty() {
            planes = scratch.iter().map(|p| Vec::with_capacity(declared_frames.unwrap_or(p.len()))).collect();
        } else if planes.len() != scratch.len() {
            return Err(SuperbError::Message(format!(
                "channel count changed mid-stream ({} -> {})",
                planes.len(),
                scratch.len()
            )));
        }
        for (dst, src) in planes.iter_mut().zip(&scratch) {
            dst.extend_from_slice(src);
        }
    }

    let hz = rate_hz.ok_or_else(|| SuperbError::Message("audio file declares no sample rate".into()))?;
    let sample_rate = SampleRate::new(hz)?;
    if planes.is_empty() {
        let channels = params.channels.map(|c| c.count()).unwrap_or(1).max(1);
        return Ok(AudioFile { buffer: PlanarBuffer::new(channels, 0), sample_rate });
    }
    let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
    Ok(AudioFile { buffer: PlanarBuffer::from_planes(&refs)?, sample_rate })
}
