use std::thread;
use std::time::Duration;

use anyhow::Result;
use noma_audio::AudioClip;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcript {
    pub text: String,
}

pub trait AsrEngine: Send + Sync {
    fn transcribe(&self, clip: &AudioClip) -> Result<Transcript>;
}

pub struct FakeEngine;

impl AsrEngine for FakeEngine {
    fn transcribe(&self, clip: &AudioClip) -> Result<Transcript> {
        let secs = clip.duration_secs();
        thread::sleep(Duration::from_millis(250));
        Ok(Transcript {
            text: format!("Hello from Noma ({secs:.1}s)"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{AsrEngine, FakeEngine};
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
}
