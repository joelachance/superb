//! Input (capture) streams.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::{Result, SampleRate, SuperbError};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use super::capture::CaptureProducer;
use super::{deinterleave_from_device, stream_instant_us, validate_io_config, DeviceInfo};

/// Lists input devices of the default host.
pub fn input_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_owned());
    let devices = host
        .input_devices()
        .map_err(|e| SuperbError::Message(format!("enumerate devices: {e}")))?;
    Ok(devices
        .filter_map(|d| {
            let desc = d.description().ok()?;
            let name = desc.name().to_owned();
            let max_channels = d
                .supported_input_configs()
                .ok()
                .map(|cfgs| cfgs.map(|c| c.channels() as usize).max().unwrap_or(0))
                .unwrap_or(0);
            Some(DeviceInfo {
                is_default: default_name.as_deref() == Some(name.as_str()),
                name,
                max_channels,
            })
        })
        .collect())
}

/// How to open an input stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputConfig {
    /// Device name from [`input_devices`], or `None` for the default device.
    pub device_name: Option<String>,
    /// Requested sample rate.
    pub sample_rate: SampleRate,
    /// Requested channel count.
    pub channels: usize,
    /// Requested buffer size in frames, or `None` for the device default.
    pub buffer_frames: Option<u32>,
    /// Largest block the callback will ever deliver. Device callbacks larger
    /// than this are split into chunks.
    pub max_block_frames: usize,
}

impl Default for InputConfig {
    fn default() -> Self {
        // Ask for stereo; [`pick_supported_input`] falls back to mono when needed.
        Self {
            device_name: None,
            sample_rate: SampleRate::HZ_48K,
            channels: 2,
            buffer_frames: None,
            max_block_frames: 4096,
        }
    }
}

/// A running input (capture) stream. Dropping it stops the audio.
pub struct InputStream {
    stream: cpal::Stream,
    sample_rate: SampleRate,
    channels: usize,
    device_name: String,
    /// Number of device callbacks observed (chunks after `max_block_frames` split).
    callbacks: Arc<AtomicU64>,
}

impl InputStream {
    /// Opens a stream that calls `capture(block)` for every callback.
    ///
    /// `block` is a planar view of the freshly captured interleaved device
    /// samples (never more than `config.max_block_frames` frames,
    /// `config.channels` planes). The scratch is reused; do not retain
    /// references across calls. Use [`open_timed`](InputStream::open_timed)
    /// to also receive the capture timestamp.
    pub fn open<F>(config: &InputConfig, mut capture: F) -> Result<Self>
    where
        F: FnMut(&PlanarBuffer) + Send + 'static,
    {
        Self::open_timed(config, move |block, _block_start_us| capture(block))
    }

    /// Like [`open`](InputStream::open) but the closure also receives the
    /// block's start time in microseconds on cpal's stream clock
    /// (`InputCallbackInfo::timestamp().capture`, advanced per chunk when a
    /// callback is split at `max_block_frames`).
    pub fn open_timed<F>(config: &InputConfig, capture: F) -> Result<Self>
    where
        F: FnMut(&PlanarBuffer, u64) + Send + 'static,
    {
        validate_io_config(config.channels, config.max_block_frames, "input config")?;
        let host = cpal::default_host();
        let device = match &config.device_name {
            Some(name) => host
                .input_devices()
                .map_err(|e| SuperbError::Message(format!("enumerate devices: {e}")))?
                .find(|d| {
                    d.description()
                        .map(|desc| desc.name() == name)
                        .unwrap_or(false)
                })
                .ok_or_else(|| SuperbError::Message(format!("input device '{name}' not found")))?,
            None => host
                .default_input_device()
                .ok_or_else(|| SuperbError::Message("no default input device".into()))?,
        };
        let device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "unknown".into());

        let (chosen, channels, sample_rate) = pick_supported_input(&device, &device_name, config)?;
        let sample_format = chosen.sample_format();
        let mut stream_config: cpal::StreamConfig = chosen.into();
        if let Some(n) = config.buffer_frames {
            stream_config.buffer_size = cpal::BufferSize::Fixed(n);
        }

        let max_block = config.max_block_frames;
        let callbacks = Arc::new(AtomicU64::new(0));
        let callbacks_cb = Arc::clone(&callbacks);
        let err_cb = |e: cpal::Error| eprintln!("superb-io input stream error: {e}");
        let timeout = Some(Duration::from_secs(2));
        let stream = match sample_format {
            SampleFormat::F32 => build_input::<f32, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                capture,
                err_cb,
                timeout,
                callbacks_cb,
            )?,
            SampleFormat::I16 => build_input::<i16, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                capture,
                err_cb,
                timeout,
                callbacks_cb,
            )?,
            SampleFormat::U16 => build_input::<u16, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                capture,
                err_cb,
                timeout,
                callbacks_cb,
            )?,
            SampleFormat::I32 => build_input::<i32, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                capture,
                err_cb,
                timeout,
                callbacks_cb,
            )?,
            other => {
                return Err(SuperbError::Message(format!(
                    "unsupported device sample format {other:?}"
                )))
            }
        };
        stream
            .play()
            .map_err(|e| SuperbError::Message(format!("start stream: {e}")))?;
        Ok(Self {
            stream,
            sample_rate,
            channels,
            device_name,
            callbacks,
        })
    }

    /// Opens a capture stream that pushes planar blocks into `producer`.
    ///
    /// Overruns (no free slot in the recycle queue) are counted on the
    /// producer and the device block is dropped — nothing allocates.
    pub fn open_to_queue(config: &InputConfig, mut producer: CaptureProducer) -> Result<Self> {
        Self::open_timed(config, move |block, block_start_us| {
            let _ = producer.push(block, block_start_us);
        })
    }

    /// Resumes a paused stream.
    pub fn play(&self) -> Result<()> {
        self.stream
            .play()
            .map_err(|e| SuperbError::Message(format!("play: {e}")))
    }

    /// Pauses the stream (not supported by every backend).
    pub fn pause(&self) -> Result<()> {
        self.stream
            .pause()
            .map_err(|e| SuperbError::Message(format!("pause: {e}")))
    }

    /// Sample rate the stream runs at.
    pub fn sample_rate(&self) -> SampleRate {
        self.sample_rate
    }

    /// Channel count.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Name of the device in use.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Number of capture chunks delivered to the user closure so far.
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }
}

/// Prefer stereo when the device supports it, otherwise mono / device default.
///
/// Order: requested channels @ rate → 2ch @ rate → 1ch @ rate → device default.
fn pick_supported_input(
    device: &cpal::Device,
    device_name: &str,
    config: &InputConfig,
) -> Result<(cpal::SupportedStreamConfig, usize, SampleRate)> {
    let ranges = device
        .supported_input_configs()
        .map_err(|e| SuperbError::Message(format!("query configs: {e}")))?
        .collect::<Vec<_>>();

    let pick_fmt = |list: &[cpal::SupportedStreamConfig]| {
        list.iter()
            .find(|c| c.sample_format() == SampleFormat::F32)
            .or_else(|| list.iter().find(|c| c.sample_format() == SampleFormat::I16))
            .or_else(|| list.first())
            .cloned()
    };
    let at_rate = |channels: usize| -> Vec<cpal::SupportedStreamConfig> {
        ranges
            .iter()
            .filter(|c| c.channels() as usize == channels)
            .filter_map(|c| c.try_with_sample_rate(config.sample_rate.hz()))
            .collect()
    };

    // 1) Exact request (caller may already ask for 2 or 1).
    if let Some(chosen) = pick_fmt(&at_rate(config.channels)) {
        return Ok((chosen, config.channels, config.sample_rate));
    }
    // 2) Stereo at requested rate when available.
    if config.channels != 2 {
        if let Some(chosen) = pick_fmt(&at_rate(2)) {
            return Ok((chosen, 2, config.sample_rate));
        }
    }
    // 3) Mono at requested rate (built-in MacBook mic).
    if config.channels != 1 {
        if let Some(chosen) = pick_fmt(&at_rate(1)) {
            return Ok((chosen, 1, config.sample_rate));
        }
    }

    // 4) OS default layout (rate + channels the driver prefers).
    let default = device
        .default_input_config()
        .map_err(|e| SuperbError::Message(format!("default input config: {e}")))?;
    let channels = default.channels() as usize;
    if channels == 0 {
        return Err(SuperbError::Message(format!(
            "device '{device_name}' reported 0 input channels"
        )));
    }
    let hz = default.sample_rate();
    let sample_rate = SampleRate::new(hz).unwrap_or_else(|_| SampleRate::new_unchecked(hz));
    Ok((default, channels, sample_rate))
}

#[allow(clippy::too_many_arguments)]
fn build_input<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    max_block: usize,
    mut capture: F,
    err_cb: impl FnMut(cpal::Error) + Send + 'static,
    timeout: Option<Duration>,
    callbacks: Arc<AtomicU64>,
) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
    F: FnMut(&PlanarBuffer, u64) + Send + 'static,
{
    let mut planar = PlanarBuffer::new(channels, max_block);
    let mut marked = false;
    let hz = config.sample_rate.max(1) as u128;
    let data_cb = move |data: &[T], info: &cpal::InputCallbackInfo| {
        if !marked {
            crate::primitives::realtime::thread::enter_rt();
            marked = true;
        }
        let capture_us = stream_instant_us(info.timestamp().capture);
        let total = data.len() / channels.max(1);
        let mut done = 0;
        while done < total {
            let n = (total - done).min(max_block);
            let _ = planar.set_frames(n);
            let chunk = &data[done * channels..(done + n) * channels];
            deinterleave_from_device(chunk, channels, &mut planar);
            let chunk_start_us = capture_us.saturating_add((done as u128 * 1_000_000 / hz) as u64);
            capture(&planar, chunk_start_us);
            callbacks.fetch_add(1, Ordering::Relaxed);
            done += n;
        }
    };
    device
        .build_input_stream(config.clone(), data_cb, err_cb, timeout)
        .map_err(|e| SuperbError::Message(format!("build input stream: {e}")))
}

/// Opens a capture stream wired to a recycle SPSC of planar blocks.
///
/// `capacity` is the number of pre-allocated [`super::CaptureBlock`]s in the
/// free/filled pair. Returns the running stream and the consumer end for the
/// control / engine thread.
pub fn run_capture(
    config: &InputConfig,
    capacity: usize,
) -> Result<(InputStream, super::CaptureConsumer)> {
    let (producer, consumer) =
        super::capture_queue(config.channels, config.max_block_frames, capacity)?;
    let stream = InputStream::open_to_queue(config, producer)?;
    Ok((stream, consumer))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::io::device::capture_queue;
    use std::sync::Mutex;

    #[test]
    fn enumeration_does_not_panic_without_hardware() {
        let _ = input_devices();
    }

    #[test]
    fn config_validation_does_not_need_a_device() {
        let bad = InputConfig {
            channels: 0,
            ..InputConfig::default()
        };
        assert!(InputStream::open(&bad, |_| {}).is_err());
        let bad = InputConfig {
            max_block_frames: 0,
            ..InputConfig::default()
        };
        assert!(InputStream::open(&bad, |_| {}).is_err());
        assert!(InputStream::open_timed(&bad, |_, _| {}).is_err());
    }

    #[test]
    fn open_to_queue_rejects_bad_config_without_device() {
        let (tx, _rx) = capture_queue(2, 64, 4).unwrap();
        let bad = InputConfig {
            channels: 0,
            ..InputConfig::default()
        };
        assert!(InputStream::open_to_queue(&bad, tx).is_err());
    }

    #[test]
    fn deinterleave_path_is_exercised_by_manual_push() {
        // Simulate what the device callback does: planar block → queue → consumer.
        let (mut tx, mut rx) = capture_queue(2, 8, 2).unwrap();
        let mut planar = PlanarBuffer::new(2, 8);
        let _ = planar.set_frames(4);
        for i in 0..4 {
            planar.channel_mut(0)[i] = i as f32;
            planar.channel_mut(1)[i] = (i + 10) as f32;
        }
        assert!(tx.push(&planar, 1_000));
        let block = rx.pop().expect("filled");
        assert_eq!(block.block_start_us(), 1_000);
        assert_eq!(block.buffer().channel(0), &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(block.buffer().channel(1), &[10.0, 11.0, 12.0, 13.0]);
        rx.recycle(block);
        assert_eq!(tx.dropped(), 0);
    }

    #[test]
    fn capture_closure_receives_planar_via_shared_slot() {
        // Unit stand-in for the device callback: call the same deinterleave
        // helper and a user closure without opening a device.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let seen_cb = Arc::clone(&seen);
        let capture = move |block: &PlanarBuffer, us: u64| {
            seen_cb.lock().unwrap().push((
                us,
                block.channel(0).to_vec(),
                block.channel(1).to_vec(),
            ));
        };
        let interleaved = [0.5f32, -0.5, 0.25, -0.25];
        let mut planar = PlanarBuffer::new(2, 4);
        let _ = planar.set_frames(2);
        deinterleave_from_device(&interleaved, 2, &mut planar);
        capture(&planar, 42);
        let got = seen.lock().unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, 42);
        assert_eq!(got[0].1, vec![0.5, 0.25]);
        assert_eq!(got[0].2, vec![-0.5, -0.25]);
    }

    #[test]
    #[ignore = "requires a real audio input device"]
    fn captures_from_the_default_device() {
        let config = InputConfig {
            channels: 1,
            max_block_frames: 512,
            ..InputConfig::default()
        };
        let (stream, mut rx) = run_capture(&config, 8).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(stream.callbacks() > 0);
        let mut got = 0usize;
        while let Some(block) = rx.pop() {
            got += block.frames();
            rx.recycle(block);
        }
        assert!(got > 0, "expected at least one captured frame");
        drop(stream);
    }
}
