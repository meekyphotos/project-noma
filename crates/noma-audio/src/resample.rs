//! Band-limited resampling to the 16 kHz the ASR model expects.
//!
//! Microphones hand us 44.1 or 48 kHz. sherpa-onnx would resample for us, but
//! it builds a fresh resampler and logs a banner on every call, which is loud
//! when partial results decode a few times a second. This is the same
//! windowed-sinc filter Kaldi uses, run once per decode.

/// Number of sinc zero crossings kept on each side of a sample.
const FILTER_WIDTH: f64 = 6.0;

/// Resample `samples` from `from` Hz to `to` Hz.
///
/// Returns an empty vector if either rate is zero, and a plain copy when the
/// rates already match.
pub fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == 0 || to == 0 || samples.is_empty() {
        return Vec::new();
    }
    if from == to {
        return samples.to_vec();
    }

    let from_rate = f64::from(from);
    let to_rate = f64::from(to);
    // Stay just under Nyquist of whichever side is narrower.
    let cutoff = 0.99 * 0.5 * from_rate.min(to_rate);
    let window_width = FILTER_WIDTH / (2.0 * cutoff);

    let out_len = (samples.len() as f64 * to_rate / from_rate).floor() as usize;
    let mut out = Vec::with_capacity(out_len);

    for index in 0..out_len {
        let center = index as f64 / to_rate;
        let first = ((center - window_width) * from_rate).ceil().max(0.0) as usize;
        let last = (((center + window_width) * from_rate).floor() as usize).min(samples.len() - 1);

        let mut sum = 0.0;
        for (offset, sample) in samples[first..=last].iter().enumerate() {
            let delta = center - (first + offset) as f64 / from_rate;
            sum += f64::from(*sample) * filter(delta, cutoff, window_width);
        }
        out.push((sum / from_rate) as f32);
    }
    out
}

/// A sinc low-pass windowed by a raised cosine, zero outside the window.
fn filter(delta: f64, cutoff: f64, window_width: f64) -> f64 {
    if delta.abs() >= window_width {
        return 0.0;
    }
    let window = 0.5 * (1.0 + (std::f64::consts::PI * delta / window_width).cos());
    let sinc = if delta == 0.0 {
        2.0 * cutoff
    } else {
        (2.0 * std::f64::consts::PI * cutoff * delta).sin() / (std::f64::consts::PI * delta)
    };
    sinc * window
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, rate: u32, secs: f64) -> Vec<f32> {
        let count = (f64::from(rate) * secs) as usize;
        (0..count)
            .map(|index| {
                let t = index as f64 / f64::from(rate);
                (2.0 * std::f64::consts::PI * freq * t).sin() as f32
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    /// Rising zero crossings tell us the frequency survived the trip.
    fn crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count()
    }

    #[test]
    fn empty_and_degenerate_inputs() {
        assert!(resample(&[], 48_000, 16_000).is_empty());
        assert!(resample(&[0.1, 0.2], 0, 16_000).is_empty());
        assert!(resample(&[0.1, 0.2], 48_000, 0).is_empty());
    }

    #[test]
    fn matching_rates_copy_through() {
        let input = vec![0.1, -0.2, 0.3];
        assert_eq!(resample(&input, 16_000, 16_000), input);
    }

    #[test]
    fn output_length_follows_the_ratio() {
        let input = sine(440.0, 48_000, 1.0);
        assert_eq!(resample(&input, 48_000, 16_000).len(), 16_000);

        let odd = sine(440.0, 44_100, 1.0);
        assert_eq!(resample(&odd, 44_100, 16_000).len(), 16_000);
    }

    #[test]
    fn keeps_frequency_and_amplitude_from_48k() {
        let input = sine(440.0, 48_000, 1.0);
        let output = resample(&input, 48_000, 16_000);
        // One second of 440 Hz has 440 rising crossings, give or take the edges.
        assert!((crossings(&output) as i32 - 440).abs() <= 2);
        assert!((rms(&output) - rms(&input)).abs() < 0.02);
    }

    #[test]
    fn keeps_frequency_and_amplitude_from_44k1() {
        let input = sine(1_000.0, 44_100, 1.0);
        let output = resample(&input, 44_100, 16_000);
        assert!((crossings(&output) as i32 - 1_000).abs() <= 2);
        assert!((rms(&output) - rms(&input)).abs() < 0.02);
    }

    #[test]
    fn rejects_content_above_the_new_nyquist() {
        // 12 kHz cannot exist at a 16 kHz sample rate. Without the low-pass it
        // would fold back to an audible 4 kHz whistle; it has to be filtered out.
        let input = sine(12_000.0, 48_000, 0.5);
        let output = resample(&input, 48_000, 16_000);
        assert!(rms(&output) < 0.05, "aliased energy: {}", rms(&output));
    }

    #[test]
    fn keeps_content_just_below_the_cutoff() {
        // Speech consonants live up here; the filter must not eat them.
        let input = sine(6_000.0, 48_000, 0.5);
        let output = resample(&input, 48_000, 16_000);
        assert!(rms(&output) > 0.6, "lost too much: {}", rms(&output));
    }

    #[test]
    fn silence_stays_silent() {
        let output = resample(&vec![0.0; 48_000], 48_000, 16_000);
        assert!(output.iter().all(|sample| sample.abs() < 1e-6));
    }

    #[test]
    fn upsampling_works_too() {
        let input = sine(440.0, 16_000, 1.0);
        let output = resample(&input, 16_000, 48_000);
        assert_eq!(output.len(), 48_000);
        assert!((crossings(&output) as i32 - 440).abs() <= 2);
    }
}
