//! End-to-end check against the real model.
//!
//! Ignored by default: it needs the ~465 MB Parakeet download, which it will
//! fetch on first run. Run it with:
//!
//! ```bash
//! cargo test -p noma-asr --test parakeet -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::time::Instant;

use noma_asr::{AsrEngine, ParakeetConfig, ParakeetEngine};
use noma_audio::AudioClip;

/// Load the model, downloading it the first time.
fn model() -> noma_model::ModelPaths {
    noma_model::ensure(&noma_model::PARAKEET_V3, &mut |progress| {
        println!("{}", progress.message());
    })
    .expect("get the model")
}

fn sample_path(paths: &noma_model::ModelPaths, name: &str) -> PathBuf {
    paths
        .encoder
        .parent()
        .expect("model directory")
        .join("test_wavs")
        .join(format!("{name}.wav"))
}

/// Minimal 16-bit PCM WAV reader.
///
/// The bundled samples are 22.05 and 24 kHz, so anything that insists on
/// 16 kHz cannot read them. Which is the point: they exercise the resampler
/// the same way a real microphone does.
fn read_wav(path: &Path) -> AudioClip {
    let bytes = std::fs::read(path).unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF file");
    assert_eq!(&bytes[8..12], b"WAVE", "not a WAVE file");

    let u16_at = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let u32_at = |at: usize| {
        u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
    };

    let mut channels = 0usize;
    let mut sample_rate = 0u32;
    let mut bits = 0u16;
    let mut samples = Vec::new();

    // Walk the chunks rather than assuming fmt is first and data is second.
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(at + 4) as usize;
        let body = at + 8;
        match id {
            b"fmt " => {
                channels = u16_at(body + 2) as usize;
                sample_rate = u32_at(body + 4);
                bits = u16_at(body + 14);
            }
            b"data" => {
                let end = (body + size).min(bytes.len());
                assert_eq!(bits, 16, "only 16-bit PCM is supported here");
                let frames = bytes[body..end].chunks_exact(2 * channels.max(1));
                samples = frames
                    .map(|frame| {
                        let sum: f32 = frame
                            .chunks_exact(2)
                            .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32768.0)
                            .sum();
                        sum / channels.max(1) as f32
                    })
                    .collect();
            }
            _ => {}
        }
        // Chunks are word-aligned, so an odd size is followed by a pad byte.
        at = body + size + (size & 1);
    }

    assert!(sample_rate > 0 && !samples.is_empty(), "no audio in the file");
    AudioClip {
        samples,
        sample_rate,
    }
}

fn engine(paths: noma_model::ModelPaths) -> ParakeetEngine {
    let started = Instant::now();
    let engine = ParakeetEngine::load(ParakeetConfig::new(paths, "cpu", 4)).expect("load Parakeet");
    println!("engine loaded in {:.1}s", started.elapsed().as_secs_f32());
    engine.warm_up().expect("warm up");
    engine
}

#[test]
#[ignore = "downloads the model and decodes with it"]
fn parakeet_transcribes_the_bundled_sample() {
    let paths = model();
    let clip = read_wav(&sample_path(&paths, "en"));
    println!(
        "sample: {:.2}s at {} Hz",
        clip.duration_secs(),
        clip.sample_rate
    );

    let engine = engine(paths);

    let started = Instant::now();
    let transcript = engine.transcribe(&clip).expect("transcribe");
    let elapsed = started.elapsed().as_secs_f32();
    println!("decoded in {elapsed:.2}s -> {:?}", transcript.text);
    println!(
        "real-time factor: {:.3} ({}x faster than real time)",
        elapsed / clip.duration_secs(),
        (clip.duration_secs() / elapsed).round() as i32
    );

    assert!(!transcript.is_empty(), "decoded nothing");
    // Parakeet punctuates and capitalizes on its own; if that ever stops being
    // true, the text stage has to start doing it.
    assert!(
        transcript.text.chars().any(char::is_uppercase),
        "expected capitalization in {:?}",
        transcript.text
    );
    assert!(
        transcript.text.contains(['.', ',', '?', '!']),
        "expected punctuation in {:?}",
        transcript.text
    );
}

/// v3 is the multilingual model, so the other samples should decode too.
#[test]
#[ignore = "needs the model on disk"]
fn parakeet_handles_the_other_languages() {
    let paths = noma_model::installed(&noma_model::PARAKEET_V3).expect("download the model first");
    let engine = engine(paths.clone());

    for language in ["de", "fr", "es"] {
        let clip = read_wav(&sample_path(&paths, language));
        let transcript = engine.transcribe(&clip).expect("transcribe");
        println!("{language}: {:?}", transcript.text);
        assert!(!transcript.is_empty(), "{language} decoded nothing");
    }
}

/// The same audio through the resampler must decode to the same words, since
/// resampling is what every real microphone input goes through.
#[test]
#[ignore = "needs the model on disk"]
fn resampled_audio_decodes_the_same() {
    let paths = noma_model::installed(&noma_model::PARAKEET_V3).expect("download the model first");
    let clip = read_wav(&sample_path(&paths, "en"));
    let engine = engine(paths);

    let direct = engine.transcribe(&clip).expect("transcribe at source rate");

    // Up to 48 kHz first, the way a 48 kHz microphone would hand it over.
    let upsampled = clip.resampled(48_000);
    assert_eq!(upsampled.sample_rate, 48_000);
    let round_tripped = engine.transcribe(&upsampled).expect("transcribe at 48k");

    println!("{} Hz: {:?}", clip.sample_rate, direct.text);
    println!("48000 Hz: {:?}", round_tripped.text);
    assert_eq!(direct.text, round_tripped.text);
}
