import unittest

import numpy as np

from app import (
    LocalVad,
    REALTIME_URL,
    TRANSCRIPTION_MODEL,
    float_to_pcm16,
    select_channel,
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


if __name__ == "__main__":
    unittest.main()
