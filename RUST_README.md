# VATSIM Live Transcriber — Rust edition

## Requirements

- 64-bit Windows 10 or Windows 11.
- An OpenAI API key with transcription API access.
- A Windows output device carrying the VATSIM radio audio.

No Python installation, Rust toolchain, Visual Studio, or Visual C++
redistributable is required to run the packaged executable.

## Start

Double-click `vatsim-live-transcriber.exe` or start it from PowerShell. Select
the output device, channel, and accuracy, enter the API key, and open the
transcriber. Press **Start** when ready.

For vPilot routed through VB-Audio Virtual Cable, use:

- Device: `CABLE In 16ch (VB-Audio Virtual Cable)`
- Channel: `Left`
- Accuracy: `xhigh` for maximum transcription accuracy

The API key is kept in memory for the current run and is not written to disk.

## Output

Final transcripts are saved under `transcripts`. Every detected transmission
is saved as a mono 24 kHz WAV file in the matching `-audio` directory. Use the
**Play** button beside a completed row to replay it.

Edit `keywords.txt` before launching to change the aviation vocabulary hints.
