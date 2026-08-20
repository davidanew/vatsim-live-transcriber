use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use serde_json::{Value, json};

use crate::recording::RecordingTracker;
use crate::transcript::transcript_variants;
use crate::{SAMPLE_RATE, TRANSCRIPTION_MODEL};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptEvent {
    Connected,
    Delta {
        item_id: String,
        original: String,
        converted: String,
    },
    Completed {
        item_id: String,
        original: String,
        converted: String,
        audio_path: Option<PathBuf>,
    },
    Error(String),
    Stopped,
}

pub fn session_update(prompt: &str, keywords: &[String], accuracy: &str) -> Value {
    json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
                    "transcription": {
                        "model": TRANSCRIPTION_MODEL,
                        "prompt": prompt,
                        "keywords": keywords,
                        "languages": ["en"],
                        "delay": accuracy,
                    },
                    "turn_detection": null,
                }
            }
        }
    })
}

pub struct ServerEventProcessor {
    streamed_text: HashMap<String, String>,
    recordings: Arc<Mutex<RecordingTracker>>,
    output_path: PathBuf,
    events: Sender<TranscriptEvent>,
}

impl ServerEventProcessor {
    pub fn new(
        recordings: Arc<Mutex<RecordingTracker>>,
        output_path: PathBuf,
        events: Sender<TranscriptEvent>,
    ) -> Self {
        Self {
            streamed_text: HashMap::new(),
            recordings,
            output_path,
            events,
        }
    }

    pub fn process(&mut self, message: &str) -> Result<()> {
        let event: Value =
            serde_json::from_str(message).context("the server returned an unreadable message")?;
        let event_type = event["type"].as_str().unwrap_or_default();
        match event_type {
            "input_audio_buffer.committed" => {
                let item_id = event["item_id"].as_str().unwrap_or_default();
                self.recordings
                    .lock()
                    .map_err(|_| anyhow!("recording state was poisoned"))?
                    .bind(item_id);
            }
            "conversation.item.input_audio_transcription.delta" => {
                let item_id = event["item_id"].as_str().unwrap_or_default();
                let delta = event["delta"].as_str().unwrap_or_default();
                if !delta.is_empty() {
                    self.recordings
                        .lock()
                        .map_err(|_| anyhow!("recording state was poisoned"))?
                        .bind(item_id);
                    let text = self.streamed_text.entry(item_id.to_owned()).or_default();
                    text.push_str(delta);
                    self.events
                        .send(TranscriptEvent::Delta {
                            item_id: item_id.to_owned(),
                            original: text.clone(),
                            converted: crate::transcript::normalize_spoken_numbers(text),
                        })
                        .context("the transcript window has closed")?;
                }
            }
            "conversation.item.input_audio_transcription.completed" => {
                let item_id = event["item_id"].as_str().unwrap_or_default();
                let raw = event["transcript"].as_str().unwrap_or_default().trim();
                self.streamed_text.remove(item_id);
                let audio_path = self
                    .recordings
                    .lock()
                    .map_err(|_| anyhow!("recording state was poisoned"))?
                    .bind(item_id);
                let variants = transcript_variants(raw);
                if let [original, converted] = variants.as_slice() {
                    self.events
                        .send(TranscriptEvent::Completed {
                            item_id: item_id.to_owned(),
                            original: original.clone(),
                            converted: converted.clone(),
                            audio_path: audio_path.clone(),
                        })
                        .context("the transcript window has closed")?;
                    append_log(
                        &self.output_path,
                        original,
                        converted,
                        audio_path.as_deref(),
                    )?;
                }
                self.recordings
                    .lock()
                    .map_err(|_| anyhow!("recording state was poisoned"))?
                    .finish(item_id);
            }
            "error" => {
                let message = event["error"]["message"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| event.to_string());
                bail!("OpenAI API error: {message}");
            }
            _ => {}
        }
        Ok(())
    }
}

fn append_log(
    output_path: &Path,
    original: &str,
    converted: &str,
    audio_path: Option<&Path>,
) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create transcript folder {}", parent.display()))?;
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(output_path)
        .with_context(|| format!("could not open transcript log {}", output_path.display()))?;
    writeln!(log, "[{}] {original}", Local::now().format("%H:%M:%S"))?;
    writeln!(log, "           {converted}")?;
    if let Some(path) = audio_path {
        writeln!(log, "           [audio] {}", path.display())?;
    }
    Ok(())
}
