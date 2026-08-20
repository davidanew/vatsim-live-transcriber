//! Transcript presentation and spoken-number normalization.

mod numbers;

pub use numbers::normalize_spoken_numbers;

pub fn transcript_variants(raw_transcript: &str) -> Vec<String> {
    let original = raw_transcript.trim();
    if original.is_empty() {
        return Vec::new();
    }
    vec![original.to_owned(), normalize_spoken_numbers(original)]
}
