# ADR 0001: TTS Provider Abstraction and Atomic Generation Cancellation

## Status
Accepted

## Context
Karen uses Groq's API for fast, high-quality Text-to-Speech (TTS). However, Groq's daily token limits frequently cause `429 Too Many Requests` errors. When this happened previously, the audio pipeline failed silently, and the text UI was simultaneously wiped because the backend unconditionally fired an `idle` event as soon as the LLM finished generating.

Additionally, user interruptions (cancellations) were only killing the active `aplay` process. The LLM Server-Sent Events (SSE) stream and overarching pipeline were unaware of the interruption, causing stale audio chunks to queue and play, and stale text to overwrite the frontend.

## Decision
1. **TTS Provider Contract:** We introduced a `TtsProvider` trait and a `TtsManager`. `GroqProvider` is the primary, and `PiperProvider` (local ONNX model) is the fallback. If Groq returns a retryable error or hits a limit, the manager automatically routes the sentence to Piper.
2. **Atomic Generation Tracking:** We introduced a global `AtomicUsize` named `current_generation`. Every time the user initiates a turn or cancels, this counter increments. The `run_agentic` LLM SSE loop captures its starting `gen_id` and checks the global counter on every chunk. If there's a mismatch, it immediately aborts.
3. **Decoupled UI State:** The backend no longer unconditionally emits an `idle` event upon LLM completion. The text response persists on the UI independently of the asynchronous TTS playback success.

## Alternatives considered
- **Replacing Groq TTS entirely with local Piper:** Rejected. Piper is fast and local but lacks the natural prosody of Groq's cloud models. Groq remains primary for quality.
- **Using Tokio channels for cancellation:** Rejected. Thread messaging requires complex select loops around the SSE stream parser and Tauri IPC handlers. An atomic counter is lock-free, zero-overhead, and easily accessible from any deeply nested async function.

## Consequences
- **Easier:** Adding new TTS providers (e.g., ElevenLabs) is now trivial due to the trait contract.
- **Harder:** The system relies on an implicit contract that every long-running async loop must manually check the `gen_id`. If a developer forgets to add this check in a future long-running feature, the cancellation bug will return.
- **Benefit:** The UI text is now perfectly stable, and audio failures gracefully degrade to a local voice without dropping the conversational thread.
