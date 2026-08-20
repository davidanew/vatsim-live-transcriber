use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use crate::SAMPLE_RATE;

/// Write mono, 24 kHz, signed PCM16 data as a standard RIFF/WAVE file.
pub fn write_pcm16_wav(path: &Path, pcm: &[u8]) -> io::Result<()> {
    if !pcm.len().is_multiple_of(2) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "PCM16 data must contain complete two-byte samples",
        ));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let data_size = u32::try_from(pcm.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "recording is too large for WAV",
        )
    })?;
    let riff_size = 36_u32.checked_add(data_size).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "recording is too large for WAV",
        )
    })?;

    let mut file = File::create(path)?;
    file.write_all(b"RIFF")?;
    file.write_all(&riff_size.to_le_bytes())?;
    file.write_all(b"WAVEfmt ")?;
    file.write_all(&16_u32.to_le_bytes())?; // PCM format chunk length
    file.write_all(&1_u16.to_le_bytes())?; // PCM format
    file.write_all(&1_u16.to_le_bytes())?; // mono
    file.write_all(&SAMPLE_RATE.to_le_bytes())?;
    file.write_all(&(SAMPLE_RATE * 2).to_le_bytes())?; // byte rate
    file.write_all(&2_u16.to_le_bytes())?; // block alignment
    file.write_all(&16_u16.to_le_bytes())?; // bits per sample
    file.write_all(b"data")?;
    file.write_all(&data_size.to_le_bytes())?;
    file.write_all(pcm)?;
    file.flush()
}
