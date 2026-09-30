//! Output (playback) streams.

use std::sync::Arc;
use std::time::Duration;

use crate::primitives::buffers::PlanarBuffer;
use crate::primitives::graph::BlockProcessor;
use crate::primitives::realtime::LoadMeter;
use crate::primitives::{Result, SampleRate, SuperbError};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};

use super::{stream_instant_us, validate_io_config, DeviceInfo};
use crate::primitives::io::Engine;

/// Lists output devices of the default host.
pub fn output_devices() -> Result<Vec<DeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_output_device()
        .and_then(|d| d.description().ok())
        .map(|d| d.name().to_owned());
    let devices = host
        .output_devices()
        .map_err(|e| SuperbError::Message(format!("enumerate devices: {e}")))?;
    Ok(devices
        .filter_map(|d| d.description().ok())
        .map(|desc| DeviceInfo {
            is_default: default_name.as_deref() == Some(desc.name()),
            name: desc.name().to_owned(),
            max_channels: 0,
        })
        .collect())
}

/// How to open an output stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputConfig {
    /// Device name from [`output_devices`], or `None` for the default device.
    pub device_name: Option<String>,
    /// Requested sample rate.
    pub sample_rate: SampleRate,
    /// Requested channel count.
    pub channels: usize,
    /// Requested buffer size in frames, or `None` for the device default.
    pub buffer_frames: Option<u32>,
    /// Largest block the callback will ever be asked for. Callbacks larger
    /// than this are processed in chunks.
    pub max_block_frames: usize,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            device_name: None,
            sample_rate: SampleRate::HZ_48K,
            channels: 2,
            buffer_frames: None,
            max_block_frames: 4096,
        }
    }
}

/// A running output stream. Dropping it stops the audio.
pub struct OutputStream {
    stream: cpal::Stream,
    sample_rate: SampleRate,
    channels: usize,
    load: Arc<LoadMeter>,
    device_name: String,
}

impl OutputStream {
    /// Opens a stream that calls `render(block)` for every callback.
    ///
    /// `block` arrives with the callback's frame count (never more than
    /// `config.max_block_frames`) and `config.channels` channels; write
    /// planar audio into it. Use [`open_timed`](OutputStream::open_timed) to
    /// also receive the callback's stream timestamp.
    pub fn open<F>(config: &OutputConfig, mut render: F) -> Result<Self>
    where
        F: FnMut(&mut PlanarBuffer) + Send + 'static,
    {
        Self::open_timed(config, move |block, _block_start_us| render(block))
    }

    /// Like [`open`](OutputStream::open) but the closure also receives the
    /// block's start time in microseconds on cpal's stream clock
    /// (`OutputCallbackInfo::timestamp().callback`, advanced per chunk when a
    /// callback is split at `max_block_frames`). Pass it to
    /// [`Engine::set_midi_block_start_us`] before rendering to place queued
    /// MIDI stamped on the same clock.
    pub fn open_timed<F>(config: &OutputConfig, render: F) -> Result<Self>
    where
        F: FnMut(&mut PlanarBuffer, u64) + Send + 'static,
    {
        let load = Arc::new(LoadMeter::new());
        Self::open_with_meter(config, render, load)
    }

    pub(super) fn open_with_meter<F>(
        config: &OutputConfig,
        render: F,
        load: Arc<LoadMeter>,
    ) -> Result<Self>
    where
        F: FnMut(&mut PlanarBuffer, u64) + Send + 'static,
    {
        validate_io_config(config.channels, config.max_block_frames, "output config")?;
        let host = cpal::default_host();
        let device = match &config.device_name {
            Some(name) => host
                .output_devices()
                .map_err(|e| SuperbError::Message(format!("enumerate devices: {e}")))?
                .find(|d| {
                    d.description()
                        .map(|desc| desc.name() == name)
                        .unwrap_or(false)
                })
                .ok_or_else(|| SuperbError::Message(format!("output device '{name}' not found")))?,
            None => host
                .default_output_device()
                .ok_or_else(|| SuperbError::Message("no default output device".into()))?,
        };
        let device_name = device
            .description()
            .map(|d| d.name().to_owned())
            .unwrap_or_else(|_| "unknown".into());

        // Prefer f32, then i16, then whatever the default is.
        let supported = device
            .supported_output_configs()
            .map_err(|e| SuperbError::Message(format!("query configs: {e}")))?
            .filter(|c| c.channels() as usize == config.channels)
            .filter_map(|c| c.try_with_sample_rate(config.sample_rate.hz()))
            .collect::<Vec<_>>();
        let pick = |fmt: SampleFormat| supported.iter().find(|c| c.sample_format() == fmt).cloned();
        let chosen = pick(SampleFormat::F32)
            .or_else(|| pick(SampleFormat::I16))
            .or_else(|| supported.first().cloned())
            .ok_or_else(|| {
                SuperbError::Message(format!(
                    "device '{device_name}' does not support {} ch @ {}",
                    config.channels, config.sample_rate
                ))
            })?;
        let sample_format = chosen.sample_format();
        let mut stream_config: cpal::StreamConfig = chosen.into();
        if let Some(n) = config.buffer_frames {
            stream_config.buffer_size = cpal::BufferSize::Fixed(n);
        }

        let channels = config.channels;
        let max_block = config.max_block_frames;
        let err_cb = |e: cpal::Error| eprintln!("superb-io stream error: {e}");
        let timeout = Some(Duration::from_secs(2));
        let stream = match sample_format {
            SampleFormat::F32 => build::<f32, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                render,
                err_cb,
                timeout,
            )?,
            SampleFormat::I16 => build::<i16, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                render,
                err_cb,
                timeout,
            )?,
            SampleFormat::U16 => build::<u16, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                render,
                err_cb,
                timeout,
            )?,
            SampleFormat::I32 => build::<i32, _>(
                &device,
                &stream_config,
                channels,
                max_block,
                render,
                err_cb,
                timeout,
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
            sample_rate: config.sample_rate,
            channels,
            load,
            device_name,
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

    /// DSP load meter (only populated when the stream was created by [`run_engine`]).
    pub fn load(&self) -> Arc<LoadMeter> {
        Arc::clone(&self.load)
    }
}

#[allow(clippy::too_many_arguments)]
fn build<T, F>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    max_block: usize,
    mut render: F,
    err_cb: impl FnMut(cpal::Error) + Send + 'static,
    timeout: Option<Duration>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
    F: FnMut(&mut PlanarBuffer, u64) + Send + 'static,
{
    let mut planar = PlanarBuffer::new(channels, max_block);
    let mut marked = false;
    let hz = config.sample_rate.max(1) as u128;
    let data_cb = move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
        if !marked {
            crate::primitives::realtime::thread::enter_rt();
            marked = true;
        }
        let callback_us = stream_instant_us(info.timestamp().callback);
        let total = data.len() / channels;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(max_block);
            // Capacity is max_block, so this never fails.
            let _ = planar.set_frames(n);
            planar.clear();
            let chunk_start_us = callback_us.saturating_add((done as u128 * 1_000_000 / hz) as u64);
            render(&mut planar, chunk_start_us);
            let out = &mut data[done * channels..(done + n) * channels];
            for ch in 0..channels {
                let plane = planar.channel(ch);
                for (frame, &s) in plane.iter().enumerate() {
                    out[frame * channels + ch] = T::from_sample(s);
                }
            }
            done += n;
        }
    };
    device
        .build_output_stream(config.clone(), data_cb, err_cb, timeout)
        .map_err(|e| SuperbError::Message(format!("build stream: {e}")))
}

/// Prepares `engine` for the stream configuration and runs it on the device.
///
/// The engine is moved into the callback; keep control handles
/// (`Arc<AtomicF32>` parameters, SPSC producers, the load meter) before calling.
/// Before every block the engine's MIDI block start is set to the cpal
/// callback timestamp (see the module docs for the clock caveat).
pub fn run_engine<P>(config: &OutputConfig, mut engine: Engine<P>) -> Result<OutputStream>
where
    P: BlockProcessor + 'static,
{
    engine.prepare(config.sample_rate, config.max_block_frames)?;
    let load = engine.load();
    OutputStream::open_with_meter(
        config,
        move |block: &mut PlanarBuffer, block_start_us: u64| {
            engine.set_midi_block_start_us(Some(block_start_us));
            if engine.render(block).is_err() {
                block.clear();
            }
        },
        load,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::graph::nodes::SineNode;
    use crate::primitives::graph::AudioGraph;
    use crate::primitives::transport::Transport;

    #[test]
    fn enumeration_does_not_panic_without_hardware() {
        // May be empty or Err on CI; must not panic.
        let _ = output_devices();
    }

    #[test]
    fn config_validation_does_not_need_a_device() {
        let bad = OutputConfig {
            channels: 0,
            ..OutputConfig::default()
        };
        assert!(OutputStream::open(&bad, |_| {}).is_err());
        let bad = OutputConfig {
            max_block_frames: 0,
            ..OutputConfig::default()
        };
        assert!(OutputStream::open(&bad, |_| {}).is_err());
        assert!(OutputStream::open_timed(&bad, |_, _| {}).is_err());
    }

    #[test]
    #[ignore = "requires a real audio output device"]
    fn plays_a_sine_on_the_default_device() {
        let mut graph = AudioGraph::new();
        let sine = graph.add_node(Box::new(SineNode::new(440.0, 0.1, 2)));
        graph.set_output(sine).unwrap();
        let mut engine = Engine::new(graph, Transport::new(SampleRate::HZ_48K, 120.0));
        engine.transport_mut().play();
        let load = engine.load();
        let stream = run_engine(&OutputConfig::default(), engine).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(load.callbacks() > 0);
        assert_eq!(stream.channels(), 2);
        drop(stream);
    }
}
