use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

#[derive(Clone, Copy, Debug)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Default)]
struct SharedBuffer {
    samples: Vec<f32>,
    format: Option<AudioFormat>,
    is_speaking: bool,
    last_speech_time: Option<std::time::Instant>,
    noise_floor: f32,
    frames_since_log: usize,
    capture_started_at: Option<std::time::Instant>,
    speech_candidate_frames: usize,
    speech_started_at: Option<std::time::Instant>,
    // Rolling window of the most recent idle audio, so the moment speech
    // actually starts isn't clipped. Only this small window is kept while
    // not speaking; `samples` itself is only filled during real speech.
    preroll: Vec<f32>,
    // Short-horizon smoothed RMS (see SMOOTHING_ALPHA) used for every VAD
    // decision. A noisy mic/room produces isolated single-callback spikes
    // well above any reasonable threshold; comparing those raw spikes
    // against threshold let one noise tick re-arm `last_speech_time`
    // forever, so speech never looked like it ended. Smoothing requires
    // the energy to actually persist across a few callbacks before it
    // counts as speech, while still reacting fast enough to catch a real
    // word's onset.
    smoothed_rms: f32,
    // State for the stateful DC/hum high-pass filter (see `highpass_hum`).
    hp_prev_x: f32,
    hp_prev_y: f32,
}

#[derive(Clone)]
pub struct AudioBuffer(Arc<Mutex<SharedBuffer>>);

impl AudioBuffer {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(SharedBuffer::default())))
    }

    /// Drains the buffered recording, returning it only if a recording was
    /// actually in progress (i.e. `Start` ran at least once since the last drain).
    pub fn drain(&self) -> Option<(Vec<f32>, AudioFormat)> {
        let mut guard = self.0.lock().unwrap();
        guard.is_speaking = false;
        guard.last_speech_time = None;
        guard.speech_started_at = None;
        guard.preroll.clear();
        guard
            .format
            .take()
            .map(|format| (std::mem::take(&mut guard.samples), format))
    }
}

pub enum AudioCommand {
    Start,
    Stop,
}

fn normalize_i16_samples(data: &[i16]) -> Vec<f32> {
    data.iter().map(|sample| *sample as f32 / i16::MAX as f32).collect()
}

fn normalize_f32_samples(data: &[f32]) -> Vec<f32> {
    data.iter().map(|sample| sample.clamp(-1.0, 1.0)).collect()
}

fn normalize_u16_samples(data: &[u16]) -> Vec<f32> {
    data.iter()
        .map(|sample| (*sample as f32 - 32768.0) / 32768.0)
        .collect()
}

/// Strip the DC bias (constant offset) from one callback buffer. Raw HDA
/// capture can carry several percent of DC, which inflates RMS, poisons the
/// noise-floor estimate, and degrades Whisper. Filtered capture stacks
/// (PipeWire clients) never see it, so remove it here on the direct path.
fn remove_dc_offset(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }
    let mean = samples.iter().sum::<f32>() / samples.len() as f32;
    for sample in samples.iter_mut() {
        *sample -= mean;
    }
}

// A capture longer than this stops accumulating (but keeps recording silently
// discarded past the cap) so a forgotten click-to-stop can't grow memory
// unbounded. Generous enough that no real voice command should ever hit it.
const MAX_RECORDING_SECS: u32 = 120;
const CAPTURE_SETTLE_MS: u64 = 450;
const NOISE_CALIBRATION_MS: u64 = 650;
const DEFAULT_SPEECH_END_SILENCE_MS: u64 = 350;
// No real utterance runs this long; beyond it the detector is latched on
// noise and must reset rather than accumulate a fake turn.
const MAX_SPEECH_SECS: std::time::Duration = std::time::Duration::from_secs(20);
// Ceiling for the learned room baseline. A genuine desktop room sits far
// below this; anything higher is a poisoned estimate (own-voice headphone
// bleed, media playing during calibration) that would put speech
// unreachable above threshold and make Friday seem deaf.
const MAX_NOISE_FLOOR: f32 = 0.15;
// Calibration seeds high so the window can track DOWN to the quietest
// moment (min-tracking). See the process closure below.
const FLOOR_CALIBRATION_SEED: f32 = 0.5;
// Weight given to each new callback's RMS when updating `smoothed_rms`.
// Low enough that a single noisy callback can't flip the VAD decision on
// its own; high enough that real speech (sustained across many callbacks)
// is reflected within ~2-3 callbacks, not noticeably delayed.
const SMOOTHING_ALPHA: f32 = 0.35;
// How much idle audio to keep "pre-rolled" so the onset of speech is never
// clipped when the detector only just crossed threshold. Idle silence
// beyond this is dropped outright rather than being recorded.
const PREROLL_MS: u64 = 300;

fn capture_settle_delay() -> std::time::Duration {
    std::time::Duration::from_millis(CAPTURE_SETTLE_MS)
}

fn speech_end_silence() -> std::time::Duration {
    let millis = std::env::var("KAREN_SPEECH_END_SILENCE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (200..=1200).contains(value))
        .unwrap_or(DEFAULT_SPEECH_END_SILENCE_MS);
    std::time::Duration::from_millis(millis)
}

fn build_input_stream(
    buffer: AudioBuffer,
    on_speech_end: std::sync::mpsc::Sender<()>,
) -> Result<cpal::Stream, Box<dyn std::error::Error>> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no input device available")?;
    let config = device.default_input_config()?;
    log::info!(
        "mic device: host={:?} device={device:?} rate={}Hz channels={} format={:?}",
        host.id(),
        config.sample_rate(),
        config.channels(),
        config.sample_format()
    );

    let format = AudioFormat {
        sample_rate: config.sample_rate(),
        channels: config.channels(),
    };
    let cap_len = (MAX_RECORDING_SECS as usize) * (format.sample_rate as usize) * (format.channels as usize);
    let end_silence = speech_end_silence();

    {
        let mut guard = buffer.0.lock().unwrap();
        guard.samples.clear();
        guard.format = Some(format);
        guard.is_speaking = false;
        guard.last_speech_time = None;
        guard.noise_floor = FLOOR_CALIBRATION_SEED; // start high; calibration tracks down to true room tone
        guard.frames_since_log = 0;
        guard.capture_started_at = Some(std::time::Instant::now());
        guard.speech_candidate_frames = 0;
        guard.speech_started_at = None;
        guard.preroll.clear();
        guard.smoothed_rms = 0.0;
    }

    let preroll_cap = (PREROLL_MS as usize) * (format.sample_rate as usize) / 1000 * (format.channels as usize);
    let err_fn = |err| log::error!("mic stream error: {err}");

    let process = move |data: &[f32]| {
                let sum_sq: f32 = data.iter().map(|s| s * s).sum();
                let rms = (sum_sq / data.len().max(1) as f32).sqrt();
                let dc: f32 = data.iter().sum::<f32>() / data.len().max(1) as f32;
                
                let mut guard = buffer.0.lock().unwrap();
                let now = std::time::Instant::now();

                // Smooth before anything else looks at this callback's level.
                // A single noisy/clicky callback must not, by itself, look
                // like speech to either calibration or endpoint detection.
                guard.smoothed_rms = (guard.smoothed_rms * (1.0 - SMOOTHING_ALPHA)) + (rms * SMOOTHING_ALPHA);
                let level = guard.smoothed_rms;

                let calibrating = guard.capture_started_at
                    .map(|started| now.duration_since(started) < std::time::Duration::from_millis(NOISE_CALIBRATION_MS))
                    .unwrap_or(false);

                // Learn the room/microphone baseline before endpoint detection. Once
                // speech starts, freeze the baseline so loud speech cannot move the
                // threshold upward and make the utterance impossible to finish.
                // Calibration tracks the QUIETEST moment (min), not an average:
                // transients during the window (Friday's own voice bleeding
                // through headphones, door slams, media) must not poison the
                // baseline upward and deafen the detector. The ceiling caps any
                // estimate no real desktop room should exceed.
                if calibrating {
                    guard.noise_floor = guard.noise_floor.min(level);
                } else if !guard.is_speaking {
                    guard.noise_floor = (guard.noise_floor * 0.995) + (level * 0.005);
                }
                guard.noise_floor = guard.noise_floor.min(MAX_NOISE_FLOOR);

                // Print a debug log every ~1 second so we can see the volume levels
                // Assuming ~43 frames per second for typical cpal buffer sizes
                guard.frames_since_log += 1;
                if guard.frames_since_log >= 43 {
                    log::debug!("MIC MONITOR -> rms: {:.5}, smoothed: {:.5}, dc: {:.5}, floor: {:.5}", rms, level, dc, guard.noise_floor);
                    guard.frames_since_log = 0;
                }

                // Do not trigger during baseline calibration. Afterward, speech must
                // rise clearly above the measured noise. The first working version
                // of this detector (see git history) used `(floor * 2.5).max(0.001)`
                // -- an almost negligible absolute floor that let quiet headset
                // speech through easily. A later session raised the absolute floor
                // to 0.04 to fight a noisy room, but that also made genuinely quiet
                // speech (RMS ~0.04-0.06 against a ~0.02-0.03 floor) sit right at
                // the edge of the threshold instead of clearing it with margin.
                // 0.01 keeps enough of a floor to reject near-total silence/clicks
                // without choking normal quiet speech the way 0.04 did.
                let threshold = (guard.noise_floor * 2.0).max(0.01);

                if !calibrating && level > threshold {
                    guard.speech_candidate_frames += 1;
                    // Require a short, continuous rise above the noise floor. This
                    // rejects isolated clicks and headset/mic noise spikes.
                    if guard.speech_candidate_frames >= 4 {
                        guard.last_speech_time = Some(now);
                        if !guard.is_speaking {
                            guard.is_speaking = true;
                            guard.speech_started_at = Some(now);
                            // Seed the real recording with whatever idle audio was
                            // just pre-rolled so the first syllable isn't clipped.
                            let mut preroll = std::mem::take(&mut guard.preroll);
                            guard.samples.append(&mut preroll);
                            log::info!("Speech started (rms={level:.5}, floor={:.5}, threshold={threshold:.5})", guard.noise_floor);
                        }
                    }
                } else if !guard.is_speaking {
                    guard.speech_candidate_frames = 0;
                }

                // Only record while actual speech (including its trailing
                // end-silence grace window) is in progress. Idle silence before
                // that is never written into `samples` -- it only feeds the
                // short pre-roll below -- so a long quiet wait can never fill
                // the recording cap or bloat what gets sent to Whisper.
                if guard.is_speaking {
                    if guard.samples.len() < cap_len {
                        guard.samples.extend_from_slice(data);
                    }
                } else {
                    guard.preroll.extend_from_slice(data);
                    if guard.preroll.len() > preroll_cap {
                        let excess = guard.preroll.len() - preroll_cap;
                        guard.preroll.drain(0..excess);
                    }
                }

                if guard.is_speaking {
                    // Unstick guarantee: room noise that keeps refreshing the
                    // speech timer must never latch the detector forever. The
                    // first working version of this detector had no such cap
                    // at all -- it just kept recording until real silence
                    // showed up, however long that took, and always handed
                    // whatever it got to STT. A later safety valve capped the
                    // wait but *silently discarded* the buffer instead of
                    // still sending it on, which meant a real utterance that
                    // got stretched by noise vanished with zero feedback --
                    // indistinguishable from Friday being completely deaf.
                    // Flushing to the pipeline here instead keeps the safety
                    // cap (never record unboundedly) while restoring the
                    // original guarantee that a capture always gets a result.
                    let stuck = guard
                        .speech_started_at
                        .map(|started| now.duration_since(started) > MAX_SPEECH_SECS)
                        .unwrap_or(false);
                    if stuck {
                        log::warn!(
                            "VAD stuck in speech for >{}s (floor={:.5}); flushing to pipeline instead of discarding",
                            MAX_SPEECH_SECS.as_secs(),
                            guard.noise_floor
                        );
                        guard.is_speaking = false;
                        guard.last_speech_time = None;
                        guard.speech_started_at = None;
                        guard.speech_candidate_frames = 0;
                        guard.preroll.clear();
                        guard.smoothed_rms = 0.0;
                        guard.noise_floor = FLOOR_CALIBRATION_SEED;
                        let _ = on_speech_end.send(());
                        guard.capture_started_at = Some(now);
                    } else if let Some(last) = guard.last_speech_time {
                        if now.duration_since(last) > end_silence {
                            guard.is_speaking = false;
                            guard.last_speech_time = None;
                            guard.speech_started_at = None;
                            log::info!("Speech ended ({:?} silence). Triggering pipeline.", end_silence);
                            let _ = on_speech_end.send(());
                        }
                    }
                }
    };

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut samples = normalize_f32_samples(data);
                remove_dc_offset(&mut samples);
                process(&samples);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let mut samples = normalize_i16_samples(data);
                remove_dc_offset(&mut samples);
                process(&samples);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            config.into(),
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let mut samples = normalize_u16_samples(data);
                remove_dc_offset(&mut samples);
                process(&samples);
            },
            err_fn,
            None,
        )?,
        sample_format => {
            return Err(format!("unsupported sample format: {sample_format:?}").into())
        }
    };

    Ok(stream)
}

// cpal::Stream is not Send on every platform, so it must never leave the
// thread it was created on. Owning it inside a dedicated worker thread and
// talking to it over a channel keeps the capture entirely on one thread.
pub fn spawn_audio_worker(
    buffer: AudioBuffer,
    on_speech_end: std::sync::mpsc::Sender<()>,
) -> Sender<AudioCommand> {
    let (tx, rx) = mpsc::channel::<AudioCommand>();

    std::thread::spawn(move || {
        let mut stream: Option<cpal::Stream> = None;

        for cmd in rx {
            match cmd {
                AudioCommand::Start => {
                    if stream.is_some() {
                        continue;
                    }
                    // Give the speakers and microphone a brief settling period
                    // after Karen finishes speaking, avoiding capture of the
                    // last playback tail as the next user turn.
                    std::thread::sleep(capture_settle_delay());
                    match build_input_stream(buffer.clone(), on_speech_end.clone()) {
                        Ok(s) => match s.play() {
                            Ok(()) => {
                                log::info!("mic capture started");
                                stream = Some(s);
                            }
                            Err(e) => log::error!("failed to start mic capture: {e}"),
                        },
                        Err(e) => log::error!("failed to open input device: {e}"),
                    }
                }
                AudioCommand::Stop => {
                    if stream.take().is_some() {
                        log::info!("mic capture stopped");
                    }
                }
            }
        }
    });

    tx
}

#[cfg(test)]
mod tests {
    use super::{capture_settle_delay, normalize_f32_samples, normalize_i16_samples, normalize_u16_samples, remove_dc_offset, speech_end_silence};

    #[test]
    fn clamps_native_float_samples() {
        assert_eq!(normalize_f32_samples(&[-2.0, -0.25, 0.5, 2.0]), vec![-1.0, -0.25, 0.5, 1.0]);
    }

    #[test]
    fn converts_signed_pcm_to_float() {
        let samples = normalize_i16_samples(&[i16::MIN, 0, i16::MAX]);
        assert!((samples[0] + 1.0).abs() < 0.0001);
        assert_eq!(samples[1], 0.0);
        assert!((samples[2] - 1.0).abs() < 0.0001);
    }

    #[test]
    fn converts_unsigned_pcm_to_centered_float() {
        let samples = normalize_u16_samples(&[0, 32768, u16::MAX]);
        assert!((samples[0] + 1.0).abs() < 0.0001);
        assert_eq!(samples[1], 0.0);
        assert!((samples[2] - 0.9999695).abs() < 0.0001);
    }

    #[test]
    fn capture_settle_delay_is_short_but_nonzero() {
        assert_eq!(capture_settle_delay(), std::time::Duration::from_millis(450));
    }

    #[test]
    fn removes_constant_dc_bias() {
        let mut samples = vec![0.3, 0.3, 0.3, 0.3];
        remove_dc_offset(&mut samples);
        assert!(samples.iter().all(|s| s.abs() < 0.0001));
    }

    #[test]
    fn preserves_ac_shape_when_removing_dc() {
        let mut samples = vec![1.1, 0.9, 1.0];
        remove_dc_offset(&mut samples);
        assert!((samples[0] - 0.1).abs() < 0.0001);
        assert!((samples[1] + 0.1).abs() < 0.0001);
        assert!(samples[2].abs() < 0.0001);
    }

    #[test]
    fn speech_end_silence_is_fast_but_safe() {
        assert_eq!(speech_end_silence(), std::time::Duration::from_millis(350));
    }
}
