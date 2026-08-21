use std::collections::VecDeque;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};

const PEAK_BINS: usize = 48;
const PEAK_WINDOW: usize = 480;

#[derive(Clone, Debug)]
pub struct AudioClip {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

impl AudioClip {
    pub fn duration_secs(&self) -> f32 {
        if self.sample_rate == 0 {
            return 0.0;
        }
        self.samples.len() as f32 / self.sample_rate as f32
    }
}

struct CaptureState {
    capturing: bool,
    samples: Vec<f32>,
    sample_rate: u32,
    peaks: VecDeque<f32>,
    peak_accum: Vec<f32>,
}

enum Command {
    Start { reply: Sender<Result<()>> },
    Stop { reply: Sender<Result<AudioClip>> },
}

#[derive(Clone)]
pub struct Recorder {
    state: Arc<Mutex<CaptureState>>,
    commands: Sender<Command>,
}

impl Recorder {
    pub fn new() -> Self {
        let state = Arc::new(Mutex::new(CaptureState {
            capturing: false,
            samples: Vec::new(),
            sample_rate: 0,
            peaks: VecDeque::from(vec![0.0; PEAK_BINS]),
            peak_accum: Vec::new(),
        }));
        let (commands, rx) = mpsc::channel();
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("noma-audio".into())
            .spawn(move || audio_thread(thread_state, rx))
            .expect("spawn audio thread");
        Self { state, commands }
    }

    pub fn start(&self) -> Result<()> {
        let (reply, rx) = mpsc::channel();
        self.commands
            .send(Command::Start { reply })
            .context("audio thread is gone")?;
        rx.recv().context("audio thread did not start capture")?
    }

    pub fn stop(&self) -> Result<AudioClip> {
        let (reply, rx) = mpsc::channel();
        self.commands
            .send(Command::Stop { reply })
            .context("audio thread is gone")?;
        rx.recv().context("audio thread did not stop capture")?
    }

    pub fn peaks(&self) -> Vec<f32> {
        let state = self.state.lock().expect("capture mutex");
        state.peaks.iter().copied().collect()
    }
}

impl Default for Recorder {
    fn default() -> Self {
        Self::new()
    }
}

fn audio_thread(state: Arc<Mutex<CaptureState>>, rx: mpsc::Receiver<Command>) {
    let mut stream = None;
    while let Ok(command) = rx.recv() {
        match command {
            Command::Start { reply } => {
                let result = start_capture(&state, &mut stream);
                let _ = reply.send(result);
            }
            Command::Stop { reply } => {
                stream = None;
                let result = {
                    let mut state = state.lock().expect("capture mutex");
                    state.capturing = false;
                    Ok(AudioClip {
                        samples: std::mem::take(&mut state.samples),
                        sample_rate: state.sample_rate,
                    })
                };
                let _ = reply.send(result);
            }
        }
    }
}

fn start_capture(
    state: &Arc<Mutex<CaptureState>>,
    stream_slot: &mut Option<cpal::Stream>,
) -> Result<()> {
    {
        let current = state.lock().expect("capture mutex");
        if current.capturing && stream_slot.is_some() {
            return Ok(());
        }
    }

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or_else(|| anyhow!("no default input device"))?;
    let supported = device
        .default_input_config()
        .context("default input config")?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.clone().into();
    let sample_rate = config.sample_rate.0;
    let channels = config.channels as usize;

    {
        let mut state = state.lock().expect("capture mutex");
        state.samples.clear();
        state.peak_accum.clear();
        state.peaks = VecDeque::from(vec![0.0; PEAK_BINS]);
        state.sample_rate = sample_rate;
        state.capturing = true;
    }

    let shared = Arc::clone(state);
    let err_fn = |err| eprintln!("noma-audio stream error: {err}");
    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, channels, shared, err_fn)?,
        SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, shared, err_fn)?,
        SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, shared, err_fn)?,
        SampleFormat::U8 => build_stream::<u8>(&device, &config, channels, shared, err_fn)?,
        SampleFormat::I32 => build_stream::<i32>(&device, &config, channels, shared, err_fn)?,
        SampleFormat::F64 => build_stream::<f64>(&device, &config, channels, shared, err_fn)?,
        other => return Err(anyhow!("unsupported sample format: {other}")),
    };
    stream.play().context("start input stream")?;
    *stream_slot = Some(stream);
    Ok(())
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    shared: Arc<Mutex<CaptureState>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let mut state = shared.lock().expect("capture mutex");
                if !state.capturing {
                    return;
                }
                for frame in data.chunks(channels.max(1)) {
                    let mono = if frame.is_empty() {
                        0.0
                    } else {
                        frame.iter().map(|s| s.to_sample::<f32>()).sum::<f32>() / frame.len() as f32
                    };
                    state.samples.push(mono);
                    state.peak_accum.push(mono);
                    if state.peak_accum.len() >= PEAK_WINDOW {
                        let rms = rms(&state.peak_accum);
                        state.peak_accum.clear();
                        if state.peaks.len() == PEAK_BINS {
                            state.peaks.pop_front();
                        }
                        state.peaks.push_back(rms);
                    }
                }
            },
            err_fn,
            None,
        )
        .context("build input stream")
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let mean = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    mean.sqrt().clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::AudioClip;

    #[test]
    fn duration_from_samples() {
        let clip = AudioClip {
            samples: vec![0.0; 8_000],
            sample_rate: 16_000,
        };
        assert!((clip.duration_secs() - 0.5).abs() < f32::EPSILON);
    }
}
