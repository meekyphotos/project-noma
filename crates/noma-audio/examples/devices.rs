//! List the microphones Noma can see, and which one it would use.
//!
//! ```bash
//! cargo run -p noma-audio --example devices
//! ```

use cpal::traits::{DeviceTrait, HostTrait};

fn main() {
    let host = cpal::default_host();
    println!("host: {}", host.id().name());

    let default = host.default_input_device();
    let default_name = default
        .as_ref()
        .and_then(|device| device.name().ok())
        .unwrap_or_else(|| "<none>".to_string());

    match host.input_devices() {
        Ok(devices) => {
            println!("\ninput devices:");
            for device in devices {
                let name = device.name().unwrap_or_else(|_| "<unnamed>".to_string());
                let marker = if name == default_name { "->" } else { "  " };
                match device.default_input_config() {
                    Ok(config) => println!(
                        "{marker} {name}\n     {} ch, {} Hz, {:?}",
                        config.channels(),
                        config.sample_rate().0,
                        config.sample_format()
                    ),
                    Err(err) => println!("{marker} {name}\n     (no usable config: {err})"),
                }
            }
        }
        Err(err) => println!("could not enumerate input devices: {err}"),
    }

    println!("\nNoma would record from: {default_name}");
    if let Some(device) = default {
        match device.default_input_config() {
            Ok(config) => {
                println!(
                    "  {} ch at {} Hz, {:?}",
                    config.channels(),
                    config.sample_rate().0,
                    config.sample_format()
                );
                println!(
                    "  downmixed to mono and resampled to 16000 Hz for the model{}",
                    if config.sample_rate().0 == 16_000 {
                        " (already 16 kHz, no resampling)"
                    } else {
                        ""
                    }
                );
            }
            Err(err) => println!("  no usable input config: {err}"),
        }
    }
}
