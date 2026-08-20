//! Platform-independent audio processing and local voice activity detection.

mod vad;
#[cfg(windows)]
mod wasapi;
mod wav;

pub use vad::{LocalVad, VadResult};
#[cfg(windows)]
pub use wasapi::{AudioDevice, capture_loopback, enumerate_render_devices};
pub use wav::write_pcm16_wav;

use std::fmt;
use std::str::FromStr;

/// The input channel or channel combination sent for transcription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelSelection {
    Left,
    Right,
    Mix,
}

impl FromStr for ChannelSelection {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "mix" => Ok(Self::Mix),
            _ => Err(format!("unknown channel: {value}")),
        }
    }
}

impl fmt::Display for ChannelSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Mix => "mix",
        })
    }
}

#[cfg(windows)]
pub fn resolve_device<'a>(
    devices: &'a [AudioDevice],
    requested: &str,
) -> Result<&'a AudioDevice, String> {
    if devices.is_empty() {
        return Err("no active Windows output devices were found".to_owned());
    }
    if let Ok(number) = requested.parse::<usize>() {
        return devices
            .get(number.wrapping_sub(1))
            .ok_or_else(|| format!("device number must be between 1 and {}", devices.len()));
    }

    let requested = requested.to_lowercase();
    let matches = devices
        .iter()
        .filter(|device| device.name.to_lowercase().contains(&requested))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [device] => Ok(*device),
        [] => Err(format!("no output device contains {requested:?}")),
        _ => Err(format!(
            "more than one output device contains {requested:?}"
        )),
    }
}

/// Select one channel or mix every interleaved channel down to mono.
pub fn select_channel(
    samples: &[f32],
    channel_count: usize,
    selection: ChannelSelection,
) -> Result<Vec<f32>, String> {
    if channel_count == 0 {
        return Err("audio must contain at least one channel".to_owned());
    }
    if !samples.len().is_multiple_of(channel_count) {
        return Err(format!(
            "sample count {} is not divisible by channel count {channel_count}",
            samples.len()
        ));
    }
    if selection == ChannelSelection::Right && channel_count < 2 {
        return Err("the selected device is mono; it has no right channel".to_owned());
    }

    let mut mono = Vec::with_capacity(samples.len() / channel_count);
    for frame in samples.chunks_exact(channel_count) {
        let sample = match selection {
            ChannelSelection::Left => frame[0],
            ChannelSelection::Right => frame[1],
            ChannelSelection::Mix => frame.iter().sum::<f32>() / channel_count as f32,
        };
        mono.push(sample);
    }
    Ok(mono)
}

/// Convert normalized floating-point samples into little-endian PCM16 bytes.
pub fn float_to_pcm16(samples: &[f32]) -> Vec<u8> {
    let mut pcm = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        let converted = (sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
        pcm.extend_from_slice(&converted.to_le_bytes());
    }
    pcm
}

/// Calculate normalized RMS energy from mono floating-point samples.
pub fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean_square =
        samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    mean_square.sqrt()
}

/// Streaming linear interpolation from a device's native rate to 24 kHz.
#[derive(Debug)]
pub struct LinearResampler {
    step: f64,
    position: f64,
    buffered: Vec<f32>,
}

/// Accumulate arbitrary capture packets into fixed-size processing chunks.
#[derive(Debug)]
pub struct SampleChunker {
    chunk_size: usize,
    pending: Vec<f32>,
}

impl SampleChunker {
    pub fn new(chunk_size: usize) -> Result<Self, String> {
        if chunk_size == 0 {
            return Err("audio chunk size must be greater than zero".to_owned());
        }
        Ok(Self {
            chunk_size,
            pending: Vec::with_capacity(chunk_size * 2),
        })
    }

    pub fn push(&mut self, samples: &[f32]) -> Vec<Vec<f32>> {
        self.pending.extend_from_slice(samples);
        let complete = self.pending.len() / self.chunk_size;
        let mut chunks = Vec::with_capacity(complete);
        for _ in 0..complete {
            chunks.push(self.pending.drain(..self.chunk_size).collect());
        }
        chunks
    }
}

impl LinearResampler {
    pub fn new(source_rate: u32, target_rate: u32) -> Result<Self, String> {
        if source_rate == 0 || target_rate == 0 {
            return Err("sample rates must be greater than zero".to_owned());
        }
        Ok(Self {
            step: source_rate as f64 / target_rate as f64,
            position: 0.0,
            buffered: Vec::new(),
        })
    }

    pub fn process(&mut self, input: &[f32]) -> Vec<f32> {
        self.buffered.extend_from_slice(input);
        let mut output = Vec::with_capacity(
            ((input.len() as f64 / self.step).ceil() as usize).saturating_add(1),
        );

        while self.position + 1.0 < self.buffered.len() as f64 {
            let first = self.position.floor() as usize;
            let fraction = (self.position - first as f64) as f32;
            let sample =
                self.buffered[first] + (self.buffered[first + 1] - self.buffered[first]) * fraction;
            output.push(sample);
            self.position += self.step;
        }

        let consumed = self.position.floor() as usize;
        if consumed > 0 {
            self.buffered.drain(..consumed);
            self.position -= consumed as f64;
        }
        output
    }
}
