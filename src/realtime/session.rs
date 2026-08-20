use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message;

use super::protocol::{ServerEventProcessor, TranscriptEvent, session_update};
use crate::audio::{
    AudioDevice, ChannelSelection, LocalVad, SampleChunker, capture_loopback, float_to_pcm16, rms,
};
use crate::recording::RecordingTracker;
use crate::{FRAMES_PER_CHUNK, REALTIME_URL};

#[derive(Debug, Clone)]
pub struct TranscriberConfig {
    pub api_key: String,
    pub device: AudioDevice,
    pub channel: ChannelSelection,
    pub accuracy: String,
    pub prompt: String,
    pub keywords: Vec<String>,
    pub output_path: PathBuf,
    pub recordings_dir: PathBuf,
    pub vad_rms: f32,
    pub silence_ms: u32,
}

enum AudioCommand {
    Append(Vec<u8>),
    Commit,
    Failed(String),
    Finished,
}

pub struct TranscriberHandle {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl TranscriberHandle {
    pub fn start(config: TranscriberConfig, events: Sender<TranscriptEvent>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let worker = thread::Builder::new()
            .name("openai-transcription".to_owned())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build();
                let result = runtime
                    .context("could not create the asynchronous runtime")
                    .and_then(|runtime| {
                        runtime.block_on(run_session(config, events.clone(), worker_stop))
                    });
                if let Err(error) = result {
                    let _ = events.send(TranscriptEvent::Error(format!("{error:#}")));
                }
                let _ = events.send(TranscriptEvent::Stopped);
            })
            .expect("could not start transcription worker");
        Self {
            stop,
            worker: Some(worker),
        }
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_none_or(thread::JoinHandle::is_finished)
    }

    pub fn join(&mut self) {
        self.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for TranscriberHandle {
    fn drop(&mut self) {
        self.join();
    }
}

async fn run_session(
    config: TranscriberConfig,
    events: Sender<TranscriptEvent>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let mut request = REALTIME_URL
        .into_client_request()
        .context("could not create the OpenAI WebSocket request")?;
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {}", config.api_key))
            .context("the OpenAI API key is not a valid HTTP header value")?,
    );
    let (socket, _) = connect_async(request)
        .await
        .context("could not connect to OpenAI Realtime transcription")?;
    let (mut writer, mut reader) = socket.split();
    writer
        .send(Message::Text(
            session_update(&config.prompt, &config.keywords, &config.accuracy)
                .to_string()
                .into(),
        ))
        .await
        .context("could not configure the transcription session")?;
    events
        .send(TranscriptEvent::Connected)
        .context("the transcript window has closed")?;

    let recordings = Arc::new(Mutex::new(RecordingTracker::new(
        config.recordings_dir.clone(),
    )));
    let mut server =
        ServerEventProcessor::new(Arc::clone(&recordings), config.output_path.clone(), events);
    let (audio_sender, mut audio_receiver) = mpsc::unbounded_channel();
    let audio_thread = spawn_audio_capture(
        config,
        Arc::clone(&stop),
        Arc::clone(&recordings),
        audio_sender,
    )?;

    let session_result: Result<()> = async {
        while !stop.load(Ordering::Relaxed) {
            tokio::select! {
                command = audio_receiver.recv() => {
                    match command {
                        Some(AudioCommand::Append(pcm)) => {
                            writer.send(Message::Text(json!({
                                "type": "input_audio_buffer.append",
                                "audio": BASE64.encode(pcm),
                            }).to_string().into())).await
                                .context("could not send captured audio to OpenAI")?;
                        }
                        Some(AudioCommand::Commit) => {
                            writer.send(Message::Text(json!({
                                "type": "input_audio_buffer.commit"
                            }).to_string().into())).await
                                .context("could not commit the captured radio turn")?;
                        }
                        Some(AudioCommand::Failed(message)) => bail!("audio capture failed: {message}"),
                        Some(AudioCommand::Finished) | None => break,
                    }
                }
                message = reader.next() => {
                    match message {
                        Some(Ok(Message::Text(text))) => server.process(text.as_str())?,
                        Some(Ok(Message::Ping(payload))) => {
                            writer.send(Message::Pong(payload)).await
                                .context("could not respond to the OpenAI WebSocket ping")?;
                        }
                        Some(Ok(Message::Close(frame))) => {
                            if !stop.load(Ordering::Relaxed) {
                                let detail = frame.map(|frame| format!("{} {}", frame.code, frame.reason))
                                    .unwrap_or_else(|| "no reason supplied".to_owned());
                                bail!("OpenAI closed the connection: {detail}");
                            }
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(error)) => return Err(error).context("OpenAI WebSocket error"),
                        None => break,
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        Ok(())
    }
    .await;

    stop.store(true, Ordering::Relaxed);
    let _ = writer.send(Message::Close(None)).await;
    let audio_result = audio_thread
        .join()
        .map_err(|_| anyhow!("the audio capture thread panicked"));
    session_result.and(audio_result)
}

fn spawn_audio_capture(
    config: TranscriberConfig,
    stop: Arc<AtomicBool>,
    recordings: Arc<Mutex<RecordingTracker>>,
    sender: mpsc::UnboundedSender<AudioCommand>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("wasapi-capture".to_owned())
        .spawn(move || {
            let result = capture_audio(config, Arc::clone(&stop), &recordings, &sender);
            if let Err(error) = result {
                let _ = sender.send(AudioCommand::Failed(format!("{error:#}")));
            } else {
                let _ = sender.send(AudioCommand::Finished);
            }
        })
        .context("could not start the WASAPI capture thread")
}

fn capture_audio(
    config: TranscriberConfig,
    stop: Arc<AtomicBool>,
    recordings: &Arc<Mutex<RecordingTracker>>,
    sender: &mpsc::UnboundedSender<AudioCommand>,
) -> Result<()> {
    let mut vad = LocalVad::new(config.vad_rms, config.silence_ms, 100, 300);
    let mut chunker = SampleChunker::new(FRAMES_PER_CHUNK).map_err(|message| anyhow!(message))?;
    let mut turn_audio = Vec::new();
    capture_loopback(&config.device.id, config.channel, stop, |samples| {
        for chunk in chunker.push(&samples) {
            let energy = rms(&chunk);
            let pcm = float_to_pcm16(&chunk);
            let result = vad.process(&pcm, energy);
            for outgoing in result.chunks {
                turn_audio.extend_from_slice(&outgoing);
                sender
                    .send(AudioCommand::Append(outgoing))
                    .context("the transcription worker has stopped")?;
            }
            if result.commit {
                recordings
                    .lock()
                    .map_err(|_| anyhow!("recording state was poisoned"))?
                    .save_turn(&turn_audio)?;
                turn_audio.clear();
                sender
                    .send(AudioCommand::Commit)
                    .context("the transcription worker has stopped")?;
            }
        }
        Ok(())
    })
}
