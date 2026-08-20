//! Core components for the Rust VATSIM Live Transcriber.
//!
//! The platform-independent processing code is kept separate from Windows
//! capture, networking, and the GUI so it can be tested deterministically.

pub mod audio;
pub mod config;
pub mod realtime;
pub mod recording;
pub mod transcript;
pub mod ui;

pub const REALTIME_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";
pub const TRANSCRIPTION_MODEL: &str = "gpt-live-transcribe";
pub const SAMPLE_RATE: u32 = 24_000;
pub const FRAMES_PER_CHUNK: usize = 2_400;
pub const DEFAULT_PROMPT: &str = concat!(
    "English VATSIM air traffic control radio communications. Transcribe ",
    "aviation phraseology exactly. Preserve callsigns, runway identifiers, ",
    "headings, altitudes, flight levels, frequencies, squawk codes, waypoint ",
    "names, registrations, and clearances. Do not invent missing speech."
);
