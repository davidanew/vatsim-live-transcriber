import json
from pathlib import Path
import tempfile
import threading
import unittest

import numpy as np

from app import (
    LiveTranscriber,
    LocalVad,
    REALTIME_URL,
    TRANSCRIPTION_MODEL,
    float_to_pcm16,
    normalize_spoken_numbers,
    select_channel,
    transcript_variants,
)


class AudioConversionTests(unittest.TestCase):
    def test_left_channel(self):
        stereo = np.array([[0.1, 0.8], [-0.2, 0.7]], dtype=np.float32)
        np.testing.assert_allclose(select_channel(stereo, "left"), [0.1, -0.2])

    def test_right_channel(self):
        stereo = np.array([[0.1, 0.8], [-0.2, 0.7]], dtype=np.float32)
        np.testing.assert_allclose(select_channel(stereo, "right"), [0.8, 0.7])

    def test_mix_channel(self):
        stereo = np.array([[0.2, 0.6], [-0.6, 0.2]], dtype=np.float32)
        np.testing.assert_allclose(select_channel(stereo, "mix"), [0.4, -0.2])

    def test_pcm_clips_out_of_range_samples(self):
        pcm = np.frombuffer(
            float_to_pcm16(np.array([-2.0, 0.0, 2.0], dtype=np.float32)),
            dtype="<i2",
        )
        self.assertEqual(pcm.tolist(), [-32767, 0, 32767])

    def test_websocket_requests_a_transcription_session(self):
        self.assertEqual(
            REALTIME_URL,
            "wss://api.openai.com/v1/realtime?intent=transcription",
        )
        self.assertEqual(TRANSCRIPTION_MODEL, "gpt-live-transcribe")

    def test_local_vad_adds_prefix_and_commits_after_silence(self):
        vad = LocalVad(threshold=0.1, silence_ms=200, chunk_ms=100, prefix_ms=200)

        self.assertEqual(vad.process(b"quiet-1", 0.0), ([], False))
        self.assertEqual(vad.process(b"quiet-2", 0.0), ([], False))
        self.assertEqual(
            vad.process(b"speech", 0.5),
            ([b"quiet-2", b"speech"], False),
        )
        self.assertEqual(vad.process(b"tail-1", 0.0), ([b"tail-1"], False))
        self.assertEqual(vad.process(b"tail-2", 0.0), ([b"tail-2"], True))

    def test_normalizes_frequency(self):
        self.assertEqual(
            normalize_spoken_numbers(
                "contact tower one one eight decimal five zero five, bye"
            ),
            "contact tower 118.505, bye",
        )

    def test_normalizes_common_atc_numbers(self):
        self.assertEqual(
            normalize_spoken_numbers(
                "Speedbird one two three, runway two seven left, "
                "heading two seven zero, squawk seven zero zero zero"
            ),
            "Speedbird 123, runway 27 left, heading 270, squawk 7000",
        )

    def test_normalizes_cardinal_and_icao_number_words(self):
        self.assertEqual(
            normalize_spoken_numbers(
                "climb six thousand feet, QNH one zero one three, "
                "frequency one two tree decimal fife niner zero"
            ),
            "climb 6000 feet, QNH 1013, frequency 123.590",
        )

    def test_normalizes_double_and_triple_digits(self):
        self.assertEqual(
            normalize_spoken_numbers("squawk seven double zero, triple two"),
            "squawk 700, 222",
        )

    def test_normalizes_nineer_spelling_variant(self):
        self.assertEqual(
            normalize_spoken_numbers(
                "AC nineer nineer taxi left Echo hold Echo one."
            ),
            "AC 99 taxi left Echo hold Echo 1.",
        )

    def test_transcript_variants_keep_original_above_converted_text(self):
        self.assertEqual(
            transcript_variants("Speedbird one two three"),
            ["Speedbird one two three", "Speedbird 123"],
        )
        self.assertEqual(
            transcript_variants("No spoken numbers"),
            ["No spoken numbers", "No spoken numbers"],
        )

    def test_gui_receives_progressive_text_then_completed_pair(self):
        class RecordingSink:
            def __init__(self):
                self.deltas = []
                self.completions = []

            def delta(self, item_id, text):
                self.deltas.append((item_id, text))

            def completed(self, item_id, original, converted):
                self.completions.append((item_id, original, converted))

        with tempfile.TemporaryDirectory() as temp_dir:
            sink = RecordingSink()
            transcriber = LiveTranscriber.__new__(LiveTranscriber)
            transcriber.streamed_text_for = {}
            transcriber.log_lock = threading.Lock()
            transcriber.output_path = Path(temp_dir) / "transcript.txt"
            transcriber.event_sink = sink

            for delta in ("Lufthansa ", "Alpha one"):
                transcriber._on_message(
                    None,
                    json.dumps(
                        {
                            "type": (
                                "conversation.item."
                                "input_audio_transcription.delta"
                            ),
                            "item_id": "radio-turn",
                            "delta": delta,
                        }
                    ),
                )
            transcriber._on_message(
                None,
                json.dumps(
                    {
                        "type": (
                            "conversation.item."
                            "input_audio_transcription.completed"
                        ),
                        "item_id": "radio-turn",
                        "transcript": "Lufthansa Alpha one",
                    }
                ),
            )

            self.assertEqual(
                sink.deltas,
                [
                    ("radio-turn", "Lufthansa "),
                    ("radio-turn", "Lufthansa Alpha one"),
                ],
            )
            self.assertEqual(
                sink.completions,
                [
                    (
                        "radio-turn",
                        "Lufthansa Alpha one",
                        "Lufthansa Alpha 1",
                    )
                ],
            )


if __name__ == "__main__":
    unittest.main()
