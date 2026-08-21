//! Measure what each microphone is actually hearing.
//!
//! A live mic in a silent room still has a noise floor. A muted, asleep or
//! disconnected one returns digital silence. That difference tells you whether
//! Noma is deaf because of the mic or because of something else.
//!
//! ```bash
//! cargo run -p noma-audio --example levels
//! ```

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat, StreamConfig};

/// Matches the threshold `AudioClip::is_silent` uses to reject a clip.
const SILENCE_RMS: f32 = 0.0015;

/// How long to listen to each device.
const LISTEN: Duration = Duration::from_millis(1500);

#[derive(Default)]
struct Meter {
    peak: f32,
    sum_squares: f64,
    count: usize,
}

fn main() {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok())
        .unwrap_or_default();

    let devices: Vec<_> = match host.input_devices() {
        Ok(devices) => devices.collect(),
        Err(err) => {
            eprintln!("could not enumerate input devices: {err}");
            return;
        }
    };

    println!(
        "listening to each mic for {:.1}s. Speak if you like - but even silence is informative.\n",
        LISTEN.as_secs_f32()
    );

    for device in devices {
        let name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
        let marker = if name == default_name { "-> " } else { "   " };
        match measure(&device) {
            Ok(meter) => {
                let rms = meter.rms();
                let verdict = if meter.count == 0 {
                    "NO DATA - stream produced no samples".to_string()
                } else if meter.peak == 0.0 {
                    "DIGITAL SILENCE - muted, asleep or disconnected".to_string()
                } else if rms < SILENCE_RMS {
                    format!("below Noma's speech threshold ({SILENCE_RMS}) - would be ignored")
                } else {
                    "hearing something".to_string()
                };
                println!("{marker}{name}\n     peak {:.5}  rms {rms:.5}  {verdict}", meter.peak);
            }
            Err(err) => println!("{marker}{name}\n     could not open: {err}"),
        }
    }

    println!("\n-> marks the device Noma records from (the Windows default).");
}

impl Meter {
    fn rms(&self) -> f32 {
        if self.count == 0 {
            return 0.0;
        }
        (self.sum_squares / self.count as f64).sqrt() as f32
    }
}

fn measure(device: &cpal::Device) -> Result<Meter, Box<dyn std::error::Error>> {
    let supported = device.default_input_config()?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let meter = Arc::new(Mutex::new(Meter::default()));
    let shared = Arc::clone(&meter);
    let stream = match sample_format {
        SampleFormat::F32 => build::<f32>(device, &config, shared)?,
        SampleFormat::I16 => build::<i16>(device, &config, shared)?,
        SampleFormat::U16 => build::<u16>(device, &config, shared)?,
        SampleFormat::U8 => build::<u8>(device, &config, shared)?,
        SampleFormat::I32 => build::<i32>(device, &config, shared)?,
        SampleFormat::F64 => build::<f64>(device, &config, shared)?,
        other => return Err(format!("unsupported sample format: {other}").into()),
    };
    stream.play()?;
    thread::sleep(LISTEN);
    drop(stream);

    let meter = meter.lock().expect("meter");
    Ok(Meter {
        peak: meter.peak,
        sum_squares: meter.sum_squares,
        count: meter.count,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    meter: Arc<Mutex<Meter>>,
) -> Result<cpal::Stream, Box<dyn std::error::Error>>
where
    T: Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut meter = meter.lock().expect("meter");
            for sample in data {
                let value = sample.to_sample::<f32>();
                meter.peak = meter.peak.max(value.abs());
                meter.sum_squares += f64::from(value) * f64::from(value);
                meter.count += 1;
            }
        },
        |err| eprintln!("stream error: {err}"),
        None,
    )?;
    Ok(stream)
}
