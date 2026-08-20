# Rust Conversion Plan

This plan describes how to rewrite the VATSIM Live Transcriber in Rust while
preserving the behaviour of the current Python application. The user handles
all Git commands, including commits, tags, branches, and pushes.

## Progress

- [x] Python baseline confirmed on the user-created `rust_1` branch.
- [x] Rust 1.97.1 GNU toolchain installed and verified by the user.
- [x] Cargo application and library structure created.
- [x] Spoken-number normalization ported.
- [x] Channel selection and PCM16 conversion ported.
- [x] Local VAD, keyword/prompt loading, and WAV writing ported.
- [x] Twenty-one Rust unit and protocol tests passing.
- [x] Windows WASAPI capture and device enumeration.
- [x] Recording association and playback.
- [x] OpenAI transcription WebSocket client implemented and protocol-tested.
- [x] Native GUI implemented and compiling.
- [ ] Visual GUI and live OpenAI session verification.
- [ ] End-to-end VATSIM testing and release packaging.

Implementation decision: use `eframe`/`egui` rather than the preliminary Slint
option. The existing application already uses a custom dark interface, and
`eframe` provides a permissively licensed native Windows application. The
project now targets Rust's Tier 1 MSVC Windows toolchain because its Windows SDK
and linker support are substantially simpler and more broadly tested than the
earlier GNU/LLVM experiment.

## 1. Freeze the Python baseline

- Confirm with the user that the current Python version is safely committed.
- Wait for the user to create any desired baseline tag, such as `python-final`.
- Record the expected behaviour with screenshots and sample transcripts.
- Keep `vatsim.wav` as a repeatable test input.
- Create a parity checklist covering every current feature.

This provides a stable reference for the Rust version and makes regressions
easier to identify.

## 2. Create the Rust project structure

Use a Cargo project with clearly separated modules:

```text
src/
  main.rs
  config.rs
  audio/
    mod.rs
    wasapi.rs
    pcm.rs
    vad.rs
  realtime/
    mod.rs
    protocol.rs
    session.rs
  transcript/
    mod.rs
    numbers.rs
    logging.rs
  recording.rs
  playback.rs
  ui/
    mod.rs
    startup.rs
    transcript_window.rs
```

Likely dependencies:

- `windows` for WASAPI and Windows audio playback.
- `tokio` for asynchronous tasks.
- `tokio-tungstenite` with `rustls` for WebSockets.
- `serde` and `serde_json` for the OpenAI protocol.
- `eframe`/`egui` for the GUI, using its lightweight OpenGL renderer.
- `clap` for command-line arguments.
- `hound` for WAV files.
- `base64` for streaming PCM data.
- `anyhow` or `thiserror` for error handling.

## 3. Port the testable processing logic

Start with components that do not depend on Windows or the network:

- Spoken-number conversion.
- Aviation variants such as `tree`, `fife`, `niner`, and `nineer`.
- Frequencies and decimal conversion.
- Double and triple digit expansion.
- Keyword loading and case-insensitive deduplication.
- Left, right, and mixed channel selection.
- Floating-point to PCM16 conversion.
- Local voice-activity detection.
- WAV creation.
- Transcript formatting.

Port every existing Python test to Rust and run them with `cargo test`.

## 4. Implement Windows audio capture

Build direct WASAPI loopback support:

- Enumerate Windows output devices.
- Match devices by index or partial name.
- Support `CABLE In 16ch (VB-Audio Virtual Cable)`.
- Capture the selected device's native sample format.
- Extract the left or right channel, or mix channels.
- Convert samples to mono PCM16 at 24 kHz.
- Resample when the native rate is not 24 kHz.
- Feed audio to the recorder, VAD, and OpenAI stream.
- Handle device disconnection cleanly.

Test audio capture independently before adding OpenAI communication.

## 5. Port the VAD and recording pipeline

Reproduce the existing transmission boundaries:

- Preserve the audio prefix before detected speech.
- Commit a transmission after the configured silence period.
- Keep the current 500 ms silence default.
- Record every committed transmission.
- Generate unique WAV filenames across repeated Start and Stop sessions.
- Associate recordings with OpenAI `item_id` values.
- Ensure the end of each transmission is not truncated.

## 6. Implement the OpenAI Realtime client

Reproduce the working transcription protocol:

- Connect using the transcription WebSocket URL.
- Send the transcription `session.update` event.
- Configure `gpt-live-transcribe`, English, accuracy, prompt, and keywords.
- Stream base64-encoded PCM chunks.
- Commit the input buffer at VAD boundaries.
- Process progressive transcript delta events.
- Process final transcript events.
- Match completed transcripts to recordings.
- Handle OpenAI errors, disconnects, and clean cancellation.
- Never write the API key to logs or configuration files.

Create a mock WebSocket server for repeatable protocol tests.

## 7. Recreate the GUI

Match the current application before considering redesigns:

- Startup configuration window.
- Device selector.
- Left, right, and mix selection.
- Accuracy selector.
- API-key prompt.
- Start, Stop, and Clear buttons.
- Disabled-button ghosting.
- Connection and error status.
- Progressive white original transcript.
- Green digit-normalized transcript.
- `>` prefix.
- Reliable word wrapping.
- Bottom-aligned short history.
- Auto-follow only when the user is already at the bottom.
- No forced scrolling while reviewing old messages.
- A Play button beside every completed transmission.
- Correct resizing for long transcripts.

Keep GUI work on the UI thread and communicate with the audio and network
runtime through message channels.

## 8. Add end-to-end tests

Test these complete workflows:

1. Synthetic stereo audio becomes the correct selected mono channel.
2. Speech followed by 500 ms of silence creates one committed transmission.
3. Committed audio produces a valid WAV recording.
4. Mock OpenAI deltas progressively update one row.
5. A final event displays the authoritative white and green text.
6. A recording is associated with the correct Play button.
7. Stop and restart do not duplicate identifiers or filenames.
8. Long transcripts wrap and scroll correctly.
9. A network failure produces a useful error and restores the Start button.
10. Real VATSIM audio produces output comparable with the Python version.

## 9. Package it as a Windows application

Produce:

- A release-mode standalone executable.
- No console window for ordinary GUI use.
- An optional console or debug build.
- Application icon and version information.
- `keywords.txt` beside the executable.
- A portable ZIP initially.
- Optionally, an MSI installer later.

The application should locate resources relative to the executable rather than
the current working directory.

## 10. Run both versions side by side

Before replacing Python:

- Capture the same VATSIM feed with both versions.
- Compare transmission boundaries.
- Compare recorded audio.
- Compare progressive and final transcripts.
- Measure CPU and memory usage.
- Test repeated Start and Stop cycles.
- Run an extended session lasting several hours.

Make Rust the primary version only after feature parity and stability are
confirmed.

## Recommended delivery order

1. Rust project and unit tests.
2. Spoken-number conversion.
3. WASAPI capture.
4. VAD and WAV recording.
5. OpenAI WebSocket connection.
6. Terminal-only end-to-end prototype.
7. Slint GUI.
8. Playback and scrolling.
9. Packaging.
10. Extended VATSIM testing.

The terminal prototype is an important checkpoint because it proves that audio
capture and live transcription work before GUI complexity is introduced.

## Expected outcome

The Rust application should provide the same transcription behaviour as the
Python version while offering:

- A standalone Windows executable.
- No Python, `uv`, virtual environment, or runtime dependency installation.
- Faster startup and generally lower memory usage.
- Easier distribution to other Windows users.

Changing implementation language will not inherently improve transcription
accuracy because both versions use the same OpenAI transcription model.
