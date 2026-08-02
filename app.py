from __future__ import annotations

import argparse
import base64
from collections import deque
import json
import os
from pathlib import Path
import re
import sys
import threading
from datetime import datetime
from typing import Any, Protocol

import numpy as np


# Realtime transcription accepts mono PCM16 audio at 24 kHz. Keeping capture
# chunks at 100 ms provides frequent updates without excessive WebSocket traffic.
REALTIME_URL = "wss://api.openai.com/v1/realtime?intent=transcription"
TRANSCRIPTION_MODEL = "gpt-live-transcribe"
SAMPLE_RATE = 24_000
FRAMES_PER_CHUNK = 2_400  # 100 ms
DEFAULT_PROMPT = (
    "English VATSIM air traffic control radio communications. Transcribe aviation "
    "phraseology exactly. Preserve callsigns, runway identifiers, headings, altitudes, "
    "flight levels, frequencies, squawk codes, waypoint names, registrations, and "
    "clearances. Do not invent missing speech."
)


class TranscriptEventSink(Protocol):
    """Thread-safe destination for transcription and connection events."""

    def connected(self) -> None: ...

    def delta(self, item_id: str, text: str) -> None: ...

    def completed(self, item_id: str, original: str, converted: str) -> None: ...

    def error(self, message: str) -> None: ...

    def stopped(self, message: str) -> None: ...

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
    """Resolve a command-line device number or partial name."""
    if not devices:
        raise RuntimeError(
            "No WASAPI loopback devices were found. Check that Windows has an enabled "
            "audio output device."
        )

    if not requested:
        raise ValueError("No device was selected.")
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


def show_startup_dialog(
    *,
    devices: list[Any],
    selected_device: Any | None,
    channel: str | None,
    accuracy: str,
    api_key: str,
) -> tuple[Any, str, str, str] | None:
    """Collect missing startup settings in a modal GUI dialog."""
    from PySide6.QtWidgets import (
        QComboBox,
        QDialog,
        QDialogButtonBox,
        QFormLayout,
        QLabel,
        QLineEdit,
        QVBoxLayout,
    )

    dialog = QDialog()
    dialog.setWindowTitle("VATSIM Live Transcriber")
    dialog.setMinimumWidth(560)
    layout = QVBoxLayout(dialog)
    title = QLabel("Start live transcription")
    title.setStyleSheet("font-size: 18px; font-weight: 600; margin-bottom: 8px;")
    layout.addWidget(title)
    form = QFormLayout()
    layout.addLayout(form)

    device_box = QComboBox()
    device_box.addItems([device.name for device in devices])
    initial_index = 0
    if selected_device is not None:
        initial_index = next(
            (
                index
                for index, device in enumerate(devices)
                if device is selected_device
            ),
            0,
        )
    device_box.setCurrentIndex(initial_index)
    form.addRow("Audio device", device_box)

    channel_box = QComboBox()
    channel_box.addItems(["left", "right", "mix"])
    channel_box.setCurrentText(channel or "left")
    form.addRow("Channel", channel_box)

    accuracy_box = QComboBox()
    accuracy_box.addItems(["minimal", "low", "medium", "high", "xhigh"])
    accuracy_box.setCurrentText(accuracy)
    form.addRow("Accuracy", accuracy_box)

    key_entry = QLineEdit(api_key)
    key_entry.setEchoMode(QLineEdit.EchoMode.Password)
    form.addRow("OpenAI API key", key_entry)

    message = QLabel()
    message.setStyleSheet("color: #ef4444;")
    layout.addWidget(message)

    def start() -> None:
        key = key_entry.text().strip()
        if not key:
            message.setText("An OpenAI API key is required.")
            key_entry.setFocus()
            return
        dialog.accept()

    buttons = QDialogButtonBox(
        QDialogButtonBox.StandardButton.Cancel
        | QDialogButtonBox.StandardButton.Ok
    )
    buttons.button(QDialogButtonBox.StandardButton.Ok).setText("Start")
    buttons.accepted.connect(start)
    buttons.rejected.connect(dialog.reject)
    layout.addWidget(buttons)
    key_entry.setFocus()
    if dialog.exec() != QDialog.DialogCode.Accepted:
        return None
    return (
        devices[device_box.currentIndex()],
        channel_box.currentText(),
        accuracy_box.currentText(),
        key_entry.text().strip(),
    )


def create_transcript_window(
    *,
    device_name: str,
    channel: str,
    accuracy: str,
    output_path: Path,
) -> Any:
    """Create the Qt transcript window and its thread-safe event signals."""
    from PySide6.QtCore import Qt, Signal, Slot
    from PySide6.QtGui import QColor, QFont, QTextCharFormat, QTextCursor
    from PySide6.QtWidgets import (
        QHBoxLayout,
        QLabel,
        QMainWindow,
        QPlainTextEdit,
        QPushButton,
        QVBoxLayout,
        QWidget,
    )

    class TranscriptWindow(QMainWindow):
        connected_signal = Signal()
        delta_signal = Signal(str, str)
        completed_signal = Signal(str, str, str)
        error_signal = Signal(str)
        stopped_signal = Signal(str)

        def __init__(self) -> None:
            super().__init__()
            self.live_cursors: dict[str, QTextCursor] = {}
            self.start_callback: Any | None = None
            self.stop_callback: Any | None = None
            self.setWindowTitle("VATSIM Live Transcriber")
            self.resize(1000, 650)
            self.setMinimumSize(650, 380)

            central = QWidget()
            self.setCentralWidget(central)
            layout = QVBoxLayout(central)
            layout.setContentsMargins(18, 16, 18, 12)
            layout.setSpacing(10)

            title = QLabel("VATSIM Live Transcriber")
            title.setStyleSheet("font-size: 20px; font-weight: 650;")
            layout.addWidget(title)
            details = QLabel(
                f"{device_name}  |  {channel.title()}  |  "
                f"{accuracy.title()} accuracy"
            )
            details.setStyleSheet("color: #9ca3af;")
            layout.addWidget(details)

            toolbar = QHBoxLayout()
            self.status = QLabel("Ready - press Start")
            self.status.setStyleSheet("color: #9ca3af; font-weight: 600;")
            toolbar.addWidget(self.status)
            toolbar.addStretch()
            self.clear_button = QPushButton("Clear")
            self.clear_button.clicked.connect(self._clear)
            self.clear_button.setEnabled(False)
            toolbar.addWidget(self.clear_button)
            self.start_button = QPushButton("Start")
            self.start_button.clicked.connect(self._request_start)
            toolbar.addWidget(self.start_button)
            self.stop_button = QPushButton("Stop")
            self.stop_button.clicked.connect(self._request_stop)
            self.stop_button.setEnabled(False)
            toolbar.addWidget(self.stop_button)
            layout.addLayout(toolbar)

            self.text = QPlainTextEdit()
            self.text.setReadOnly(True)
            self.text.setLineWrapMode(QPlainTextEdit.LineWrapMode.WidgetWidth)
            self.text.setFont(QFont("Consolas", 13))
            self.text.setStyleSheet(
                "QPlainTextEdit {"
                "background: #030712; color: #f3f4f6; border: 1px solid #1f2937;"
                "padding: 12px; selection-background-color: #374151;"
                "}"
            )
            layout.addWidget(self.text, 1)

            path_label = QLabel(f"Transcript: {output_path}")
            path_label.setStyleSheet("color: #6b7280;")
            layout.addWidget(path_label)

            self.original_format = QTextCharFormat()
            self.original_format.setForeground(QColor("#f3f4f6"))
            self.converted_format = QTextCharFormat()
            self.converted_format.setForeground(QColor("#22c55e"))

            self.setStyleSheet(
                "QMainWindow, QWidget { background: #0b1220; color: #f9fafb; }"
                "QPushButton { background: #1f2937; border: 1px solid #374151;"
                "padding: 6px 14px; border-radius: 4px; }"
                "QPushButton:hover { background: #374151; }"
                "QPushButton:disabled { background: #111827; color: #4b5563;"
                "border-color: #1f2937; }"
            )

            queued = Qt.ConnectionType.QueuedConnection
            self.connected_signal.connect(self._connected, queued)
            self.delta_signal.connect(self._show_delta, queued)
            self.completed_signal.connect(self._show_completed, queued)
            self.error_signal.connect(self._show_error, queued)
            self.stopped_signal.connect(self._show_stopped, queued)

        def set_session_callbacks(
            self, start_callback: Any, stop_callback: Any
        ) -> None:
            self.start_callback = start_callback
            self.stop_callback = stop_callback

        # Emitting Qt signals is safe from the WebSocket worker thread. Qt
        # queues each update for execution on the GUI thread.
        def connected(self) -> None:
            self.connected_signal.emit()

        def delta(self, item_id: str, text: str) -> None:
            self.delta_signal.emit(item_id, text)

        def completed(
            self, item_id: str, original: str, converted: str
        ) -> None:
            self.completed_signal.emit(item_id, original, converted)

        def error(self, message: str) -> None:
            self.error_signal.emit(message)

        def stopped(self, message: str) -> None:
            self.stopped_signal.emit(message)

        @Slot()
        def _connected(self) -> None:
            self.status.setText("Connected - waiting for radio audio")
            self.status.setStyleSheet("color: #22c55e; font-weight: 600;")
            self.start_button.setEnabled(False)
            self.stop_button.setEnabled(True)

        def _new_live_cursor(self) -> QTextCursor:
            cursor = self.text.textCursor()
            cursor.movePosition(QTextCursor.MoveOperation.End)
            if not self.text.document().isEmpty():
                cursor.insertText("\n", self.original_format)
            return cursor

        @Slot(str, str)
        def _show_delta(self, item_id: str, text: str) -> None:
            cursor = self.live_cursors.pop(item_id, None)
            if cursor is None:
                cursor = self._new_live_cursor()
            else:
                cursor.removeSelectedText()
            start = cursor.position()
            original = text.lstrip()
            cursor.insertText(f"> {original}\n", self.original_format)
            cursor.insertText(
                normalize_spoken_numbers(original),
                self.converted_format,
            )
            end = cursor.position()
            cursor.setPosition(start)
            cursor.setPosition(end, QTextCursor.MoveMode.KeepAnchor)
            self.live_cursors[item_id] = cursor
            self.clear_button.setEnabled(True)
            self.text.moveCursor(QTextCursor.MoveOperation.End)
            self.text.ensureCursorVisible()

        @Slot(str, str, str)
        def _show_completed(
            self, item_id: str, original: str, converted: str
        ) -> None:
            cursor = self.live_cursors.pop(item_id, None)
            if cursor is None:
                cursor = self._new_live_cursor()
            else:
                cursor.removeSelectedText()
            cursor.insertText(f"> {original}\n", self.original_format)
            cursor.insertText(f"{converted}\n", self.converted_format)
            self.clear_button.setEnabled(True)
            self.text.moveCursor(QTextCursor.MoveOperation.End)
            self.text.ensureCursorVisible()

        @Slot()
        def _clear(self) -> None:
            self.text.clear()
            self.live_cursors.clear()
            self.clear_button.setEnabled(False)

        @Slot()
        def _request_start(self) -> None:
            self.status.setText("Connecting...")
            self.status.setStyleSheet("color: #fbbf24; font-weight: 600;")
            self.start_button.setEnabled(False)
            self.stop_button.setEnabled(True)
            if self.start_callback is not None:
                self.start_callback()

        @Slot()
        def _request_stop(self) -> None:
            self.status.setText("Stopping...")
            self.status.setStyleSheet("color: #fbbf24; font-weight: 600;")
            self.stop_button.setEnabled(False)
            if self.stop_callback is not None:
                self.stop_callback()

        @Slot(str)
        def _show_error(self, message: str) -> None:
            self.status.setText(message)
            self.status.setStyleSheet("color: #ef4444; font-weight: 600;")
            self.start_button.setEnabled(True)
            self.stop_button.setEnabled(False)

        @Slot(str)
        def _show_stopped(self, message: str) -> None:
            self.status.setText(message)
            self.status.setStyleSheet("color: #9ca3af; font-weight: 600;")
            self.start_button.setEnabled(True)
            self.stop_button.setEnabled(False)

        def closeEvent(self, event: Any) -> None:
            if self.stop_callback is not None:
                self.stop_callback()
            super().closeEvent(event)

    return TranscriptWindow()


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
        event_sink: TranscriptEventSink,
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
        self.event_sink = event_sink
        # The WebSocket callbacks and audio capture run on separate threads.
        self.connected = threading.Event()
        self.stop_requested = threading.Event()
        self.audio_thread: threading.Thread | None = None
        self.streamed_text_for: dict[str, str] = {}
        self.log_lock = threading.Lock()
        self.error_reported = False
        self.stopped_reported = False

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
        self.event_sink.connected()
        # Audio capture would block the WebSocket callback loop, so it runs in a
        # daemon thread and sends events through the shared WebSocket object.
        self.audio_thread = threading.Thread(
            target=self._capture_audio,
            name="wasapi-capture",
            daemon=True,
        )
        self.audio_thread.start()

    def _report_error(self, message: str) -> None:
        self.error_reported = True
        self.event_sink.error(message)

    def _report_stopped(self) -> None:
        if not self.error_reported and not self.stopped_reported:
            self.stopped_reported = True
            self.event_sink.stopped("Stopped - press Start to reconnect")

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
            self._report_error(f"Audio capture failed: {exc}")
            self.stop_requested.set()
            self.ws.close()

    def _on_message(self, ws: Any, message: str) -> None:
        try:
            event = json.loads(message)
        except json.JSONDecodeError:
            self._report_error("The server returned an unreadable message.")
            return

        event_type = event.get("type", "")
        if event_type == "conversation.item.input_audio_transcription.delta":
            # The GUI replaces one live text range as each delta arrives.
            item_id = str(event.get("item_id", ""))
            delta = str(event.get("delta", ""))
            if delta:
                previous_text = self.streamed_text_for.get(item_id, "")
                streamed_text = previous_text + delta
                self.streamed_text_for[item_id] = streamed_text
                self.event_sink.delta(item_id, streamed_text)
            return

        if event_type == "conversation.item.input_audio_transcription.completed":
            # Completed events contain the authoritative text written to disk.
            item_id = str(event.get("item_id", ""))
            raw_transcript = str(event.get("transcript", "")).strip()
            variants = transcript_variants(raw_transcript)
            self.streamed_text_for.pop(item_id, "")
            if variants:
                self.event_sink.completed(item_id, variants[0], variants[1])
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
            self._report_error(f"OpenAI API error: {message_text}")
            self.stop_requested.set()
            ws.close()

    def _on_error(self, ws: Any, error: Any) -> None:
        if not self.stop_requested.is_set():
            self._report_error(f"WebSocket error: {error}")

    def _on_close(self, ws: Any, status_code: Any, message: Any) -> None:
        self.stop_requested.set()
        if status_code and status_code != 1000:
            self._report_error(
                f"Connection closed ({status_code}): "
                f"{message or 'no reason supplied'}"
            )
        else:
            self._report_stopped()

    def stop(self) -> None:
        """Request a clean stop from the GUI thread."""
        self.stop_requested.set()
        self.ws.close()

    def run(self) -> None:
        """Run until the GUI requests a stop or the WebSocket closes."""
        try:
            self.ws.run_forever(ping_interval=20, ping_timeout=10)
        except Exception as exc:
            if not self.stop_requested.is_set():
                self._report_error(f"Transcription failed: {exc}")
        finally:
            self.stop_requested.set()
            self.ws.close()
            if self.audio_thread and self.audio_thread.is_alive():
                self.audio_thread.join(timeout=2)
            self._report_stopped()


def main() -> int:
    from PySide6.QtWidgets import QApplication, QMessageBox

    args = parse_args()
    application = QApplication(sys.argv)
    application.setApplicationName("VATSIM Live Transcriber")
    application.setStyle("Fusion")

    def fail(message: str, code: int = 2) -> int:
        QMessageBox.critical(None, "VATSIM Live Transcriber", message)
        return code

    if not 0 < args.vad_rms <= 1:
        return fail("--vad-rms must be greater than 0 and no greater than 1.")
    if args.silence_ms < 100:
        return fail("--silence-ms must be at least 100.")

    try:
        # SoundCard exposes Windows playback endpoints as WASAPI loopback
        # microphones, allowing the program to capture CABLE In from vPilot.
        import soundcard as sc
    except ImportError:
        return fail("SoundCard is not installed. Start the app through run.cmd.")

    loopbacks = [
        microphone
        for microphone in sc.all_microphones(include_loopback=True)
        if getattr(microphone, "isloopback", False)
    ]

    if args.list_devices:
        if not loopbacks:
            return fail("No WASAPI loopback devices were found.", 1)
        QMessageBox.information(
            None,
            "Windows output devices",
            "\n".join(
                f"{index}. {device.name}"
                for index, device in enumerate(loopbacks, start=1)
            ),
        )
        return 0

    if not loopbacks:
        return fail(
            "No WASAPI loopback devices were found. Check that Windows has an "
            "enabled audio output device."
        )

    api_key = os.environ.get("OPENAI_API_KEY", "").strip()
    try:
        selected_device = (
            choose_device(loopbacks, args.device) if args.device else None
        )
        keywords = load_keywords(args.keywords)
        prompt = load_prompt(args.prompt_file)
    except (OSError, RuntimeError, ValueError) as exc:
        return fail(f"Configuration error: {exc}")

    channel = args.channel
    accuracy = args.accuracy
    if selected_device is None or channel is None or not api_key:
        settings = show_startup_dialog(
            devices=loopbacks,
            selected_device=selected_device,
            channel=channel,
            accuracy=accuracy,
            api_key=api_key,
        )
        if settings is None:
            return 0
        selected_device, channel, accuracy, api_key = settings
    device = selected_device

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

    window = create_transcript_window(
        device_name=device.name,
        channel=channel,
        accuracy=accuracy,
        output_path=output_path,
    )
    active_transcriber: LiveTranscriber | None = None
    active_worker: threading.Thread | None = None

    def start_transcription() -> None:
        nonlocal active_transcriber, active_worker
        if active_worker is not None and active_worker.is_alive():
            window.error("The previous session is still stopping. Try again.")
            return

        active_transcriber = LiveTranscriber(
            api_key=api_key,
            device=device,
            channel=channel,
            accuracy=accuracy,
            prompt=prompt,
            keywords=keywords,
            output_path=output_path,
            vad_rms=args.vad_rms,
            silence_ms=args.silence_ms,
            event_sink=window,
        )
        active_worker = threading.Thread(
            target=active_transcriber.run,
            name="openai-transcription",
            daemon=True,
        )
        active_worker.start()

    def stop_transcription() -> None:
        if active_transcriber is not None:
            active_transcriber.stop()

    window.set_session_callbacks(start_transcription, stop_transcription)
    window.show()
    exit_code = application.exec()
    stop_transcription()
    if active_worker is not None:
        active_worker.join(timeout=2)
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
