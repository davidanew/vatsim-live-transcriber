use std::fs;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use vatsim_live_transcriber::realtime::{ServerEventProcessor, TranscriptEvent, session_update};
use vatsim_live_transcriber::recording::RecordingTracker;
use vatsim_live_transcriber::{REALTIME_URL, SAMPLE_RATE, TRANSCRIPTION_MODEL};

fn temporary_path(name: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("vatsim-realtime-{unique}-{name}"))
}

#[test]
fn builds_transcription_session_update() {
    let update = session_update("ATC prompt", &["Speedbird".to_owned()], "high");
    assert_eq!(update["type"], "session.update");
    assert_eq!(update["session"]["type"], "transcription");
    assert_eq!(
        update["session"]["audio"]["input"]["format"]["rate"],
        SAMPLE_RATE
    );
    assert_eq!(
        update["session"]["audio"]["input"]["transcription"]["model"],
        TRANSCRIPTION_MODEL
    );
    assert_eq!(
        update["session"]["audio"]["input"]["transcription"]["delay"],
        "high"
    );
    assert!(update["session"]["audio"]["input"]["turn_detection"].is_null());
    assert!(REALTIME_URL.contains("intent=transcription"));
}

#[test]
fn turns_deltas_and_completion_into_gui_events_and_log() {
    let root = temporary_path("session");
    let recordings_dir = root.join("audio");
    let output_path = root.join("transcript.txt");
    let mut tracker = RecordingTracker::with_session_id(recordings_dir, "test".to_owned());
    let recording = tracker.save_turn(&[0, 0, 1, 0]).unwrap().unwrap();
    let tracker = Arc::new(Mutex::new(tracker));
    let (sender, receiver) = mpsc::channel();
    let mut processor = ServerEventProcessor::new(tracker, output_path.clone(), sender);

    processor
        .process(r#"{"type":"input_audio_buffer.committed","item_id":"turn"}"#)
        .unwrap();
    processor
        .process(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"turn","delta":"Speedbird "}"#)
        .unwrap();
    processor
        .process(r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"turn","delta":"one two three"}"#)
        .unwrap();
    processor
        .process(r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"turn","transcript":"Speedbird one two three"}"#)
        .unwrap();

    let events = receiver.try_iter().collect::<Vec<_>>();
    assert_eq!(
        events[0],
        TranscriptEvent::Delta {
            item_id: "turn".to_owned(),
            original: "Speedbird ".to_owned(),
            converted: "Speedbird ".to_owned(),
        }
    );
    assert_eq!(
        events[1],
        TranscriptEvent::Delta {
            item_id: "turn".to_owned(),
            original: "Speedbird one two three".to_owned(),
            converted: "Speedbird 123".to_owned(),
        }
    );
    assert_eq!(
        events[2],
        TranscriptEvent::Completed {
            item_id: "turn".to_owned(),
            original: "Speedbird one two three".to_owned(),
            converted: "Speedbird 123".to_owned(),
            audio_path: Some(recording),
        }
    );
    let log = fs::read_to_string(&output_path).unwrap();
    assert!(log.contains("Speedbird one two three"));
    assert!(log.contains("Speedbird 123"));
    assert!(log.contains("[audio]"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn exposes_server_error_message() {
    let root = temporary_path("error");
    let tracker = Arc::new(Mutex::new(RecordingTracker::with_session_id(
        root.join("audio"),
        "test".to_owned(),
    )));
    let (sender, _receiver) = mpsc::channel();
    let mut processor = ServerEventProcessor::new(tracker, root.join("log.txt"), sender);
    let error = processor
        .process(r#"{"type":"error","error":{"message":"bad session"}}"#)
        .unwrap_err();
    assert!(error.to_string().contains("bad session"));
}
