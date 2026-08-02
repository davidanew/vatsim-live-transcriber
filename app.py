from __future__ import annotations

import argparse
import base64
from collections import deque
import getpass
import json
import os
from pathlib import Path
import re
import sys
import threading
from datetime import datetime
from typing import Any

import numpy as np


# Realtime transcription accepts mono PCM16 audio at 24 kHz. Keeping capture
# chunks at 100 ms provides frequent updates without excessive WebSocket traffic.
REALTIME_URL = "wss://api.openai.com/v1/realtime?intent=transcription"
TRANSCRIPTION_MODEL = "gpt-live-transcribe"
SAMPLE_RATE = 24_000
FRAMES_PER_CHUNK = 2_400  # 100 ms
TERMINAL_GREEN = "\033[32m"
TERMINAL_RESET = "\033[0m"
DEFAULT_PROMPT = (
    "English VATSIM air traffic control radio communications. Transcribe aviation "
    "phraseology exactly. Preserve callsigns, runway identifiers, headings, altitudes, "
    "flight levels, frequencies, squawk codes, waypoint names, registrations, and "
    "clearances. Do not invent missing speech."
)

# Spoken-number aliases include common ICAO pronunciations. The API often
# returns these as words, so finalized transcripts are normalized locally.
SPOKEN_DIGITS = {
    "zero": "0",
    "oh": "0",
    "nought": "0",
    "nil": "0",
    "one": "1",
    "two": "2",
    "three": "3",
    "tree": "3",
    "four": "4",
    "fower": "4",
    "five": "5",
    "fife": "5",
    "six": "6",
    "seven": "7",
    "eight": "8",
    "nine": "9",
    "niner": "9",
    "nineer": "9",
}
CARDINAL_VALUES = {
    **{word: int(digit) for word, digit in SPOKEN_DIGITS.items()},
    "ten": 10,
    "eleven": 11,
    "twelve": 12,
    "thirteen": 13,
    "fourteen": 14,
    "fifteen": 15,
    "sixteen": 16,
    "seventeen": 17,
    "eighteen": 18,
    "nineteen": 19,
    "twenty": 20,
    "thirty": 30,
    "forty": 40,
    "fifty": 50,
    "sixty": 60,
    "seventy": 70,
    "eighty": 80,
    "ninety": 90,
}
NUMBER_SCALES = {"hundred", "thousand", "million"}
NUMBER_CONNECTORS = {"and", "decimal", "point"}
NUMBER_REPEATS = {"double", "triple"}
_NUMBER_START_WORDS = set(CARDINAL_VALUES) | NUMBER_REPEATS
_NUMBER_WORDS = (
    _NUMBER_START_WORDS | NUMBER_SCALES | NUMBER_CONNECTORS
)
_NUMBER_START_PATTERN = "|".join(
    re.escape(word) for word in sorted(_NUMBER_START_WORDS, key=len, reverse=True)
)
_NUMBER_WORD_PATTERN = "|".join(
    re.escape(word) for word in sorted(_NUMBER_WORDS, key=len, reverse=True)
)
_SPOKEN_NUMBER_PATTERN = re.compile(
    rf"\b(?:{_NUMBER_START_PATTERN})"
    rf"(?:(?:[\s-]+)(?:{_NUMBER_WORD_PATTERN}))*\b",
    re.IGNORECASE,
)
_NUMBER_TOKEN_PATTERN = re.compile(
    rf"\b(?:{_NUMBER_WORD_PATTERN})\b",
    re.IGNORECASE,
)


def _expand_repeated_digits(tokens: list[str]) -> list[str] | None:
    """Expand phrases such as 'double seven' before number parsing."""
    expanded: list[str] = []
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token not in NUMBER_REPEATS:
            expanded.append(token)
            index += 1
            continue

        if index + 1 >= len(tokens) or tokens[index + 1] not in SPOKEN_DIGITS:
            return None
        repetitions = 2 if token == "double" else 3
        expanded.extend([tokens[index + 1]] * repetitions)
        index += 2
    return expanded


def _parse_integer_words(tokens: list[str]) -> str | None:
    """Convert digit-by-digit or cardinal number tokens to an integer string."""
    tokens = [token for token in tokens if token != "and"]
    if not tokens:
        return None

    if all(token in SPOKEN_DIGITS for token in tokens):
        return "".join(SPOKEN_DIGITS[token] for token in tokens)

    total = 0
    current = 0
    for token in tokens:
        if token in CARDINAL_VALUES:
            current += CARDINAL_VALUES[token]
        elif token == "hundred":
            current = max(1, current) * 100
        elif token == "thousand":
            total += max(1, current) * 1_000
            current = 0
        elif token == "million":
            total = (total + max(1, current)) * 1_000_000
            current = 0
        else:
            return None
    return str(total + current)


def _convert_number_tokens(tokens: list[str]) -> str | None:
    """Convert one matched sequence of spoken-number tokens."""
    expanded = _expand_repeated_digits(tokens)
    if expanded is None:
        return None
    tokens = expanded

    decimal_positions = [
        index for index, token in enumerate(tokens) if token in {"decimal", "point"}
    ]
    if decimal_positions:
        if len(decimal_positions) != 1:
            return None
        decimal_index = decimal_positions[0]
        whole = _parse_integer_words(tokens[:decimal_index])
        fraction_tokens = [
            token for token in tokens[decimal_index + 1 :] if token != "and"
        ]
        if whole is None or not fraction_tokens:
            return None
        if not all(token in SPOKEN_DIGITS for token in fraction_tokens):
            return None
        fraction = "".join(SPOKEN_DIGITS[token] for token in fraction_tokens)
        return f"{whole}.{fraction}"

    # Without a scale word, "and" separates independent numbers rather than
    # joining them into one digit sequence: "one and two" becomes "1 and 2".
    if "and" in tokens and not any(token in NUMBER_SCALES for token in tokens):
        groups: list[list[str]] = [[]]
        for token in tokens:
            if token == "and":
                groups.append([])
            else:
                groups[-1].append(token)
        converted = [_parse_integer_words(group) for group in groups]
        if any(value is None for value in converted):
            return None
        return " and ".join(value for value in converted if value is not None)

    return _parse_integer_words(tokens)


def normalize_spoken_numbers(text: str) -> str:
    """Render spoken number phrases as digits in a finalized transcript."""

    def replace(match: re.Match[str]) -> str:
        phrase = match.group(0)
        trailing = ""

        # Preserve a dangling connector captured before ordinary prose, as in
        # "one and only", instead of accidentally deleting the word "and".
        trailing_match = re.search(
            r"(?P<separator>[\s-]+)(?P<word>and|decimal|point)$",
            phrase,
            re.IGNORECASE,
        )
        if trailing_match:
            trailing = phrase[trailing_match.start() :]
            phrase = phrase[: trailing_match.start()]

        tokens = [
            token.group(0).casefold()
            for token in _NUMBER_TOKEN_PATTERN.finditer(phrase)
        ]
        converted = _convert_number_tokens(tokens)
        if converted is None:
            return match.group(0)
        return converted + trailing

    return _SPOKEN_NUMBER_PATTERN.sub(replace, text)


def transcript_variants(raw_transcript: str) -> list[str]:
    """Return the original transcript followed by its normalized copy."""
    original = raw_transcript.strip()
    if not original:
        return []
    normalized = normalize_spoken_numbers(original)
    return [original, normalized]


def green_terminal_line(text: str) -> str:
    """Wrap one terminal line in ANSI green without affecting transcript logs."""
    return f"{TERMINAL_GREEN}{text}{TERMINAL_RESET}"


def detection_terminal_line(text: str) -> str:
    """Prefix the original text for a newly detected radio transmission."""
    return f"> {text.lstrip()}"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Live VATSIM/system-audio transcription with gpt-live-transcribe."
    )
    parser.add_argument(
        "--device",
        help="Loopback device number or part of its name. Prompts when omitted.",
    )
    parser.add_argument(
        "--channel",
        choices=("left", "right", "mix"),
        help="Stereo channel to send. Prompts when omitted.",
    )
    parser.add_argument(
        "--accuracy",
        choices=("minimal", "low", "medium", "high", "xhigh"),
        default="medium",
        help="Transcription accuracy setting (default: medium).",
    )
    parser.add_argument(
        "--keywords",
        type=Path,
        default=Path(__file__).with_name("keywords.txt"),
        help="Text file containing one keyword per line.",
    )
    parser.add_argument(
        "--prompt-file",
        type=Path,
        help="Optional UTF-8 text file replacing the default ATC prompt.",
    )
    parser.add_argument(
        "--output",
        type=Path,
        help="Transcript log path (default: transcripts/<timestamp>.txt).",
    )
    parser.add_argument(
        "--list-devices",
        action="store_true",
        help="List Windows loopback devices and exit.",
    )
    parser.add_argument(
        "--vad-rms",
        "--vad-threshold",
        dest="vad_rms",
        type=float,
        default=0.008,
        help="Local RMS level that starts/continues a transmission (default: 0.008).",
    )
    parser.add_argument(
        "--silence-ms",
        type=int,
        default=650,
        help="Silence that ends a radio turn (default: 650 ms).",
    )
    return parser.parse_args()


def load_keywords(path: Path) -> list[str]:
    """Load a case-insensitive, de-duplicated list of transcription hints."""
    if not path.exists():
        return []

    keywords: list[str] = []
    seen: set[str] = set()
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        keyword = raw_line.strip()
        if not keyword or keyword.startswith("#"):
            continue
        if any(character in keyword for character in ("<", ">", "\r", "\n")):
            raise ValueError(f"Invalid keyword: {keyword!r}")
        key = keyword.casefold()
        if key not in seen:
            seen.add(key)
            keywords.append(keyword)
    return keywords


def load_prompt(path: Path | None) -> str:
    """Return the built-in ATC prompt or a normalized user-supplied prompt."""
    if path is None:
        return DEFAULT_PROMPT
    prompt = path.read_text(encoding="utf-8").strip()
    if not prompt:
        raise ValueError("The prompt file is empty.")
    return " ".join(prompt.splitlines())


def select_channel(samples: np.ndarray, channel: str) -> np.ndarray:
    """Select one radio channel or mix all channels down to mono."""
    audio = np.asarray(samples)
    if audio.ndim == 1:
        return audio.astype(np.float32, copy=False)
    if audio.ndim != 2 or audio.shape[1] < 1:
        raise ValueError(f"Unexpected audio shape: {audio.shape}")

    if channel == "left":
        selected = audio[:, 0]
    elif channel == "right":
        if audio.shape[1] < 2:
            raise ValueError("The selected device is mono; it has no right channel.")
        selected = audio[:, 1]
    elif channel == "mix":
        selected = audio.mean(axis=1)
    else:
        raise ValueError(f"Unknown channel: {channel}")
    return selected.astype(np.float32, copy=False)


def float_to_pcm16(samples: np.ndarray) -> bytes:
    """Convert SoundCard's normalized float samples to little-endian PCM16."""
    # Clipping prevents values outside [-1, 1] from wrapping during conversion.
    clipped = np.clip(np.asarray(samples, dtype=np.float32), -1.0, 1.0)
    return (clipped * 32767.0).astype("<i2").tobytes()


class LocalVad:
    """Simple energy-based voice activity detector for radio transmissions.

    The detector holds a short rolling prefix so the beginning of a callsign is
    retained when the signal first crosses the configured RMS threshold.
    """

    def __init__(
        self,
        *,
        threshold: float,
        silence_ms: int,
        chunk_ms: int = 100,
        prefix_ms: int = 300,
    ) -> None:
        self.threshold = threshold
        self.required_silent_chunks = max(1, round(silence_ms / chunk_ms))
        # The prefix buffer contains audio immediately before speech detection.
        self.prefix: deque[bytes] = deque(maxlen=max(1, round(prefix_ms / chunk_ms)))
        self.active = False
        self.silent_chunks = 0

    def process(self, pcm: bytes, rms: float) -> tuple[list[bytes], bool]:
        """Return chunks to send and whether the current turn should be committed."""
        if not self.active:
            self.prefix.append(pcm)
            if rms < self.threshold:
                return [], False

            self.active = True
            self.silent_chunks = 0
            chunks = list(self.prefix)
            self.prefix.clear()
            return chunks, False

        chunks = [pcm]
        if rms >= self.threshold:
            self.silent_chunks = 0
            return chunks, False

        self.silent_chunks += 1
        if self.silent_chunks < self.required_silent_chunks:
            return chunks, False

        self.active = False
        self.silent_chunks = 0
        self.prefix.clear()
        return chunks, True


def choose_device(devices: list[Any], requested: str | None) -> Any:
    if not devices:
        raise RuntimeError(
            "No WASAPI loopback devices were found. Check that Windows has an enabled "
            "audio output device."
        )

    if requested:
        if requested.isdigit():
            index = int(requested)
            if 1 <= index <= len(devices):
                return devices[index - 1]
            raise ValueError(f"Device number must be between 1 and {len(devices)}.")

        matches = [
            device for device in devices if requested.casefold() in device.name.casefold()
        ]
        if len(matches) == 1:
            return matches[0]
        if not matches:
            raise ValueError(f"No loopback device contains {requested!r}.")
        raise ValueError(f"More than one loopback device contains {requested!r}.")

    print("\nWindows output devices:")
    for index, device in enumerate(devices, start=1):
        print(f"  {index}. {device.name}")

    while True:
        value = input(f"Choose device [1-{len(devices)}]: ").strip()
        if value.isdigit() and 1 <= int(value) <= len(devices):
            return devices[int(value) - 1]
        print("Enter one of the displayed numbers.")


def choose_channel(requested: str | None) -> str:
    if requested:
        return requested
    print("\nAudio channel:")
    print("  1. Left")
    print("  2. Right")
    print("  3. Mix both channels")
    mapping = {"1": "left", "2": "right", "3": "mix"}
    while True:
        value = input("Choose channel [1-3]: ").strip()
        if value in mapping:
            return mapping[value]
        print("Enter 1, 2, or 3.")


class LiveTranscriber:
    """Connect Windows loopback capture to an OpenAI transcription session."""

    def __init__(
        self,
        *,
        api_key: str,
        device: Any,
        channel: str,
        accuracy: str,
        prompt: str,
        keywords: list[str],
        output_path: Path,
        vad_rms: float,
        silence_ms: int,
    ) -> None:
        # Imported lazily so argument/help and unit-test code can load without
        # creating a WebSocket dependency at module import time.
        import websocket

        self.websocket_module = websocket
        self.device = device
        self.channel = channel
        self.accuracy = accuracy
        self.prompt = prompt
        self.keywords = keywords
        self.output_path = output_path
        self.vad_rms = vad_rms
        self.silence_ms = silence_ms
        # The WebSocket callbacks and audio capture run on separate threads.
        self.connected = threading.Event()
        self.stop_requested = threading.Event()
        self.audio_thread: threading.Thread | None = None
        self.streamed_text_for: dict[str, str] = {}
        self.log_lock = threading.Lock()

        # The intent query selects a dedicated transcription session. Passing a
        # Realtime conversation model here creates an incompatible session type.
        self.ws = websocket.WebSocketApp(
            REALTIME_URL,
            header={"Authorization": f"Bearer {api_key}"},
            on_open=self._on_open,
            on_message=self._on_message,
            on_error=self._on_error,
            on_close=self._on_close,
        )

    def session_update(self) -> dict[str, Any]:
        """Build the initial transcription-session configuration event."""
        return {
            "type": "session.update",
            "session": {
                "type": "transcription",
                "audio": {
                    "input": {
                        "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
                        "transcription": {
                            "model": TRANSCRIPTION_MODEL,
                            "prompt": self.prompt,
                            "keywords": self.keywords,
                            "languages": ["en"],
                            # The API names this accuracy setting "delay".
                            "delay": self.accuracy,
                        },
                        # Turn boundaries are supplied by LocalVad so short radio
                        # pauses can be tuned independently of server-side VAD.
                        "turn_detection": None,
                    }
                },
            },
        }

    def _on_open(self, ws: Any) -> None:
        ws.send(json.dumps(self.session_update()))
        self.connected.set()
        print("Connected. Waiting for radio audio; press Ctrl+C to stop.\n")
        # Audio capture would block the WebSocket callback loop, so it runs in a
        # daemon thread and sends events through the shared WebSocket object.
        self.audio_thread = threading.Thread(
            target=self._capture_audio,
            name="wasapi-capture",
            daemon=True,
        )
        self.audio_thread.start()

    def _capture_audio(self) -> None:
        vad = LocalVad(
            threshold=self.vad_rms,
            silence_ms=self.silence_ms,
            chunk_ms=round(FRAMES_PER_CHUNK * 1000 / SAMPLE_RATE),
        )
        try:
            with self.device.recorder(samplerate=SAMPLE_RATE) as recorder:
                while not self.stop_requested.is_set():
                    frames = recorder.record(numframes=FRAMES_PER_CHUNK)
                    mono = select_channel(frames, self.channel)
                    pcm = float_to_pcm16(mono)
                    # RMS is an inexpensive signal-energy measurement suitable
                    # for detecting the start and end of push-to-talk audio.
                    rms = float(np.sqrt(np.mean(np.square(mono), dtype=np.float64)))
                    chunks, commit = vad.process(pcm, rms)
                    for chunk in chunks:
                        # Realtime API audio events carry base64-encoded PCM bytes.
                        event = {
                            "type": "input_audio_buffer.append",
                            "audio": base64.b64encode(chunk).decode("ascii"),
                        }
                        self.ws.send(json.dumps(event))
                    if commit:
                        # A commit asks the model to finalize this radio turn.
                        self.ws.send(json.dumps({"type": "input_audio_buffer.commit"}))
        except Exception as exc:
            print(f"\nAudio capture failed: {exc}", file=sys.stderr)
            self.stop_requested.set()
            self.ws.close()

    def _on_message(self, ws: Any, message: str) -> None:
        try:
            event = json.loads(message)
        except json.JSONDecodeError:
            print(f"\nUnexpected server message: {message}", file=sys.stderr)
            return

        event_type = event.get("type", "")
        if event_type == "conversation.item.input_audio_transcription.delta":
            # Deltas provide immediate terminal feedback while speech is decoded.
            item_id = str(event.get("item_id", ""))
            delta = str(event.get("delta", ""))
            if delta:
                previous_text = self.streamed_text_for.get(item_id, "")
                streamed_text = previous_text + delta
                self.streamed_text_for[item_id] = streamed_text
                # Each event contains only the new text. Appending that delta
                # directly keeps one live white line without cursor redrawing.
                displayed_delta = delta.lstrip() if not previous_text else delta
                sys.stdout.write(
                    f"> {displayed_delta}" if not previous_text else displayed_delta
                )
                sys.stdout.flush()
            return

        if event_type == "conversation.item.input_audio_transcription.completed":
            # Completed events contain the authoritative text written to disk.
            item_id = str(event.get("item_id", ""))
            raw_transcript = str(event.get("transcript", "")).strip()
            variants = transcript_variants(raw_transcript)
            streamed_text = self.streamed_text_for.pop(item_id, "")
            if streamed_text:
                # Finish the live white line, then show the authoritative
                # number-normalized transcript once in green.
                sys.stdout.write("\n")
                if len(variants) > 1:
                    print(green_terminal_line(variants[1]))
                sys.stdout.flush()
            else:
                # A completed event can occasionally arrive without deltas.
                if variants:
                    print(detection_terminal_line(variants[0]))
                if len(variants) > 1:
                    print(green_terminal_line(variants[1]))
            if variants:
                timestamp = datetime.now().strftime("%H:%M:%S")
                with self.log_lock:
                    with self.output_path.open("a", encoding="utf-8") as log:
                        log.write(f"[{timestamp}] {variants[0]}\n")
                        for variant in variants[1:]:
                            log.write(f"           {variant}\n")
            return

        if event_type == "error":
            error = event.get("error", {})
            message_text = error.get("message", json.dumps(event))
            print(f"\nOpenAI API error: {message_text}", file=sys.stderr)
            self.stop_requested.set()
            ws.close()

    def _on_error(self, ws: Any, error: Any) -> None:
        if not self.stop_requested.is_set():
            print(f"\nWebSocket error: {error}", file=sys.stderr)

    def _on_close(self, ws: Any, status_code: Any, message: Any) -> None:
        self.stop_requested.set()
        if status_code and status_code != 1000:
            print(
                f"\nConnection closed ({status_code}): {message or 'no reason supplied'}",
                file=sys.stderr,
            )

    def run(self) -> None:
        """Run until Ctrl+C, an API error, or the WebSocket closes."""
        try:
            self.ws.run_forever(ping_interval=20, ping_timeout=10)
        except KeyboardInterrupt:
            pass
        finally:
            self.stop_requested.set()
            self.ws.close()
            if self.audio_thread and self.audio_thread.is_alive():
                self.audio_thread.join(timeout=2)


def main() -> int:
    args = parse_args()
    if not 0 < args.vad_rms <= 1:
        print("--vad-rms must be greater than 0 and no greater than 1.", file=sys.stderr)
        return 2
    if args.silence_ms < 100:
        print("--silence-ms must be at least 100.", file=sys.stderr)
        return 2

    try:
        # SoundCard exposes Windows playback endpoints as WASAPI loopback
        # microphones, allowing the program to capture CABLE In from vPilot.
        import soundcard as sc
    except ImportError:
        print("SoundCard is not installed. Run this program through run.ps1.", file=sys.stderr)
        return 2

    loopbacks = [
        microphone
        for microphone in sc.all_microphones(include_loopback=True)
        if getattr(microphone, "isloopback", False)
    ]

    if args.list_devices:
        if not loopbacks:
            print("No WASAPI loopback devices were found.")
            return 1
        for index, device in enumerate(loopbacks, start=1):
            print(f"{index}. {device.name}")
        return 0

    api_key = os.environ.get("OPENAI_API_KEY", "").strip()
    if not api_key:
        # getpass keeps a manually entered key out of terminal history and logs.
        api_key = getpass.getpass("OpenAI API key (not saved): ").strip()
    if not api_key:
        print("An OpenAI API key is required.", file=sys.stderr)
        return 2

    try:
        device = choose_device(loopbacks, args.device)
        channel = choose_channel(args.channel)
        keywords = load_keywords(args.keywords)
        prompt = load_prompt(args.prompt_file)
    except (OSError, RuntimeError, ValueError) as exc:
        print(f"Configuration error: {exc}", file=sys.stderr)
        return 2

    if args.output:
        output_path = args.output.expanduser().resolve()
    else:
        # Use a new timestamped log for every run to preserve older transcripts.
        output_path = (
            Path(__file__).parent
            / "transcripts"
            / f"vatsim-{datetime.now():%Y%m%d-%H%M%S}.txt"
        )
    output_path.parent.mkdir(parents=True, exist_ok=True)

    print("\nVATSIM Live Transcriber")
    print(f"  Device : {device.name}")
    print(f"  Channel: {channel}")
    print(f"  Accuracy: {args.accuracy}")
    print("  Session type       : transcription")
    print(f"  Transcription model: {TRANSCRIPTION_MODEL}")
    print(f"  Log    : {output_path}")
    print(f"  Keywords: {len(keywords)}")

    transcriber = LiveTranscriber(
        api_key=api_key,
        device=device,
        channel=channel,
        accuracy=args.accuracy,
        prompt=prompt,
        keywords=keywords,
        output_path=output_path,
        vad_rms=args.vad_rms,
        silence_ms=args.silence_ms,
    )
    transcriber.run()
    print(f"\nStopped. Transcript saved to {output_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
