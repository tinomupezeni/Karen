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

// A capture longer than this stops accumulating (but keeps recording silently
// discarded past the cap) so a forgotten click-to-stop can't grow memory
// unbounded. Generous enough that no real voice command should ever hit it.
const MAX_RECORDING_SECS: u32 = 120;
const CAPTURE_SETTLE_MS: u64 = 450;
const NOISE_CALIBRATION_MS: u64 = 650;
const DEFAULT_SPEECH_END_SILENCE_MS: u64 = 350;

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
        guard.noise_floor = 0.005; // start with a small sane default
        guard.frames_since_log = 0;
        guard.capture_started_at = Some(std::time::Instant::now());
        guard.speech_candidate_frames = 0;
    }

    let err_fn = |err| log::error!("mic stream error: {err}");

    let process = move |data: &[f32]| {
                let sum_sq: f32 = data.iter().map(|s| s * s).sum();
                let rms = (sum_sq / data.len().max(1) as f32).sqrt();
                
                let mut guard = buffer.0.lock().unwrap();
                let now = std::time::Instant::now();
                
                let calibrating = guard.capture_started_at
                    .map(|started| now.duration_since(started) < std::time::Duration::from_millis(NOISE_CALIBRATION_MS))
                    .unwrap_or(false);

                // Learn the room/microphone baseline before endpoint detection. Once
                // speech starts, freeze the baseline so loud speech cannot move the
                // threshold upward and make the utterance impossible to finish.
                if calibrating {
                    guard.noise_floor = (guard.noise_floor * 0.95) + (rms * 0.05);
                } else if !guard.is_speaking {
                    guard.noise_floor = (guard.noise_floor * 0.995) + (rms * 0.005);
                }
                
                // Print a debug log every ~1 second so we can see the volume levels
                // Assuming ~43 frames per second for typical cpal buffer sizes
                guard.frames_since_log += 1;
                if guard.frames_since_log >= 43 {
                    log::debug!("MIC MONITOR -> rms: {:.5}, floor: {:.5}", rms, guard.noise_floor);
                    guard.frames_since_log = 0;
                }

                // Do not trigger during baseline calibration. Afterward, speech must
                // rise clearly above the measured noise, with a floor for near-silence.
                let threshold = (guard.noise_floor * 2.0).max(0.08);
                
                if !calibrating && rms > threshold {
                    guard.speech_candidate_frames += 1;
                    // Require a short, continuous rise above the noise floor. This
                    // rejects isolated clicks and headset/mic noise spikes.
                    if guard.speech_candidate_frames >= 4 {
                        guard.last_speech_time = Some(now);
                        if !guard.is_speaking {
                            guard.is_speaking = true;
                            log::info!("Speech started (rms={rms:.5}, floor={:.5}, threshold={threshold:.5})", guard.noise_floor);
                        }
                    }
                } else if !guard.is_speaking {
                    guard.speech_candidate_frames = 0;
                }

                if guard.samples.len() < cap_len {
                    guard.samples.extend_from_slice(data);
                }

                if guard.is_speaking {
                    if let Some(last) = guard.last_speech_time {
                        if now.duration_since(last) > end_silence {
                            guard.is_speaking = false;
                            guard.last_speech_time = None;
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
                let samples = normalize_f32_samples(data);
                process(&samples);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.into(),
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                let samples = normalize_i16_samples(data);
                process(&samples);
            },
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            config.into(),
            move |data: &[u16], _: &cpal::InputCallbackInfo| {
                let samples = normalize_u16_samples(data);
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
    use super::{capture_settle_delay, normalize_f32_samples, normalize_i16_samples, normalize_u16_samples, speech_end_silence};

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
    fn speech_end_silence_is_fast_but_safe() {
        assert_eq!(speech_end_silence(), std::time::Duration::from_millis(350));
    }
}
