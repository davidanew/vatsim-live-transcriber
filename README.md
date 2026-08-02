# VATSIM Live Transcriber

A native Windows GUI that captures an output device through WASAPI loopback and
streams one selected stereo channel to OpenAI `gpt-live-transcribe`.

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
Python environment inside this folder. The app then opens a setup window for
any device, channel, accuracy, or API-key setting not supplied on the command
line.

Choose **Left** or **Right** when the source contains two independent radio
channels. **Mix** combines them and can reduce recognition accuracy when both
channels contain speech.

Use **Start** to connect and begin listening, and **Stop** to disconnect. You can
start a fresh session again without reopening the app. Finalized turns are
written to the `transcripts` folder.

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
