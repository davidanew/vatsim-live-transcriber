//! Windows WASAPI loopback capture.

use std::ffi::c_void;
use std::slice;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, IAudioCaptureClient, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, WAVE_FORMAT_PCM, WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize, STGM_READ,
};
use windows::core::PCWSTR;

use super::{ChannelSelection, LinearResampler};
use crate::SAMPLE_RATE;

const CAPTURE_POLL_INTERVAL: Duration = Duration::from_millis(10);
const BUFFER_DURATION_100NS: i64 = 10_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

struct ComApartment;

impl ComApartment {
    fn multithreaded() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .context("could not initialize Windows COM")?;
        }
        Ok(Self)
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct TaskMemory(*mut c_void);

impl Drop for TaskMemory {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0.cast_const())) };
    }
}

fn device_enumerator() -> Result<IMMDeviceEnumerator> {
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .context("could not create the Windows audio-device enumerator")
}

unsafe fn take_wide_string(pointer: windows::core::PWSTR) -> Result<String> {
    if pointer.is_null() {
        bail!("Windows returned an empty audio-device string");
    }
    let memory = TaskMemory(pointer.0.cast());
    let value = unsafe { pointer.to_string() }
        .context("Windows returned an invalid audio-device string")?;
    drop(memory);
    Ok(value)
}

unsafe fn device_name(device: &IMMDevice) -> Result<String> {
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }
        .context("could not open the audio-device properties")?;
    let mut value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }
        .context("could not read the audio-device name")?;
    let pointer = unsafe { PropVariantToStringAlloc(&value) }
        .context("could not convert the audio-device name")?;
    let result = unsafe { take_wide_string(pointer) };
    unsafe { PropVariantClear(&mut value) }.context("could not release audio-device metadata")?;
    result
}

pub fn enumerate_render_devices() -> Result<Vec<AudioDevice>> {
    let _com = ComApartment::multithreaded()?;
    let enumerator = device_enumerator()?;
    let collection = unsafe { enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) }
        .context("could not enumerate active Windows output devices")?;
    let count = unsafe { collection.GetCount() }
        .context("could not count active Windows output devices")?;

    let mut devices = Vec::with_capacity(count as usize);
    for index in 0..count {
        let device = unsafe { collection.Item(index) }
            .with_context(|| format!("could not open Windows output device {index}"))?;
        let id = unsafe { take_wide_string(device.GetId()?) }?;
        let name = unsafe { device_name(&device) }
            .unwrap_or_else(|_| format!("Windows output device {}", index + 1));
        devices.push(AudioDevice { id, name });
    }
    Ok(devices)
}

#[derive(Debug, Clone, Copy)]
enum SampleEncoding {
    Unsigned8,
    Signed16,
    Signed24,
    Signed32,
    Float32,
    Float64,
}

#[derive(Debug, Clone, Copy)]
struct DeviceFormat {
    channels: usize,
    sample_rate: u32,
    block_align: usize,
    bytes_per_sample: usize,
    encoding: SampleEncoding,
}

impl DeviceFormat {
    unsafe fn from_wave_format(format: *const WAVEFORMATEX) -> Result<Self> {
        if format.is_null() {
            bail!("Windows returned an empty audio mix format");
        }
        let basic = unsafe { format.read_unaligned() };
        let format_tag = basic.wFormatTag as u32;
        let channels = basic.nChannels as usize;
        let sample_rate = basic.nSamplesPerSec;
        let block_align = basic.nBlockAlign as usize;
        let bits = basic.wBitsPerSample;

        if channels == 0 || sample_rate == 0 || block_align == 0 {
            bail!("Windows returned an invalid audio mix format");
        }

        let is_float = if format_tag == WAVE_FORMAT_EXTENSIBLE {
            let extended = unsafe { format.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
            let sub_format = unsafe { std::ptr::addr_of!(extended.SubFormat).read_unaligned() };
            if sub_format == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT {
                true
            } else if sub_format == KSDATAFORMAT_SUBTYPE_PCM {
                false
            } else {
                bail!("the selected audio device uses an unsupported extensible format");
            }
        } else if format_tag == WAVE_FORMAT_IEEE_FLOAT {
            true
        } else if format_tag == WAVE_FORMAT_PCM {
            false
        } else {
            bail!("the selected audio device uses unsupported format tag {format_tag}");
        };

        let encoding = match (is_float, bits) {
            (true, 32) => SampleEncoding::Float32,
            (true, 64) => SampleEncoding::Float64,
            (false, 8) => SampleEncoding::Unsigned8,
            (false, 16) => SampleEncoding::Signed16,
            (false, 24) => SampleEncoding::Signed24,
            (false, 32) => SampleEncoding::Signed32,
            _ => bail!("the selected audio device uses unsupported {bits}-bit samples"),
        };
        let bytes_per_sample = usize::from(bits).div_ceil(8);
        if bytes_per_sample * channels > block_align {
            bail!("the selected audio device has an invalid block alignment");
        }

        Ok(Self {
            channels,
            sample_rate,
            block_align,
            bytes_per_sample,
            encoding,
        })
    }

    fn decode_sample(&self, bytes: &[u8]) -> f32 {
        match self.encoding {
            SampleEncoding::Unsigned8 => (bytes[0] as f32 - 128.0) / 128.0,
            SampleEncoding::Signed16 => i16::from_le_bytes([bytes[0], bytes[1]]) as f32 / 32_768.0,
            SampleEncoding::Signed24 => {
                let raw =
                    i32::from(bytes[0]) | (i32::from(bytes[1]) << 8) | (i32::from(bytes[2]) << 16);
                let signed = if raw & 0x0080_0000 != 0 {
                    raw | !0x00ff_ffff
                } else {
                    raw
                };
                signed as f32 / 8_388_608.0
            }
            SampleEncoding::Signed32 => {
                i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32
                    / 2_147_483_648.0
            }
            SampleEncoding::Float32 => f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
            SampleEncoding::Float64 => f64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]) as f32,
        }
    }

    unsafe fn decode_mono(
        &self,
        data: *const u8,
        frame_count: usize,
        silent: bool,
        selection: ChannelSelection,
    ) -> Result<Vec<f32>> {
        if selection == ChannelSelection::Right && self.channels < 2 {
            bail!("the selected device is mono; it has no right channel");
        }
        if silent {
            return Ok(vec![0.0; frame_count]);
        }
        if data.is_null() {
            bail!("Windows returned an empty non-silent audio buffer");
        }

        let byte_count = frame_count
            .checked_mul(self.block_align)
            .ok_or_else(|| anyhow!("the captured audio packet is too large"))?;
        let packet = unsafe { slice::from_raw_parts(data, byte_count) };
        let mut mono = Vec::with_capacity(frame_count);
        for frame in packet.chunks_exact(self.block_align) {
            let read_channel = |index: usize| {
                let start = index * self.bytes_per_sample;
                self.decode_sample(&frame[start..start + self.bytes_per_sample])
            };
            mono.push(match selection {
                ChannelSelection::Left => read_channel(0),
                ChannelSelection::Right => read_channel(1),
                ChannelSelection::Mix => {
                    (0..self.channels).map(read_channel).sum::<f32>() / self.channels as f32
                }
            });
        }
        Ok(mono)
    }
}

/// Capture one Windows render endpoint in loopback mode until `stop` is set.
///
/// Each callback receives mono floating-point samples already resampled to the
/// 24 kHz rate required by the transcription service.
pub fn capture_loopback<F>(
    device_id: &str,
    selection: ChannelSelection,
    stop: Arc<AtomicBool>,
    mut on_samples: F,
) -> Result<()>
where
    F: FnMut(Vec<f32>) -> Result<()>,
{
    let _com = ComApartment::multithreaded()?;
    let enumerator = device_enumerator()?;
    let wide_id = device_id.encode_utf16().chain(Some(0)).collect::<Vec<_>>();
    let device = unsafe { enumerator.GetDevice(PCWSTR(wide_id.as_ptr())) }
        .context("the selected Windows audio device is no longer available")?;
    let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
        .context("could not activate WASAPI loopback capture")?;
    let format_pointer = unsafe { client.GetMixFormat() }
        .context("could not read the selected device's audio format")?;
    let format_memory = TaskMemory(format_pointer.cast());
    let format = unsafe { DeviceFormat::from_wave_format(format_pointer) }?;

    unsafe {
        client.Initialize(
            AUDCLNT_SHAREMODE_SHARED,
            AUDCLNT_STREAMFLAGS_LOOPBACK,
            BUFFER_DURATION_100NS,
            0,
            format_pointer,
            None,
        )
    }
    .context("could not initialize WASAPI loopback capture")?;
    let capture: IAudioCaptureClient =
        unsafe { client.GetService() }.context("could not obtain the WASAPI capture service")?;
    let mut resampler = LinearResampler::new(format.sample_rate, SAMPLE_RATE)
        .map_err(|message| anyhow!(message))?;

    unsafe { client.Start() }.context("could not start WASAPI loopback capture")?;
    let capture_result = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(CAPTURE_POLL_INTERVAL);
            loop {
                let packet_size = unsafe { capture.GetNextPacketSize() }
                    .context("could not query the WASAPI capture buffer")?;
                if packet_size == 0 {
                    break;
                }

                let mut data = std::ptr::null_mut();
                let mut frame_count = 0_u32;
                let mut flags = 0_u32;
                unsafe { capture.GetBuffer(&mut data, &mut frame_count, &mut flags, None, None) }
                    .context("could not read a WASAPI capture packet")?;

                let decoded = unsafe {
                    format.decode_mono(
                        data,
                        frame_count as usize,
                        flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0,
                        selection,
                    )
                };
                let release = unsafe { capture.ReleaseBuffer(frame_count) };
                release.context("could not release a WASAPI capture packet")?;

                let resampled = resampler.process(&decoded?);
                if !resampled.is_empty() {
                    on_samples(resampled)?;
                }
            }
        }
        Ok(())
    })();

    let stop_result = unsafe { client.Stop() }.context("could not stop WASAPI capture");
    drop(format_memory);
    capture_result.and(stop_result)
}
