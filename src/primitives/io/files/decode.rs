//! Container probing and packet decoding through symphonia.
//!
//! **MPL-2.0 boundary: symphonia is confined to decode.rs.** This is the
//! only Aura source file that may `use symphonia`; everything it exports
//! is expressed in Aura types so callers (and the rest of this crate)
//! never touch an MPL-licensed type. Keep it that way — `lib.rs` has a test
//! that scans the crate for stray imports.

use std::fs::File;
use std::io::{Cursor, Read};
use std::path::Path;

use symphonia::core::audio::sample::SampleFormat;
use symphonia::core::audio::GenericAudioBufferRef;
use symphonia::core::codecs::audio::well_known as codec;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::CodecParameters;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{
    MediaSource, MediaSourceStream, MediaSourceStreamOptions, ReadOnlySource,
};
use symphonia::core::meta::MetadataOptions;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::{AuraError, Result, SampleRate};

use super::AudioFile;

fn err(context: &str, e: impl std::fmt::Display) -> AuraError {
    AuraError::Message(format!("{context}: {e}"))
}

pub(super) fn decode_path(path: &Path) -> Result<AudioFile> {
    let file = File::open(path).map_err(|e| err("open audio file", e))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    decode_source(Box::new(file), &hint)
}

pub(super) fn decode_reader<R: Read + Send + Sync>(
    reader: R,
    extension_hint: Option<&str>,
) -> Result<AudioFile> {
    decode_source(
        Box::new(ReadOnlySource::new(reader)),
        &hint_from(extension_hint),
    )
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
        .probe(
            hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| err("unrecognised audio container", e))?;

    let track = reader
        .default_track(TrackType::Audio)
        .ok_or_else(|| AuraError::Message("no audio track in file".into()))?;
    let track_id = track.id;
    let params = match &track.codec_params {
        Some(CodecParameters::Audio(p)) => p.clone(),
        _ => {
            return Err(AuraError::Message(
                "audio track has no usable codec parameters".into(),
            ))
        }
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
            Err(SymError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof && packets > 0 =>
            {
                break
            }
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
            planes = scratch
                .iter()
                .map(|p| Vec::with_capacity(declared_frames.unwrap_or(p.len())))
                .collect();
        } else if planes.len() != scratch.len() {
            return Err(AuraError::Message(format!(
                "channel count changed mid-stream ({} -> {})",
                planes.len(),
                scratch.len()
            )));
        }
        for (dst, src) in planes.iter_mut().zip(&scratch) {
            dst.extend_from_slice(src);
        }
    }

    let hz =
        rate_hz.ok_or_else(|| AuraError::Message("audio file declares no sample rate".into()))?;
    let sample_rate = SampleRate::new(hz)?;
    if planes.is_empty() {
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count())
            .unwrap_or(1)
            .max(1);
        return Ok(AudioFile {
            buffer: PlanarBuffer::new(channels, 0),
            sample_rate,
            bits_per_sample: bits_in(&params),
        });
    }
    let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
    Ok(AudioFile {
        buffer: PlanarBuffer::from_planes(&refs)?,
        sample_rate,
        bits_per_sample: bits_in(&params),
    })
}

/// Bits per sample a codec's sample format implies, when it gives no count
/// (float WAVs say F32 and nothing else).
fn bits_of(format: SampleFormat) -> u32 {
    match format {
        SampleFormat::U8 | SampleFormat::S8 => 8,
        SampleFormat::U16 | SampleFormat::S16 => 16,
        SampleFormat::U24 | SampleFormat::S24 => 24,
        SampleFormat::U32 | SampleFormat::S32 | SampleFormat::F32 => 32,
        SampleFormat::F64 => 64,
    }
}

/// The depth a file's samples were stored at: what the codec parameters
/// say, or what the sample format or the PCM codec itself implies (a float
/// WAV says only that it is PCM_F32). `None` for lossy codecs.
fn bits_in(params: &symphonia::core::codecs::audio::AudioCodecParameters) -> Option<u32> {
    params
        .bits_per_sample
        .or(params.bits_per_coded_sample)
        .or_else(|| params.sample_format.map(bits_of))
        .or_else(|| bits_of_codec(params.codec))
}

/// Bits per sample a PCM codec id implies.
fn bits_of_codec(id: symphonia::core::codecs::audio::AudioCodecId) -> Option<u32> {
    use codec::*;
    let bits = match id {
        CODEC_ID_PCM_S8
        | CODEC_ID_PCM_S8_PLANAR
        | CODEC_ID_PCM_U8
        | CODEC_ID_PCM_U8_PLANAR
        | CODEC_ID_PCM_ALAW
        | CODEC_ID_PCM_MULAW => 8,
        CODEC_ID_PCM_S16LE
        | CODEC_ID_PCM_S16LE_PLANAR
        | CODEC_ID_PCM_S16BE
        | CODEC_ID_PCM_S16BE_PLANAR
        | CODEC_ID_PCM_U16LE
        | CODEC_ID_PCM_U16LE_PLANAR
        | CODEC_ID_PCM_U16BE
        | CODEC_ID_PCM_U16BE_PLANAR => 16,
        CODEC_ID_PCM_S24LE
        | CODEC_ID_PCM_S24LE_PLANAR
        | CODEC_ID_PCM_S24BE
        | CODEC_ID_PCM_S24BE_PLANAR
        | CODEC_ID_PCM_U24LE
        | CODEC_ID_PCM_U24LE_PLANAR
        | CODEC_ID_PCM_U24BE
        | CODEC_ID_PCM_U24BE_PLANAR => 24,
        CODEC_ID_PCM_S32LE
        | CODEC_ID_PCM_S32LE_PLANAR
        | CODEC_ID_PCM_S32BE
        | CODEC_ID_PCM_S32BE_PLANAR
        | CODEC_ID_PCM_U32LE
        | CODEC_ID_PCM_U32LE_PLANAR
        | CODEC_ID_PCM_U32BE
        | CODEC_ID_PCM_U32BE_PLANAR
        | CODEC_ID_PCM_F32LE
        | CODEC_ID_PCM_F32LE_PLANAR
        | CODEC_ID_PCM_F32BE
        | CODEC_ID_PCM_F32BE_PLANAR => 32,
        CODEC_ID_PCM_F64LE
        | CODEC_ID_PCM_F64LE_PLANAR
        | CODEC_ID_PCM_F64BE
        | CODEC_ID_PCM_F64BE_PLANAR => 64,
        _ => return None,
    };
    Some(bits)
}
