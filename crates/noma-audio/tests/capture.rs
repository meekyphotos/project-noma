//! Smoke test against the real microphone.
//!
//! Ignored by default: it needs an input device, and CI machines rarely have
//! one. Run it with:
//!
//! ```bash
//! cargo test -p noma-audio --test capture -- --ignored --nocapture
//! ```

use std::thread;
use std::time::{Duration, Instant};

use noma_audio::Recorder;

#[test]
#[ignore = "needs a microphone"]
fn capture_snapshot_and_stop() {
    let recorder = Recorder::new();
    recorder.start().expect("start capture");

    thread::sleep(Duration::from_millis(600));

    // Partial decoding reads the buffer while the stream is still running.
    let started = Instant::now();
    let snapshot = recorder.snapshot();
    let snapshot_cost = started.elapsed();
    println!(
        "snapshot: {} samples at {} Hz ({:.2}s) in {:.2}ms",
        snapshot.samples.len(),
        snapshot.sample_rate,
        snapshot.duration_secs(),
        snapshot_cost.as_secs_f32() * 1000.0
    );

    assert!(snapshot.sample_rate >= 8_000, "implausible sample rate");
    assert!(!snapshot.samples.is_empty(), "captured nothing");
    // The audio callback waits on the same lock, so a slow snapshot would show
    // up as dropouts in the recording.
    assert!(
        snapshot_cost < Duration::from_millis(5),
        "snapshot took {snapshot_cost:?}, which would stall the audio callback"
    );

    thread::sleep(Duration::from_millis(400));

    let clip = recorder.stop().expect("stop capture");
    println!(
        "clip: {:.2}s at {} Hz",
        clip.duration_secs(),
        clip.sample_rate
    );
    assert!(
        clip.samples.len() > snapshot.samples.len(),
        "capture did not continue past the snapshot"
    );
    assert!(clip.duration_secs() > 0.8, "clip is shorter than the hold");

    // 16 kHz is what the model wants, whatever the device runs at.
    let resampled = clip.resampled(16_000);
    assert_eq!(resampled.sample_rate, 16_000);
    let expected = (clip.duration_secs() * 16_000.0) as usize;
    assert!(
        resampled.samples.len().abs_diff(expected) < 100,
        "resampled to {} samples, expected about {expected}",
        resampled.samples.len()
    );

    println!("peaks: {} bins", recorder.peaks().len());
}

#[test]
#[ignore = "needs a microphone"]
fn a_second_start_is_harmless() {
    let recorder = Recorder::new();
    recorder.start().expect("start capture");
    recorder.start().expect("starting twice should be a no-op");
    thread::sleep(Duration::from_millis(200));
    let clip = recorder.stop().expect("stop capture");
    assert!(!clip.samples.is_empty(), "captured nothing");
}
