use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use vatsim_live_transcriber::audio::{
    AudioDevice, ChannelSelection, LinearResampler, LocalVad, SampleChunker, float_to_pcm16,
    resolve_device, select_channel, write_pcm16_wav,
};
use vatsim_live_transcriber::config::{load_keywords, load_prompt};
use vatsim_live_transcriber::recording::RecordingTracker;
use vatsim_live_transcriber::transcript::{normalize_spoken_numbers, transcript_variants};
use vatsim_live_transcriber::{DEFAULT_PROMPT, REALTIME_URL, SAMPLE_RATE, TRANSCRIPTION_MODEL};

fn temporary_path(name: &str) -> std::path::PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should follow the Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("vatsim-rust-{unique}-{name}"))
}

#[test]
fn selects_left_channel() {
    let stereo = [0.1, 0.8, -0.2, 0.7];
    assert_eq!(
        select_channel(&stereo, 2, ChannelSelection::Left).unwrap(),
        vec![0.1, -0.2]
    );
}

#[test]
fn selects_right_channel() {
    let stereo = [0.1, 0.8, -0.2, 0.7];
    assert_eq!(
        select_channel(&stereo, 2, ChannelSelection::Right).unwrap(),
        vec![0.8, 0.7]
    );
}

#[test]
fn mixes_channels() {
    let stereo = [0.2, 0.6, -0.6, 0.2];
    let mixed = select_channel(&stereo, 2, ChannelSelection::Mix).unwrap();
    assert!((mixed[0] - 0.4).abs() < 1e-6);
    assert!((mixed[1] + 0.2).abs() < 1e-6);
}

#[test]
fn clips_pcm_samples() {
    let bytes = float_to_pcm16(&[-2.0, 0.0, 2.0]);
    let samples = bytes
        .chunks_exact(2)
        .map(|sample| i16::from_le_bytes([sample[0], sample[1]]))
        .collect::<Vec<_>>();
    assert_eq!(samples, vec![-32_767, 0, 32_767]);
}

#[test]
fn writes_mono_pcm16_wav() {
    let path = temporary_path("turn.wav");
    let pcm = [0x00, 0x00, 0xff, 0x7f, 0x01, 0x80];
    write_pcm16_wav(&path, &pcm).unwrap();
    let wav = fs::read(&path).unwrap();
    assert_eq!(&wav[0..4], b"RIFF");
    assert_eq!(&wav[8..12], b"WAVE");
    assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
    assert_eq!(
        u32::from_le_bytes(wav[24..28].try_into().unwrap()),
        SAMPLE_RATE
    );
    assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);
    assert_eq!(&wav[44..], &pcm);
    fs::remove_file(path).unwrap();
}

#[test]
fn exposes_transcription_session_constants() {
    assert_eq!(
        REALTIME_URL,
        "wss://api.openai.com/v1/realtime?intent=transcription"
    );
    assert_eq!(TRANSCRIPTION_MODEL, "gpt-live-transcribe");
}

#[test]
fn vad_adds_prefix_and_commits_after_silence() {
    let mut vad = LocalVad::new(0.1, 200, 100, 200);
    assert_eq!(vad.process(b"quiet-1", 0.0).chunks, Vec::<Vec<u8>>::new());
    assert_eq!(vad.process(b"quiet-2", 0.0).chunks, Vec::<Vec<u8>>::new());
    assert_eq!(
        vad.process(b"speech", 0.5).chunks,
        vec![b"quiet-2".to_vec(), b"speech".to_vec()]
    );
    assert!(!vad.process(b"tail-1", 0.0).commit);
    assert!(vad.process(b"tail-2", 0.0).commit);
}

#[test]
fn normalizes_frequency() {
    assert_eq!(
        normalize_spoken_numbers("contact tower one one eight decimal five zero five, bye"),
        "contact tower 118.505, bye"
    );
}

#[test]
fn normalizes_common_atc_numbers() {
    assert_eq!(
        normalize_spoken_numbers(
            "Speedbird one two three, runway two seven left, heading two seven zero, squawk seven zero zero zero"
        ),
        "Speedbird 123, runway 27 left, heading 270, squawk 7000"
    );
}

#[test]
fn normalizes_cardinal_and_icao_words() {
    assert_eq!(
        normalize_spoken_numbers(
            "climb six thousand feet, QNH one zero one three, frequency one two tree decimal fife niner zero"
        ),
        "climb 6000 feet, QNH 1013, frequency 123.590"
    );
}

#[test]
fn normalizes_double_and_triple_digits() {
    assert_eq!(
        normalize_spoken_numbers("squawk seven double zero, triple two"),
        "squawk 700, 222"
    );
}

#[test]
fn normalizes_nineer_variant() {
    assert_eq!(
        normalize_spoken_numbers("AC nineer nineer taxi left Echo hold Echo one."),
        "AC 99 taxi left Echo hold Echo 1."
    );
}

#[test]
fn keeps_original_above_normalized_variant() {
    assert_eq!(
        transcript_variants("Speedbird one two three"),
        vec!["Speedbird one two three", "Speedbird 123"]
    );
    assert_eq!(
        transcript_variants("No spoken numbers"),
        vec!["No spoken numbers", "No spoken numbers"]
    );
}

#[test]
fn loads_case_insensitive_unique_keywords() {
    let path = temporary_path("keywords.txt");
    fs::write(&path, "Speedbird\n# comment\nspeedbird\nQNH\n").unwrap();
    assert_eq!(load_keywords(&path).unwrap(), vec!["Speedbird", "QNH"]);
    fs::remove_file(path).unwrap();
}

#[test]
fn loads_default_and_multiline_prompts() {
    assert_eq!(load_prompt(None).unwrap(), DEFAULT_PROMPT);
    let path = temporary_path("prompt.txt");
    fs::write(&path, "First line\nSecond line\n").unwrap();
    assert_eq!(load_prompt(Some(&path)).unwrap(), "First line Second line");
    fs::remove_file(path).unwrap();
}

#[test]
fn resolves_audio_device_by_number_or_unique_name() {
    let devices = vec![
        AudioDevice {
            id: "one".to_owned(),
            name: "CABLE In 16ch".to_owned(),
        },
        AudioDevice {
            id: "two".to_owned(),
            name: "Speakers".to_owned(),
        },
    ];
    assert_eq!(resolve_device(&devices, "1").unwrap().id, "one");
    assert_eq!(resolve_device(&devices, "16CH").unwrap().id, "one");
    assert!(resolve_device(&devices, "missing").is_err());
}

#[test]
fn resamples_and_frames_streaming_audio() {
    let mut resampler = LinearResampler::new(48_000, 24_000).unwrap();
    let first = resampler.process(&[0.0, 0.5, 1.0, 0.5]);
    let second = resampler.process(&[0.0, -0.5, -1.0, -0.5, 0.0]);
    assert_eq!(first, vec![0.0, 1.0]);
    assert_eq!(second, vec![0.0, -1.0]);

    let mut chunker = SampleChunker::new(3).unwrap();
    assert!(chunker.push(&[1.0, 2.0]).is_empty());
    assert_eq!(
        chunker.push(&[3.0, 4.0, 5.0, 6.0]),
        vec![vec![1.0, 2.0, 3.0], vec![4.0, 5.0, 6.0]]
    );
}

#[test]
fn records_and_binds_turns_in_order() {
    let directory = temporary_path("recordings");
    let mut tracker = RecordingTracker::with_session_id(directory.clone(), "test".to_owned());
    let first = tracker.save_turn(&[0, 0]).unwrap().unwrap();
    let second = tracker.save_turn(&[1, 0]).unwrap().unwrap();
    assert!(first.ends_with("turn-test-0001.wav"));
    assert!(second.ends_with("turn-test-0002.wav"));
    assert_eq!(tracker.bind("item-one"), Some(first.clone()));
    assert_eq!(tracker.bind("item-one"), Some(first.clone()));
    assert_eq!(tracker.bind("item-two"), Some(second.clone()));
    tracker.finish("item-one");
    fs::remove_dir_all(directory).unwrap();
}
