# Karen — Autonomous Personal Assistant

Karen is a local AI desktop assistant built with a Rust/Tauri backend and a React/Vite frontend. Unlike stateless chatbots, Karen is designed to operate as a continuous background daemon with multi-tier memory, deterministic tool execution, and local-first speech pipelines.

## Features
- **Local Speech-to-Text (STT):** Uses local Whisper inference (`tiny.en` by default) to convert microphone input into text.
- **Asynchronous LLM Reasoning:** Connects to Groq for high-speed reasoning and tool orchestration via Server-Sent Events (SSE).
- **Hybrid Text-to-Speech (TTS):** Uses Groq's high-quality cloud voices as the primary provider, with a fallback to a local Piper model if rate limits or network issues occur.
- **Persistent Memory:** Uses an embedded SQLite database for short-term conversational episodic memory.

## Prerequisites

- **Node.js** and **npm**
- **Rust** and Cargo
- **System Dependencies:** ALSA / `aplay` for Linux audio playback.
- **Piper TTS (Optional but recommended):** 
  To use the local fallback voice, download the Piper `linux_x86_64` binary and the `en_US-lessac-medium.onnx` model, and place them in `~/.local/share/piper-voices/`.

## Quick Start

1. Clone the repository and navigate to `karen-desktop/`.
2. Copy `.env.example` to `.env` and fill in your `GROQ_API_KEY`:
   ```bash
   cp .env.example .env
   ```
3. Install frontend dependencies:
   ```bash
   npm install
   ```
4. Start the Tauri development server:
   ```bash
   npm run tauri dev
   ```

## Architecture

See [ARCHITECTURE.md](./ARCHITECTURE.md) for a detailed overview of how the STT, LLM, and TTS pipelines interact asynchronously.

## Configuration

See `.env.example` for the full list of configuration variables, including how to override model selection and paths.
