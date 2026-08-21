use std::collections::VecDeque;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};

mod resample;

pub use resample::resample;

const PEAK_BINS: usize = 48;
const PEAK_WINDOW: usize = 480;

/// Below this the clip is treated as "nothing was said" and never sent to ASR.
const SILENCE_RMS: f32 = 0.0015;

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

    /// The same audio at `rate`, band-limited on the way down.
    pub fn resampled(&self, rate: u32) -> AudioClip {
        AudioClip {
            samples: resample(&self.samples, self.sample_rate, rate),
            sample_rate: rate,
        }
    }

    /// True when the clip is too short or too quiet to be speech.
    pub fn is_silent(&self) -> bool {
        self.duration_secs() < 0.15 || rms(&self.samples) < SILENCE_RMS
    }
}

struct CaptureState {
    capturing: bool,
    samples: Vec<f32>,
    sample_rate: u32,
    peaks: VecDeque<f32>,
    peak_accum: Vec<f32>,
    /// Name of the device the last capture actually opened.
    device: String,
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
    /// Record from whatever Windows has set as the default input.
    pub fn new() -> Self {
        Self::with_device(None)
    }

    /// Record from the first input device whose name contains `preferred`,
    /// case-insensitively, falling back to the system default.
    ///
    /// Matching on a substring rather than the exact name means "yeti" is
    /// enough for "Microphone (Yeti Classic)", and it survives Windows
    /// renumbering a device to "2- Arctis Nova Pro Wireless".
    pub fn with_device(preferred: Option<String>) -> Self {
        let state = Arc::new(Mutex::new(CaptureState {
            capturing: false,
            samples: Vec::new(),
            sample_rate: 0,
            peaks: VecDeque::from(vec![0.0; PEAK_BINS]),
            peak_accum: Vec::new(),
            device: String::new(),
        }));
        let (commands, rx) = mpsc::channel();
        let thread_state = Arc::clone(&state);
        thread::Builder::new()
            .name("noma-audio".into())
            .spawn(move || audio_thread(thread_state, rx, preferred))
            .expect("spawn audio thread");
        Self { state, commands }
    }

    /// The device the last capture opened, empty before the first one.
    pub fn device_name(&self) -> String {
        self.state.lock().expect("capture mutex").device.clone()
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

    /// Everything captured so far, without interrupting the capture.
    ///
    /// This is what feeds partial results while the key is still held.
    pub fn snapshot(&self) -> AudioClip {
        let state = self.state.lock().expect("capture mutex");
        AudioClip {
            samples: state.samples.clone(),
            sample_rate: state.sample_rate,
        }
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

fn audio_thread(
    state: Arc<Mutex<CaptureState>>,
    rx: mpsc::Receiver<Command>,
    preferred: Option<String>,
) {
    let mut stream = None;
    while let Ok(command) = rx.recv() {
        match command {
            Command::Start { reply } => {
                let result = start_capture(&state, &mut stream, preferred.as_deref());
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
    preferred: Option<&str>,
) -> Result<()> {
    {
        let current = state.lock().expect("capture mutex");
        if current.capturing && stream_slot.is_some() {
            return Ok(());
        }
    }

    let host = cpal::default_host();
    // Resolved on every start, so changing the Windows default takes effect on
    // the next key press rather than needing a restart.
    let device = pick_device(&host, preferred)?;
    let device_name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
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
        state.device = device_name;
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

/// Every input device the system exposes, in the order it lists them.
pub fn input_device_names() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|devices| devices.filter_map(|device| device.name().ok()).collect())
        .unwrap_or_default()
}

/// The name of the device Noma would record from right now.
pub fn current_device_name(preferred: Option<&str>) -> Option<String> {
    let host = cpal::default_host();
    pick_device(&host, preferred)
        .ok()
        .and_then(|device| device.name().ok())
}

fn pick_device(host: &cpal::Host, preferred: Option<&str>) -> Result<cpal::Device> {
    if let Some(want) = preferred.map(str::trim).filter(|want| !want.is_empty()) {
        let devices: Vec<cpal::Device> = host
            .input_devices()
            .map(|devices| devices.collect())
            .unwrap_or_default();
        let names: Vec<String> = devices
            .iter()
            .map(|device| device.name().unwrap_or_default())
            .collect();
        if let Some(index) = match_device(&names, want) {
            return Ok(devices.into_iter().nth(index).expect("matched index"));
        }
        // Falling back is kinder than refusing to record: an unplugged mic
        // should not stop dictation, it should just be noted.
        eprintln!(
            "noma: no input device matching {want:?} (found: {}), using the system default",
            names.join(", ")
        );
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("no input device available"))
}

/// Index of the first name containing `want`, case-insensitively.
///
/// An exact match always wins, so a specific name cannot be stolen by a
/// device that merely contains it as a substring.
fn match_device(names: &[String], want: &str) -> Option<usize> {
    let want = want.trim().to_lowercase();
    if want.is_empty() {
        return None;
    }
    names
        .iter()
        .position(|name| name.to_lowercase() == want)
        .or_else(|| {
            names
                .iter()
                .position(|name| name.to_lowercase().contains(&want))
        })
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

    fn tone(samples: usize, rate: u32, amplitude: f32) -> AudioClip {
        AudioClip {
            samples: (0..samples)
                .map(|index| amplitude * (index as f32 * 0.1).sin())
                .collect(),
            sample_rate: rate,
        }
    }

    #[test]
    fn duration_from_samples() {
        let clip = AudioClip {
            samples: vec![0.0; 8_000],
            sample_rate: 16_000,
        };
        assert!((clip.duration_secs() - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn duration_survives_a_missing_sample_rate() {
        let clip = AudioClip {
            samples: vec![0.0; 8_000],
            sample_rate: 0,
        };
        assert_eq!(clip.duration_secs(), 0.0);
        assert!(clip.is_silent());
    }

    #[test]
    fn resampled_reports_the_new_rate() {
        let clip = tone(48_000, 48_000, 0.5);
        let resampled = clip.resampled(16_000);
        assert_eq!(resampled.sample_rate, 16_000);
        assert_eq!(resampled.samples.len(), 16_000);
    }

    /// The real device names from a machine that hit this bug: the default was
    /// a sleeping headset while the Yeti was the live mic.
    fn devices() -> Vec<String> {
        [
            "Microphone (Razer Ripsaw HD)",
            "Microphone (2- Arctis Nova Pro Wireless)",
            "Microphone (Yeti Classic)",
            "Microphone (Razer Ripsaw HD HDMI )",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect()
    }

    #[test]
    fn a_short_substring_is_enough_to_pick_a_mic() {
        assert_eq!(super::match_device(&devices(), "yeti"), Some(2));
        assert_eq!(super::match_device(&devices(), "Arctis"), Some(1));
    }

    #[test]
    fn matching_ignores_case_and_surrounding_space() {
        assert_eq!(super::match_device(&devices(), "  YETI  "), Some(2));
    }

    #[test]
    fn an_exact_name_beats_a_longer_device_containing_it() {
        // "Razer Ripsaw HD" is a substring of "Razer Ripsaw HD HDMI".
        let names = vec![
            "Microphone (Razer Ripsaw HD HDMI )".to_string(),
            "Microphone (Razer Ripsaw HD)".to_string(),
        ];
        assert_eq!(super::match_device(&names, "Microphone (Razer Ripsaw HD)"), Some(1));
        // Without an exact match, first substring hit wins.
        assert_eq!(super::match_device(&names, "ripsaw"), Some(0));
    }

    #[test]
    fn no_preference_matches_nothing() {
        assert_eq!(super::match_device(&devices(), ""), None);
        assert_eq!(super::match_device(&devices(), "   "), None);
        assert_eq!(super::match_device(&devices(), "webcam"), None);
    }

    #[test]
    fn silence_and_speech_are_told_apart() {
        assert!(tone(16_000, 16_000, 0.0001).is_silent());
        assert!(tone(1_000, 16_000, 0.5).is_silent(), "too short to be speech");
        assert!(!tone(16_000, 16_000, 0.2).is_silent());
    }
}
