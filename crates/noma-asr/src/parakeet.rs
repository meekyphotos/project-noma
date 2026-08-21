//! NVIDIA Parakeet TDT running locally through sherpa-onnx.

use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use noma_audio::AudioClip;
use noma_model::ModelPaths;
use sherpa_rs::transducer::{TransducerConfig, TransducerRecognizer};

use crate::{AsrEngine, Transcript, TARGET_SAMPLE_RATE};

/// Mel bins Parakeet was trained on. Wrong values decode to nonsense rather
/// than to an error, so it is pinned here instead of being configurable.
const FEATURE_DIM: i32 = 80;

#[derive(Clone, Debug)]
pub struct ParakeetConfig {
    pub paths: ModelPaths,
    /// ONNX Runtime execution provider: "cpu", "cuda", or "directml".
    pub provider: String,
    pub threads: i32,
}

impl ParakeetConfig {
    pub fn new(paths: ModelPaths, provider: impl Into<String>, threads: i32) -> Self {
        Self {
            paths,
            provider: provider.into(),
            threads: threads.max(1),
        }
    }
}

pub struct ParakeetEngine {
    /// sherpa-onnx decodes through `&mut self`, and one recognizer is all we
    /// want in memory, so decodes queue here. A partial decode and the final
    /// one therefore never overlap.
    recognizer: Mutex<TransducerRecognizer>,
    label: String,
}

impl ParakeetEngine {
    pub fn load(config: ParakeetConfig) -> Result<Self> {
        let paths = &config.paths;
        for path in [
            &paths.encoder,
            &paths.decoder,
            &paths.joiner,
            &paths.tokens,
        ] {
            if !path.is_file() {
                return Err(anyhow!("model file missing: {}", path.display()));
            }
        }

        let recognizer = TransducerRecognizer::new(TransducerConfig {
            encoder: path_string(&paths.encoder)?,
            decoder: path_string(&paths.decoder)?,
            joiner: path_string(&paths.joiner)?,
            tokens: path_string(&paths.tokens)?,
            model_type: paths.model_type.clone(),
            provider: Some(config.provider.clone()),
            num_threads: config.threads,
            sample_rate: TARGET_SAMPLE_RATE as i32,
            feature_dim: FEATURE_DIM,
            decoding_method: "greedy_search".to_string(),
            ..Default::default()
        })
        .map_err(|err| anyhow!("load Parakeet: {err}"))?;

        Ok(Self {
            recognizer: Mutex::new(recognizer),
            label: format!("Parakeet on {}", config.provider),
        })
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl AsrEngine for ParakeetEngine {
    fn transcribe(&self, clip: &AudioClip) -> Result<Transcript> {
        if clip.samples.is_empty() {
            return Ok(Transcript::default());
        }
        // sherpa-onnx would resample for us, but it rebuilds a resampler and
        // logs a banner every call, which is loud at partial-decode cadence.
        let resampled;
        let samples = if clip.sample_rate == TARGET_SAMPLE_RATE {
            &clip.samples
        } else {
            resampled = clip.resampled(TARGET_SAMPLE_RATE);
            &resampled.samples
        };

        let mut recognizer = self
            .recognizer
            .lock()
            .map_err(|_| anyhow!("Parakeet recognizer is poisoned"))?;
        let text = recognizer.transcribe(TARGET_SAMPLE_RATE, samples);
        Ok(Transcript::new(text.trim()))
    }
}

fn path_string(path: &std::path::Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .with_context(|| format!("model path is not valid UTF-8: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use noma_model::PARAKEET_V3;
    use std::path::PathBuf;

    fn missing_paths() -> ModelPaths {
        ModelPaths {
            encoder: PathBuf::from("/nope/encoder.int8.onnx"),
            decoder: PathBuf::from("/nope/decoder.int8.onnx"),
            joiner: PathBuf::from("/nope/joiner.int8.onnx"),
            tokens: PathBuf::from("/nope/tokens.txt"),
            model_type: PARAKEET_V3.model_type.to_string(),
        }
    }

    #[test]
    fn missing_weights_fail_with_the_path_that_is_missing() {
        let config = ParakeetConfig::new(missing_paths(), "cpu", 4);
        let err = ParakeetEngine::load(config)
            .map(|_| ())
            .expect_err("should not load");
        assert!(err.to_string().contains("encoder.int8.onnx"), "{err}");
    }

    #[test]
    fn thread_count_never_drops_below_one() {
        let config = ParakeetConfig::new(missing_paths(), "cpu", 0);
        assert_eq!(config.threads, 1);
    }
}
