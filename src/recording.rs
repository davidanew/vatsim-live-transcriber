//! Per-transmission WAV storage, server item association, and playback.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::Local;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME, SND_NODEFAULT};
use windows::core::PCWSTR;

use crate::audio::write_pcm16_wav;

#[derive(Debug)]
pub struct RecordingTracker {
    directory: PathBuf,
    session_id: String,
    next_number: u32,
    pending: VecDeque<PathBuf>,
    by_item: HashMap<String, PathBuf>,
}

impl RecordingTracker {
    pub fn new(directory: PathBuf) -> Self {
        Self::with_session_id(directory, Local::now().format("%H%M%S-%f").to_string())
    }

    pub fn with_session_id(directory: PathBuf, session_id: String) -> Self {
        Self {
            directory,
            session_id,
            next_number: 0,
            pending: VecDeque::new(),
            by_item: HashMap::new(),
        }
    }

    pub fn save_turn(&mut self, pcm: &[u8]) -> Result<Option<PathBuf>> {
        if pcm.is_empty() {
            return Ok(None);
        }
        self.next_number = self
            .next_number
            .checked_add(1)
            .context("too many recordings were created in one session")?;
        let path = self.directory.join(format!(
            "turn-{}-{:04}.wav",
            self.session_id, self.next_number
        ));
        write_pcm16_wav(&path, pcm)
            .with_context(|| format!("could not save recording {}", path.display()))?;
        self.pending.push_back(path.clone());
        Ok(Some(path))
    }

    pub fn bind(&mut self, item_id: &str) -> Option<PathBuf> {
        if item_id.is_empty() {
            return None;
        }
        if let Some(existing) = self.by_item.get(item_id) {
            return Some(existing.clone());
        }
        let path = self.pending.pop_front()?;
        self.by_item.insert(item_id.to_owned(), path.clone());
        Some(path)
    }

    pub fn finish(&mut self, item_id: &str) {
        self.by_item.remove(item_id);
    }
}

pub fn play_wav(path: &Path) -> Result<()> {
    if !path.is_file() {
        bail!("the recording could not be found: {}", path.display());
    }
    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let flags = SND_FILENAME | SND_ASYNC | SND_NODEFAULT;
    let played = unsafe { PlaySoundW(PCWSTR(wide_path.as_ptr()), None, flags) };
    if played.as_bool() {
        Ok(())
    } else {
        bail!("Windows could not play recording {}", path.display())
    }
}

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
