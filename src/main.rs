#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::Local;
use clap::Parser;

use vatsim_live_transcriber::audio::{
    ChannelSelection, capture_loopback, enumerate_render_devices, resolve_device, rms,
};
use vatsim_live_transcriber::config::{load_keywords, load_prompt};
use vatsim_live_transcriber::ui::{self, AppOptions};

#[derive(Debug, Parser)]
#[command(
    name = "vatsim-live-transcriber",
    about = "Live VATSIM/system-audio transcription"
)]
struct Arguments {
    /// List active Windows output devices and exit.
    #[arg(long)]
    list_devices: bool,

    /// Device number or a unique part of its name.
    #[arg(long)]
    device: Option<String>,

    /// Select the left or right channel, or mix every channel.
    #[arg(long)]
    channel: Option<ChannelSelection>,

    /// Transcription accuracy setting.
    #[arg(
        long,
        default_value = "medium",
        value_parser = ["minimal", "low", "medium", "high", "xhigh"]
    )]
    accuracy: String,

    /// Text file containing one keyword per line.
    #[arg(long)]
    keywords: Option<PathBuf>,

    /// UTF-8 text file replacing the default ATC prompt.
    #[arg(long)]
    prompt_file: Option<PathBuf>,

    /// Transcript log path.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Local RMS level that starts or continues a radio transmission.
    #[arg(long, default_value_t = 0.008)]
    vad_rms: f32,

    /// Silence that ends a radio transmission.
    #[arg(long, default_value_t = 500)]
    silence_ms: u32,

    /// Capture this many seconds without connecting to OpenAI.
    #[arg(long, value_name = "SECONDS")]
    test_audio: Option<u64>,
}

fn run() -> Result<()> {
    let arguments = Arguments::parse();
    if arguments.list_devices {
        for (index, device) in enumerate_render_devices()?.iter().enumerate() {
            println!("{:>2}. {}", index + 1, device.name);
        }
        return Ok(());
    }

    if let Some(seconds) = arguments.test_audio {
        let devices = enumerate_render_devices()?;
        let requested = arguments
            .device
            .as_deref()
            .context("--test-audio requires --device")?;
        let device = resolve_device(&devices, requested).map_err(|message| anyhow!(message))?;
        println!(
            "Capturing {} for {seconds} seconds ({})...",
            device.name,
            arguments.channel.unwrap_or(ChannelSelection::Left)
        );

        let stop = Arc::new(AtomicBool::new(false));
        let timer_stop = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(seconds));
            timer_stop.store(true, Ordering::Relaxed);
        });

        let mut sample_count = 0_usize;
        let mut peak_rms = 0.0_f32;
        capture_loopback(
            &device.id,
            arguments.channel.unwrap_or(ChannelSelection::Left),
            stop,
            |samples| {
                sample_count += samples.len();
                peak_rms = peak_rms.max(rms(&samples));
                Ok(())
            },
        )?;
        println!("Captured {sample_count} mono samples at 24 kHz; peak RMS {peak_rms:.6}");
        return Ok(());
    }

    if !(0.0 < arguments.vad_rms && arguments.vad_rms <= 1.0) {
        anyhow::bail!("--vad-rms must be greater than 0 and no greater than 1");
    }
    if arguments.silence_ms < 100 {
        anyhow::bail!("--silence-ms must be at least 100");
    }

    let devices = enumerate_render_devices()?;
    if devices.is_empty() {
        anyhow::bail!("no active Windows output devices were found");
    }
    let base = resource_base();
    let keyword_path = arguments
        .keywords
        .unwrap_or_else(|| base.join("keywords.txt"));
    let keywords = load_keywords(&keyword_path)
        .with_context(|| format!("could not load keywords from {}", keyword_path.display()))?;
    let prompt = load_prompt(arguments.prompt_file.as_deref())?;
    let output_path = arguments.output.unwrap_or_else(|| {
        base.join("transcripts").join(format!(
            "vatsim-{}.txt",
            Local::now().format("%Y%m%d-%H%M%S")
        ))
    });
    let stem = output_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("vatsim");
    let recordings_dir = output_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{stem}-audio"));
    let options = AppOptions {
        devices,
        requested_device: arguments.device,
        requested_channel: arguments.channel,
        accuracy: arguments.accuracy,
        api_key: std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        prompt,
        keywords,
        output_path,
        recordings_dir,
        vad_rms: arguments.vad_rms,
        silence_ms: arguments.silence_ms,
    };
    ui::run(options).map_err(|message| anyhow!(message))
}

fn resource_base() -> PathBuf {
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if current.join("keywords.txt").is_file() {
        return current;
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or(current)
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error:#}");
        std::process::exit(1);
    }
}
