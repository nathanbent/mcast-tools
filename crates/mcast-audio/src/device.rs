//! Audio device lookup and cpal stream helpers.

use crate::format::AudioFormat;
use crate::jitter::JitterReader;
use anyhow::{anyhow, bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use rtrb::Producer;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::Thread;

/// Human-readable list of input and output devices on the default host.
pub fn list() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let default_in = host.default_input_device().and_then(|d| d.name().ok());
    let default_out = host.default_output_device().and_then(|d| d.name().ok());
    let mut lines = vec![format!("host: {}", host.id().name())];
    for (kind, devices, default) in [
        (
            "input ",
            host.input_devices()?.collect::<Vec<_>>(),
            &default_in,
        ),
        (
            "output",
            host.output_devices()?.collect::<Vec<_>>(),
            &default_out,
        ),
    ] {
        for d in devices {
            let name = d.name().unwrap_or_else(|_| "<unknown>".into());
            let mark = if Some(&name) == default.as_ref() {
                " (default)"
            } else {
                ""
            };
            lines.push(format!("  {kind}  {name}{mark}"));
        }
    }
    Ok(lines)
}

fn find(devices: impl Iterator<Item = Device>, wanted: &str) -> Option<Device> {
    let wanted = wanted.to_lowercase();
    devices.into_iter().find(|d| {
        d.name()
            .map(|n| n.to_lowercase().contains(&wanted))
            .unwrap_or(false)
    })
}

pub fn output_device(name: Option<&str>) -> Result<Device> {
    let host = cpal::default_host();
    match name {
        None => host
            .default_output_device()
            .ok_or_else(|| anyhow!("no default output device")),
        Some(n) => find(host.output_devices()?, n)
            .ok_or_else(|| anyhow!("no output device matching '{n}' (try `mcast audio devices`)")),
    }
}

pub fn input_device(name: Option<&str>) -> Result<Device> {
    let host = cpal::default_host();
    match name {
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device")),
        Some(n) => find(host.input_devices()?, n)
            .ok_or_else(|| anyhow!("no input device matching '{n}' (try `mcast audio devices`)")),
    }
}

fn stream_config(format: &AudioFormat) -> StreamConfig {
    StreamConfig {
        channels: format.channels,
        sample_rate: cpal::SampleRate(format.sample_rate),
        buffer_size: cpal::BufferSize::Default,
    }
}

/// Start playing from `buffer` on `device`.
pub fn play(device: &Device, format: &AudioFormat, buffer: JitterReader) -> Result<Stream> {
    let sample_format = device.default_output_config()?.sample_format();
    let config = stream_config(format);
    let stream = match sample_format {
        SampleFormat::F32 => build_output::<f32>(device, &config, buffer),
        SampleFormat::I16 => build_output::<i16>(device, &config, buffer),
        SampleFormat::U16 => build_output::<u16>(device, &config, buffer),
        SampleFormat::I32 => build_output::<i32>(device, &config, buffer),
        other => bail!("unsupported output sample format {other:?}"),
    }
    .with_context(|| format!("opening output device for {format}"))?;
    stream.play()?;
    Ok(stream)
}

fn build_output<T>(
    device: &Device,
    config: &StreamConfig,
    mut buffer: JitterReader,
) -> Result<Stream>
where
    T: SizedSample + FromSample<f32>,
{
    // Converted in fixed-size blocks so the callback never allocates.
    let mut scratch = [0.0f32; 1024];
    let block = scratch.len() / buffer.channels() * buffer.channels();
    let stream = device.build_output_stream(
        config,
        move |data: &mut [T], _| {
            for chunk in data.chunks_mut(block) {
                let s = &mut scratch[..chunk.len()];
                buffer.pull(s);
                for (d, x) in chunk.iter_mut().zip(s.iter()) {
                    *d = T::from_sample(*x);
                }
            }
        },
        |e| eprintln!("audio output error: {e}"),
        None,
    )?;
    Ok(stream)
}

/// The rate and channel count `device` captures at by default.
pub fn input_format(device: &Device) -> Result<(u32, u16)> {
    let c = device.default_input_config()?;
    Ok((c.sample_rate().0, c.channels()))
}

/// Start capturing from `device` at its default rate and channel count (see
/// [`input_format`]), writing interleaved f32 samples to `ring` and waking
/// `reader` after each callback. Whole frames are dropped (never blocked on)
/// if the reader falls behind; the count is added to `dropped`.
pub fn capture(
    device: &Device,
    ring: Producer<f32>,
    reader: Thread,
    dropped: Arc<AtomicU64>,
) -> Result<Stream> {
    let supported = device.default_input_config()?;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_input::<f32>(device, &config, ring, reader, dropped),
        SampleFormat::I16 => build_input::<i16>(device, &config, ring, reader, dropped),
        SampleFormat::U16 => build_input::<u16>(device, &config, ring, reader, dropped),
        SampleFormat::I32 => build_input::<i32>(device, &config, ring, reader, dropped),
        other => bail!("unsupported input sample format {other:?}"),
    }
    .context("opening input device")?;
    stream.play()?;
    Ok(stream)
}

fn build_input<T>(
    device: &Device,
    config: &StreamConfig,
    mut ring: Producer<f32>,
    reader: Thread,
    dropped: Arc<AtomicU64>,
) -> Result<Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let channels = (config.channels as usize).max(1);
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let room = ring.slots() / channels * channels;
            let n = data.len().min(room);
            if n < data.len() {
                dropped.fetch_add((data.len() - n) as u64, Ordering::Relaxed);
            }
            if let Ok(chunk) = ring.write_chunk_uninit(n) {
                chunk.fill_from_iter(data[..n].iter().map(|s| s.to_sample::<f32>()));
            }
            reader.unpark();
        },
        |e| eprintln!("audio input error: {e}"),
        None,
    )?;
    Ok(stream)
}
