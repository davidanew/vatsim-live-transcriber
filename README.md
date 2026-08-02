# VATSIM Live Transcriber

A native Windows terminal program that captures an output device through WASAPI
loopback and streams one selected stereo channel to OpenAI
`gpt-live-transcribe`.

## Start

Open PowerShell and set your API key for that window:

```powershell
$env:OPENAI_API_KEY = "your-key"
```

Run:

```powershell
.\run.cmd
```

The first run offers to install the `uv` Python manager and creates a private
Python environment inside this folder. It then lists Windows output devices and
asks which device and channel to use.

Choose **Left** or **Right** when the source contains two independent radio
channels. **Mix** combines them and can reduce recognition accuracy when both
channels contain speech.

Press `Ctrl+C` to stop. Finalized turns are written to the `transcripts` folder.

## Useful options

List capture devices:

```powershell
.\run.cmd --list-devices
```

Start without interactive device/channel questions:

```powershell
.\run.cmd --device 1 --channel left --delay medium
```

Example for vPilot routed through VB-Audio Virtual Cable:

```powershell
.\run.cmd --device "CABLE In 16ch" --channel left --delay medium
```

Latency/accuracy choices are `minimal`, `low`, `medium`, `high`, and `xhigh`.
For difficult radio audio, start with `medium` or `high`.

Edit `keywords.txt` before starting to add current callsigns, airports,
frequencies, waypoints, SIDs and STARs. The API key is read from the
`OPENAI_API_KEY` environment variable or requested without being saved.

## What is captured

WASAPI loopback captures everything being played through the selected Windows
output device. Windows does not isolate one application's audio in ordinary
device loopback mode. To isolate VATSIM, route it to a dedicated output or
virtual audio device, then select that device here.
