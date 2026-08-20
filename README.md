# VATSIM Live Transcriber

A native Windows GUI that captures an output device through WASAPI loopback and
streams one selected stereo channel to OpenAI `gpt-live-transcribe`.

> **Rust conversion preview:** The native Rust rewrite lives alongside the
> Python application on the conversion branch. The Python `run.cmd` remains the
> established version until parity testing is complete.

## Run the Python version

Open PowerShell and set your API key for that window:

```powershell
$env:OPENAI_API_KEY = "your-key"
```

Run:

```powershell
.\run.cmd
```

The first run offers to install the `uv` Python manager and creates a private
Python environment inside this folder. The app then opens a setup window for
any device, channel, accuracy, or API-key setting not supplied on the command
line.

Choose **Left** or **Right** when the source contains two independent radio
channels. **Mix** combines them and can reduce recognition accuracy when both
channels contain speech.

Use **Start** to connect and begin listening, and **Stop** to disconnect. You can
start a fresh session again without reopening the app. Finalized turns are
written to the `transcripts` folder. Each detected turn is also saved as a mono
24 kHz WAV file in the session's `-audio` folder. Every completed transcript row
has its own **Play** button for replaying that transmission.

## Run the Rust version

The prebuilt standalone executable is located at:

```text
dist\vatsim-live-transcriber-rust-0.1.0-windows-x64\vatsim-live-transcriber.exe
```

It runs on 64-bit Windows 10 or Windows 11. No Python installation, Rust
toolchain, Visual Studio, or Visual C++ redistributable is required. Double-click
the executable in File Explorer, or run it from PowerShell:

```powershell
& ".\dist\vatsim-live-transcriber-rust-0.1.0-windows-x64\vatsim-live-transcriber.exe"
```

The API key is kept in memory for the current run and is not written to disk.

### Build and run from source

Building the Rust version requires the Rust MSVC toolchain and Visual Studio C++
Build Tools. From PowerShell, run:

```powershell
.\run-rust.cmd
```

This builds an optimized executable, updates the runnable copy under `dist`,
and opens that exact `dist` executable. The file you test is therefore also the
file ready to commit.

Start with the known vPilot virtual cable and highest accuracy:

```powershell
.\run-rust.cmd --device "CABLE In 16ch" --channel left --accuracy xhigh
```

List output devices without opening the GUI:

```powershell
cargo run -- --list-devices
```

For a quick development build that does not update `dist`:

```powershell
cargo run -- --device "CABLE In 16ch" --channel left --accuracy xhigh
```

Before updating the committed executable, format, lint, and test the source:

```powershell
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

## Useful options

List capture devices:

```powershell
.\run.cmd --list-devices
```

Start without interactive device/channel questions:

```powershell
.\run.cmd --device 1 --channel left --accuracy medium
```

Example for vPilot routed through VB-Audio Virtual Cable:

```powershell
.\run.cmd --device "CABLE In 16ch" --channel left --accuracy medium
```

Accuracy choices are `minimal`, `low`, `medium`, `high`, and `xhigh`.
For difficult radio audio, start with `medium` or `high`.

As live transcript deltas arrive, the GUI progressively updates the original
white line prefixed with `> ` and its digit-normalized green line underneath.
When that transmission finishes, both are replaced with the authoritative final
versions. For example,
`one one eight decimal five zero five` becomes `118.505`, and
`Speedbird one two three` becomes `Speedbird 123`. Both finalized versions are
saved as plain text in the transcript log. Number conversion also applies to
headings, flight levels, runway numbers, squawks, QNH values, and altitudes.

Edit `keywords.txt` before starting to add current callsigns, airports,
frequencies, waypoints, SIDs and STARs. The API key is read from the
`OPENAI_API_KEY` environment variable or requested without being saved.

## What is captured

WASAPI loopback captures everything being played through the selected Windows
output device. Windows does not isolate one application's audio in ordinary
device loopback mode. To isolate VATSIM, route it to a dedicated output or
virtual audio device, then select that device here.
