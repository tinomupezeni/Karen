# Friday

Karen is a voice-first desktop assistant inspired by JARVIS. She listens through the system microphone, transcribes speech locally with Whisper, reasons with a streaming LLM, operates the computer through guarded tools, and replies with speech. The desktop interface is a Tauri and React dashboard with live status, dialogue, latency, mission events, and confirmation controls.

Karen is designed for conversations with Tino: discussing systems, debating architecture, remembering useful context, and carrying out approved desktop actions.

## Features

- Local Whisper speech recognition and wake-word commands.
- Streaming Groq or Ollama responses.
- Local Piper speech output or Groq Orpheus TTS.
- Desktop tools for applications, URLs, system information, volume, keyboard input, screenshots, and screen OCR.
- SQLite conversations, memories, tasks, and event logging.
- Confirmation gates for higher-risk actions.
- Push-to-talk, keyboard controls, and single-instance protection.
- JARVIS-inspired dashboard with live conversation and latency details.

## Requirements

- Linux with PipeWire or PulseAudio
- Node.js, npm, Rust, and Cargo
- ALSA playback tools, including `aplay`
- A working microphone
- Piper for local speech output
- A Groq API key, or an Ollama server for the selected LLM provider

## Setup

```bash
npm install
cp .env.example .env
```

Set the required values in `.env`. Never commit `.env` or API keys.

For local Piper speech, install Piper and place these files here:

```text
~/.local/share/piper-voices/en/en_US/lessac/medium/
├── en_US-lessac-medium.onnx
└── en_US-lessac-medium.onnx.json
```

`PIPER_MODEL_PATH` can override this location.

## Run

```bash
npm run tauri dev
```

For separate frontend and backend processes:

```bash
npm run dev -- --host 127.0.0.1
cargo run --manifest-path src-tauri/Cargo.toml
```

Only one Karen instance should run at a time.

## Voice configuration

Karen defaults to local Piper speech so cloud TTS quotas do not interrupt conversations:

```dotenv
KAREN_TTS_PROVIDER=piper
KAREN_VOICE_PROFILE=fast
```

To explicitly use the Groq Hannah voice:

```dotenv
KAREN_TTS_PROVIDER=groq
GROQ_TTS_VOICE=hannah
```

Hannah is a Groq Orpheus cloud voice, not a Piper voice model. The local voice installed for this project is Piper `en_US-lessac-medium`.

## Microphone setup

Karen uses the system default input source. For a 3.5 mm headset, select the headset microphone port in the system sound settings.

```bash
pactl get-default-source
pactl list sources
pactl list cards
```

Karen calibrates the microphone when capture starts, rejects short noise spikes, and stops after silence. A headset microphone close to the mouth usually performs better than a laptop microphone. Music or video through headphones normally does not enter the headset microphone; pausing playback while speaking gives the best recognition results.

## Controls

- **Core click:** toggle listening or mute Karen.
- **Ctrl+Space:** toggle listening.
- **Ctrl+Alt+M:** push to talk.
- **Cancel phrases:** `stop`, `cancel`, `be quiet`, and `never mind`.
- **Wake word:** `Karen`, `Hey Karen`, and common Whisper variants such as `Karin`.

## Providers

The LLM can use Groq for low-latency cloud reasoning or Ollama for a local/network model. Ollama can point to another machine or VM through its OpenAI-compatible endpoint.

The TTS manager supports Groq and Piper. Groq rate-limit errors fall back immediately to Piper. Piper is recommended for a consistent local voice.

## Testing

```bash
cargo test --manifest-path src-tauri/Cargo.toml
npm run build
```

## Documentation

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the component map, voice pipeline, memory model, tool permissions, event flow, configuration reference, and known limitations.
