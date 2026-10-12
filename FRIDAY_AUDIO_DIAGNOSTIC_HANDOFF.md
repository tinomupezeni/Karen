# Friday audio issue: diagnostic handoff

## Current situation

Friday has had two reported audio problems during this session:

1. Voice input was not being recognized.
2. Friday showed “speaking” but produced no audible output.

The input problem was traced to an overly high voice activity detection (VAD) threshold and the threshold was lowered. The output problem is still unresolved. We added more detailed playback logging, but the latest file log does not show a response reaching TTS, so it does not yet contain the new playback diagnostics.

At the last check, no `target/debug/app`, `tauri dev`, or Vite process was running. The newest observed log activity was around 07:03:45 in the timestamp format in Friday’s log (the file mtime showed 09:03:45 in the local system timezone).

## Work completed

### VAD adjustment

File: `karen-desktop/src-tauri/src/audio.rs`

The threshold changed from:

```rust
(noise_floor * 2.0).max(0.08)
```

to:

```rust
(noise_floor * 1.35).max(0.04)
```

Reason: the app’s debug readings showed a noise floor near `0.067`, so the old threshold was about `0.134`. The observed RMS often remained below that level. Lowering the multiplier allowed VAD to report `Speech started` and `Speech ended` for captured utterances.

### Friday wake phrase

The “no wake phrase” status in `karen-desktop/src-tauri/src/pipeline.rs` now says “Say Friday followed by a command.” The configured default wake word is Friday, with aliases added in the earlier rename work.

### Playback backend and diagnostics

File: `karen-desktop/src-tauri/src/tts/mod.rs`

Playback now prefers `pw-play` when available, supports an explicit `KAREN_AUDIO_PLAYER` override, and falls back to `aplay` if `pw-play` is not found. This change was made after `pactl` could not connect to PulseAudio from the diagnostic shell, while `pw-play` was installed.

Additional playback logs were added to report:

- Selected player and WAV file details (size, channels, sample rate, bit depth, duration)
- Whether relevant audio environment variables are set
- Player spawn failures and whether the executable is found on `PATH`
- Player exit status and captured stderr

`cargo check --manifest-path karen-desktop/src-tauri/Cargo.toml` passed after these logging changes.

## Evidence observed

### Input capture

Earlier live Tauri terminal output showed non-zero microphone RMS values and successful VAD events. Examples included:

```text
Speech started (rms=0.12129, floor=0.06682, threshold=0.09020)
Speech ended (350ms silence). Triggering pipeline.
Audio captured: 4.54s (including VAD silence)
```

This established that CPAL capture was receiving samples and that the revised VAD could segment an utterance.

In later live terminal output, Whisper decoded some captures as background/noise markers such as `(water running)` and `(wind howling)`. This suggests some captures may contain room/device noise or unintelligible speech. Those samples do not establish that Friday transcribed the user’s intended command correctly.

### TTS and playback before the diagnostic logging change

Earlier live terminal output showed:

```text
Groq synthesis duration: ...
Playback started
Playing WAVE '/tmp/karen-tts-....wav' ...
Playback ended (...)
```

Another live run showed Piper synthesis followed by `pw-play exited successfully`. The user still reported no audible output. A successful process exit only shows that the player process returned success; it does not prove audio reached the intended physical output.

The `pactl` checks from the agent shell failed with “Connection refused” / “Operation not permitted.” These checks ran in the tool environment and may not share Friday’s audio-server environment, so treat them as a clue, not proof that the app itself cannot access PipeWire.

### Latest persisted log

Friday’s current log is:

```text
/home/shadowe/.local/share/com.tauri.dev/logs/friday.log
```

The previous assistant initially inspected `karen.log`, which was stale. The latest `friday.log` showed repeated VAD capture/endpoint events and entries like:

```text
Speech ended (350ms silence). Triggering pipeline.
Audio captured: ... (including VAD silence)
mic capture stopped
```

But after those entries there were no `heard:`, STT error, LLM, synthesis, or playback entries in the inspected tail. Thus those logged turns did not provide evidence of a TTS output attempt. The new `Playback starting: ...` diagnostics were not present in the inspected Friday log.

## Current diagnosis

- Microphone samples are reaching the app.
- The revised VAD has detected utterance boundaries.
- Some captured segments were transcribed as noise markers in earlier live output.
- TTS synthesis and `pw-play` previously ran successfully, but the user heard no sound.
- The latest persisted file log does not show a complete STT-to-TTS turn, and it does not include the new playback diagnostics.
- No Friday process was running at the last process check.

Therefore there is not yet enough evidence to identify one confirmed root cause for the current no-audio report. There may be two separate problems: noisy/incorrect capture or stalled transcription on input, and a playback routing or device issue on output.

## Next steps for the receiving agent

1. Start the current build from `karen-desktop` with `npm run tauri dev`. Keep its terminal output available; previous long-running tool sessions were lost, while terminal logs sometimes contained newer data than the persisted file.
2. Confirm Friday starts and stays running. Check that `friday.log` gets a fresh modification time.
3. Ask for one short test command in a quiet environment. Record the complete sequence from `Speech started` / `Speech ended` through the Whisper result (`heard:` or an STT error), LLM response, TTS synthesis, and player diagnostics.
4. If the app captures speech but never logs a Whisper result, inspect the STT task path and failures in `pipeline.rs` / `stt.rs`. Add stage-start, completion, elapsed-time, and join-error logging if needed.
5. If a response reaches TTS, inspect the new `Playback starting` line and captured player stderr. Check whether the WAV exists and has a plausible duration/format before playback. If `pw-play` exits successfully with no audible output, verify the PipeWire sink from the same desktop user/session that launched Friday, then try an explicitly selected sink/player as a controlled diagnostic.
6. Do not treat `pw-play` exit code 0 alone as proof of audible output. Confirm with the user after a short known response.

## Useful files and commands

- Audio capture and VAD: `karen-desktop/src-tauri/src/audio.rs`
- STT orchestration and transcript handling: `karen-desktop/src-tauri/src/pipeline.rs`
- Whisper setup/transcription: `karen-desktop/src-tauri/src/stt.rs`
- TTS queue and playback process: `karen-desktop/src-tauri/src/tts/mod.rs`
- TTS provider selection/fallback: `karen-desktop/src-tauri/src/tts/manager.rs`
- Latest persisted app log: `/home/shadowe/.local/share/com.tauri.dev/logs/friday.log`
- Older, stale log: `/home/shadowe/.local/share/com.tauri.dev/logs/karen.log`
- Start dev app: `npm run tauri dev` from `karen-desktop`
- Compile check: `cargo check --manifest-path karen-desktop/src-tauri/Cargo.toml` from the repository root

`KAREN_AUDIO_PLAYER=pw-play` can explicitly select PipeWire playback. Existing `KAREN_*` environment names remain in use after the assistant’s display name changed to Friday.
