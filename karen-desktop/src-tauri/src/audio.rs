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

// A capture longer than this stops accumulating (but keeps recording silently
// discarded past the cap) so a forgotten click-to-stop can't grow memory
// unbounded. Generous enough that no real voice command should ever hit it.
const MAX_RECORDING_SECS: u32 = 120;

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

    {
        let mut guard = buffer.0.lock().unwrap();
        guard.samples.clear();
        guard.format = Some(format);
        guard.is_speaking = false;
        guard.last_speech_time = None;
        guard.noise_floor = 0.005; // start with a small sane default
        guard.frames_since_log = 0;
    }

    let err_fn = |err| log::error!("mic stream error: {err}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let sum_sq: f32 = data.iter().map(|s| s * s).sum();
                let rms = (sum_sq / data.len().max(1) as f32).sqrt();
                
                let mut guard = buffer.0.lock().unwrap();
                let now = std::time::Instant::now();
                
                // Use a very slow Exponential Moving Average for the noise floor
                // 99.9% old floor, 0.1% new rms
                guard.noise_floor = (guard.noise_floor * 0.999) + (rms * 0.001);
                
                // Print a debug log every ~1 second so we can see the volume levels
                // Assuming ~43 frames per second for typical cpal buffer sizes
                guard.frames_since_log += 1;
                if guard.frames_since_log >= 43 {
                    log::debug!("MIC MONITOR -> rms: {:.5}, floor: {:.5}", rms, guard.noise_floor);
                    guard.frames_since_log = 0;
                }

                // Speech is considered when RMS is significantly above the noise floor
                // Using a multiplier (2x) instead of an absolute addition so quiet mics work
                // But add a tiny baseline so pure silence (0.0000) doesn't false-trigger
                let threshold = (guard.noise_floor * 2.5).max(0.001);
                
                if rms > threshold {
                    guard.last_speech_time = Some(now);
                    if !guard.is_speaking {
                        guard.is_speaking = true;
                        log::info!("Speech started (rms={rms:.5}, floor={:.5})", guard.noise_floor);
                    }
                }

                if guard.samples.len() < cap_len {
                    guard.samples.extend_from_slice(data);
                }

                if guard.is_speaking {
                    if let Some(last) = guard.last_speech_time {
                        if now.duration_since(last) > std::time::Duration::from_millis(500) {
                            guard.is_speaking = false;
                            guard.last_speech_time = None;
                            log::info!("Speech ended (0.5s silence). Triggering pipeline.");
                            let _ = on_speech_end.send(());
                        }
                    }
                }
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
