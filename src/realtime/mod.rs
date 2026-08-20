//! OpenAI Realtime transcription session and worker lifecycle.

mod protocol;
mod session;

pub use protocol::{ServerEventProcessor, TranscriptEvent, session_update};
pub use session::{TranscriberConfig, TranscriberHandle};
