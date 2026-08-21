//! Speech to text.
//!
//! [`AsrEngine`] is the seam: [`ParakeetEngine`] is the real one, [`FakeEngine`]
//! keeps the app testable without half a gigabyte of weights, and [`EngineSlot`]
//! stands in for either while the model is still downloading.

use std::thread;
use std::time::Duration;

use anyhow::Result;
use noma_audio::AudioClip;

mod parakeet;
mod slot;

pub use parakeet::{ParakeetConfig, ParakeetEngine};
pub use slot::{EngineSlot, EngineStatus};

/// The sample rate every engine here works at.
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
}

impl Transcript {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    pub fn is_empty(&self) -> bool {
        self.text.trim().is_empty()
    }
}

pub trait AsrEngine: Send + Sync {
    fn transcribe(&self, clip: &AudioClip) -> Result<Transcript>;

    /// Run a throwaway decode so the first real one is not the slow one.
    ///
    /// ONNX Runtime allocates its arenas lazily, which puts about a second of
    /// setup on whichever decode happens first. Better that it is this one.
    fn warm_up(&self) -> Result<()> {
        let silence = AudioClip {
            samples: vec![0.0; TARGET_SAMPLE_RATE as usize / 2],
            sample_rate: TARGET_SAMPLE_RATE,
        };
        self.transcribe(&silence).map(|_| ())
    }
}

/// Stands in for a real engine: echoes how long the key was held.
pub struct FakeEngine;

impl AsrEngine for FakeEngine {
    fn transcribe(&self, clip: &AudioClip) -> Result<Transcript> {
        let secs = clip.duration_secs();
        thread::sleep(Duration::from_millis(250));
        Ok(Transcript::new(format!("Hello from Noma ({secs:.1}s)")))
    }
}

#[cfg(test)]
mod tests {
    use super::{AsrEngine, FakeEngine, Transcript};
    use noma_audio::AudioClip;

    #[test]
    fn fake_engine_includes_duration() {
        let clip = AudioClip {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
        };
        let transcript = FakeEngine.transcribe(&clip).unwrap();
        assert_eq!(transcript.text, "Hello from Noma (1.0s)");
    }

    #[test]
    fn blank_transcripts_are_recognisable() {
        assert!(Transcript::new("   ").is_empty());
        assert!(!Transcript::new("hello").is_empty());
    }
}
