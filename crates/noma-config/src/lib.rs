//! Where Noma keeps what it needs to remember between runs.
//!
//! Two files, both under `%APPDATA%\noma`: a hand-editable `settings.toml`
//! and an append-only `history.jsonl` of what was dictated.

use std::fs;
use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

mod history;

pub use history::{now_secs, Entry, History};
pub use noma_text::{Replacement, TextSettings};

/// Which ONNX Runtime backend sherpa-onnx should use.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Works everywhere. Parakeet int8 decodes far faster than real time on it.
    #[default]
    Cpu,
    /// Needs the `cuda` cargo feature and a matching CUDA runtime.
    Cuda,
    /// Windows-only GPU path, needs the `directml` cargo feature.
    DirectMl,
}

impl Provider {
    /// The string sherpa-onnx expects.
    pub fn as_sherpa(&self) -> &'static str {
        match self {
            Provider::Cpu => "cpu",
            Provider::Cuda => "cuda",
            Provider::DirectMl => "directml",
        }
    }
}

/// What lands in the focused app.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PasteMode {
    /// Clipboard plus Ctrl+V. Fast, and correct for any character.
    #[default]
    Paste,
    /// Synthesize keystrokes. Slower, but works where paste is blocked.
    Type,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct Settings {
    /// Id of a model from `noma_model::catalog`.
    pub model: String,
    pub provider: Provider,
    /// ONNX threads. Half the physical cores is a good starting point.
    pub threads: i32,
    /// Id of the hold-to-talk key, see `noma_hotkey::PttKey`.
    pub hotkey: String,
    /// Which microphone to record from, matched as a case-insensitive
    /// substring of the device name ("yeti" finds "Microphone (Yeti Classic)").
    ///
    /// Empty means whatever Windows has set as the default input, which is not
    /// always the mic you are actually speaking into.
    pub microphone: String,
    pub paste_mode: PasteMode,
    /// How opaque the HUD pill is, 0.0 (invisible) to 1.0 (solid).
    ///
    /// Nothing is painted behind the pill, so this is the only thing between
    /// the text and your wallpaper: it trades see-through for readability.
    /// Below about 0.35 the subtitle starts to disappear over a busy
    /// background. `f64` rather than `f32` only so the written file reads
    /// `0.6` instead of `0.6000000238418579`.
    pub hud_opacity: f64,
    /// Decode while the key is held and show the text as it arrives.
    pub partials: bool,
    /// Floor on how often a partial decode may start.
    pub partial_interval_ms: u64,
    /// How many past dictations to keep. Zero disables history entirely.
    pub history_limit: usize,
    pub text: TextSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            model: "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8".to_string(),
            provider: Provider::default(),
            threads: default_threads(),
            hotkey: "right-ctrl".to_string(),
            microphone: String::new(),
            paste_mode: PasteMode::default(),
            hud_opacity: 0.6,
            partials: true,
            partial_interval_ms: 700,
            history_limit: 200,
            text: TextSettings::default(),
        }
    }
}

/// Half the cores, so dictating does not stall whatever you are dictating into.
fn default_threads() -> i32 {
    std::thread::available_parallelism()
        .map(|count| (count.get() / 2).clamp(1, 8) as i32)
        .unwrap_or(4)
}

impl Settings {
    /// Load settings, writing a default file the first time.
    ///
    /// A file that fails to parse is moved aside rather than overwritten, so a
    /// typo never silently costs someone their custom vocabulary.
    pub fn load() -> Result<Settings> {
        let path = settings_path()?;
        let Ok(text) = fs::read_to_string(&path) else {
            let settings = Settings::default();
            let _ = settings.save();
            return Ok(settings);
        };

        match toml::from_str(&text) {
            Ok(settings) => Ok(settings),
            Err(err) => {
                let broken = path.with_extension("toml.invalid");
                let _ = fs::rename(&path, &broken);
                eprintln!(
                    "noma: {} could not be parsed ({err}); kept a copy at {} and using defaults",
                    path.display(),
                    broken.display()
                );
                let settings = Settings::default();
                let _ = settings.save();
                Ok(settings)
            }
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = settings_path()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialize settings")?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    /// The configured microphone, or None to follow the system default.
    pub fn microphone(&self) -> Option<String> {
        let want = self.microphone.trim();
        (!want.is_empty()).then(|| want.to_string())
    }

    /// Partial decodes never start closer together than this.
    pub fn partial_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.partial_interval_ms.max(200))
    }

    /// The pill's alpha, as the 0-255 the painter wants.
    ///
    /// Clamped rather than trusted: a hand-edited 5.0 would paint a solid slab,
    /// which is the exact thing the transparent background is meant to avoid.
    pub fn hud_alpha(&self) -> u8 {
        let opacity = if self.hud_opacity.is_finite() {
            self.hud_opacity.clamp(0.0, 1.0)
        } else {
            0.6
        };
        (opacity * 255.0).round() as u8
    }
}

/// `%APPDATA%\noma` on Windows, `~/.local/share/noma` elsewhere.
pub fn config_dir() -> Result<PathBuf> {
    let base = dirs::data_dir().ok_or_else(|| anyhow!("no application data directory"))?;
    Ok(base.join("noma"))
}

pub fn settings_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("settings.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_toml() {
        let settings = Settings::default();
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: Settings = toml::from_str(&text).expect("parse");
        assert_eq!(parsed, settings);
    }

    #[test]
    fn a_partial_file_fills_in_defaults() {
        let parsed: Settings = toml::from_str("threads = 2\npartials = false\n").expect("parse");
        assert_eq!(parsed.threads, 2);
        assert!(!parsed.partials);
        assert_eq!(parsed.model, Settings::default().model);
        assert_eq!(parsed.text, TextSettings::default());
    }

    #[test]
    fn a_blank_microphone_means_the_system_default() {
        assert_eq!(Settings::default().microphone(), None);
        let blank = Settings {
            microphone: "   ".to_string(),
            ..Settings::default()
        };
        assert_eq!(blank.microphone(), None);
    }

    #[test]
    fn a_named_microphone_is_trimmed_and_kept() {
        let named = Settings {
            microphone: "  yeti  ".to_string(),
            ..Settings::default()
        };
        assert_eq!(named.microphone().as_deref(), Some("yeti"));
    }

    #[test]
    fn provider_names_match_sherpa() {
        assert_eq!(Provider::Cpu.as_sherpa(), "cpu");
        assert_eq!(Provider::Cuda.as_sherpa(), "cuda");
        assert_eq!(Provider::DirectMl.as_sherpa(), "directml");
    }

    #[test]
    fn provider_and_paste_mode_parse_from_plain_words() {
        let parsed: Settings =
            toml::from_str("provider = \"cuda\"\npaste-mode = \"type\"\n").expect("parse");
        assert_eq!(parsed.provider, Provider::Cuda);
        assert_eq!(parsed.paste_mode, PasteMode::Type);
    }

    /// A hand-editable file should not be full of float noise.
    #[test]
    fn opacity_is_written_back_cleanly() {
        let text = toml::to_string_pretty(&Settings::default()).expect("serialize");
        assert!(text.contains("hud-opacity = 0.6"), "{text}");
        assert!(!text.contains("0.6000000"), "float noise in {text}");
    }

    #[test]
    fn hud_alpha_maps_opacity_onto_the_painter_range() {
        let at = |opacity| {
            Settings {
                hud_opacity: opacity,
                ..Settings::default()
            }
            .hud_alpha()
        };
        assert_eq!(at(0.0), 0);
        assert_eq!(at(1.0), 255);
        assert_eq!(at(0.6), 153);
    }

    #[test]
    fn a_hand_edited_opacity_cannot_make_the_hud_solid_again() {
        let at = |opacity| {
            Settings {
                hud_opacity: opacity,
                ..Settings::default()
            }
            .hud_alpha()
        };
        assert_eq!(at(5.0), 255, "clamped, not wrapped");
        assert_eq!(at(-2.0), 0);
        assert_eq!(at(f64::NAN), 153, "falls back to the default");
    }

    #[test]
    fn partial_interval_has_a_floor() {
        let settings = Settings {
            partial_interval_ms: 5,
            ..Settings::default()
        };
        assert_eq!(settings.partial_interval().as_millis(), 200);
    }

    #[test]
    fn thread_default_is_sane() {
        let threads = default_threads();
        assert!((1..=8).contains(&threads));
    }
}
