//! Turns a raw ASR hypothesis into text worth pasting.
//!
//! Parakeet already emits punctuation and casing, so this stage is about the
//! things a speech model cannot know: line breaks the speaker asked for out
//! loud, filler words they did not mean to say, and names the model spells its
//! own way ("no ma" for "Noma").

use serde::{Deserialize, Serialize};

mod commands;
mod render;

pub use commands::{spoken_commands, SpokenCommand};

use render::{render, Piece};

/// A user-defined rewrite, applied after fillers are dropped.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct Replacement {
    /// Spoken phrase to look for, matched case-insensitively on whole words.
    pub from: String,
    /// Text to write instead.
    pub to: String,
}

impl Replacement {
    pub fn new(from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case")]
pub struct TextSettings {
    /// Master switch: off means the transcript is pasted exactly as decoded.
    pub enabled: bool,
    /// Turn "new line", "period", "question mark" into the characters they name.
    pub spoken_commands: bool,
    /// Drop hesitation sounds.
    pub remove_fillers: bool,
    /// Words treated as fillers, lowercase and without punctuation.
    pub fillers: Vec<String>,
    /// Custom vocabulary fixes.
    pub replacements: Vec<Replacement>,
    /// Capitalize the first word of the text and of every new sentence.
    pub capitalize_sentences: bool,
    /// Add a final period when the text ends without punctuation.
    pub ensure_final_punctuation: bool,
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            spoken_commands: true,
            remove_fillers: true,
            fillers: default_fillers(),
            replacements: Vec::new(),
            capitalize_sentences: true,
            ensure_final_punctuation: false,
        }
    }
}

/// Hesitation sounds that are safe to drop: none of them is also a real word.
pub fn default_fillers() -> Vec<String> {
    ["um", "uh", "uhm", "erm", "eh", "mm", "hmm", "mhm"]
        .iter()
        .map(|filler| filler.to_string())
        .collect()
}

/// Apply the whole pipeline. Returns the text to paste.
pub fn process(raw: &str, settings: &TextSettings) -> String {
    if !settings.enabled {
        return raw.trim().to_string();
    }

    let words: Vec<&str> = raw.split_whitespace().collect();
    let mut pieces: Vec<Piece> = Vec::with_capacity(words.len());
    let mut index = 0;

    while index < words.len() {
        if settings.spoken_commands {
            if let Some((command, len)) = commands::match_at(&words, index) {
                pieces.push(command.piece());
                index += len;
                continue;
            }
        }
        if let Some((replacement, len)) = match_replacement(&words, index, &settings.replacements) {
            // Keep whatever punctuation the model attached to the last matched word.
            let tail = trailing_punctuation(words[index + len - 1]);
            pieces.push(Piece::Word(format!("{replacement}{tail}")));
            index += len;
            continue;
        }
        if settings.remove_fillers && is_filler(words[index], &settings.fillers) {
            // A filler carrying punctuation ("um,") leaves the punctuation behind.
            let tail = trailing_punctuation(words[index]);
            if !tail.is_empty() {
                pieces.push(Piece::Punctuation(tail.to_string()));
            }
            index += 1;
            continue;
        }
        pieces.push(Piece::Word(words[index].to_string()));
        index += 1;
    }

    render(&pieces, settings)
}

/// Lowercase a word with its surrounding punctuation stripped.
fn normalize(word: &str) -> String {
    word.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// The punctuation hanging off the end of a word, if any.
fn trailing_punctuation(word: &str) -> &str {
    match word.rfind(char::is_alphanumeric) {
        Some(index) => {
            let last = word[index..].chars().next().map_or(1, char::len_utf8);
            &word[index + last..]
        }
        None => word,
    }
}

fn is_filler(word: &str, fillers: &[String]) -> bool {
    let normalized = normalize(word);
    !normalized.is_empty() && fillers.iter().any(|filler| filler == &normalized)
}

/// Longest-match a replacement phrase starting at `start`. Returns the
/// replacement text and how many words it consumed.
fn match_replacement<'a>(
    words: &[&str],
    start: usize,
    replacements: &'a [Replacement],
) -> Option<(&'a str, usize)> {
    let mut best: Option<(&str, usize)> = None;
    for replacement in replacements {
        let phrase: Vec<String> = replacement
            .from
            .split_whitespace()
            .map(normalize)
            .filter(|word| !word.is_empty())
            .collect();
        if phrase.is_empty() || start + phrase.len() > words.len() {
            continue;
        }
        let matched = phrase
            .iter()
            .enumerate()
            .all(|(offset, want)| &normalize(words[start + offset]) == want);
        if matched && best.is_none_or(|(_, len)| phrase.len() > len) {
            best = Some((replacement.to.as_str(), phrase.len()));
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> TextSettings {
        TextSettings::default()
    }

    /// The whole settings file is kebab-case; this section must not drift.
    #[test]
    fn settings_keys_are_kebab_case() {
        let text = toml::to_string(&TextSettings::default()).expect("serialize");
        assert!(text.contains("spoken-commands"), "{text}");
        assert!(text.contains("remove-fillers"), "{text}");
        assert!(text.contains("capitalize-sentences"), "{text}");
        assert!(text.contains("ensure-final-punctuation"), "{text}");
        assert!(!text.contains('_'), "snake_case leaked into {text}");
    }

    #[test]
    fn passthrough_when_disabled() {
        let mut settings = settings();
        settings.enabled = false;
        assert_eq!(process("  um hello there  ", &settings), "um hello there");
    }

    #[test]
    fn drops_fillers() {
        assert_eq!(process("um hello uh there", &settings()), "Hello there");
    }

    #[test]
    fn keeps_words_that_merely_contain_a_filler() {
        assert_eq!(
            process("the umbrella is uh red", &settings()),
            "The umbrella is red"
        );
    }

    #[test]
    fn filler_keeps_its_punctuation() {
        assert_eq!(
            process("well um, that works", &settings()),
            "Well, that works"
        );
    }

    #[test]
    fn spoken_new_line_breaks_the_text() {
        assert_eq!(
            process("first line new line second line", &settings()),
            "First line\nSecond line"
        );
    }

    #[test]
    fn spoken_punctuation_attaches_to_the_previous_word() {
        assert_eq!(
            process("does it work question mark", &settings()),
            "Does it work?"
        );
    }

    #[test]
    fn replacements_win_over_fillers_and_keep_punctuation() {
        let mut settings = settings();
        settings.replacements = vec![Replacement::new("no ma", "Noma")];
        assert_eq!(process("i use no ma, daily", &settings), "I use Noma, daily");
    }

    #[test]
    fn longest_replacement_wins() {
        let mut settings = settings();
        settings.replacements = vec![
            Replacement::new("open", "OPEN"),
            Replacement::new("open a i", "OpenAI"),
        ];
        assert_eq!(process("open a i ships", &settings), "OpenAI ships");
    }

    #[test]
    fn capitalizes_after_sentence_end() {
        assert_eq!(
            process("hello there. how are you?", &settings()),
            "Hello there. How are you?"
        );
    }

    #[test]
    fn optional_final_period() {
        let mut settings = settings();
        settings.ensure_final_punctuation = true;
        assert_eq!(
            process("no trailing period", &settings),
            "No trailing period."
        );
        assert_eq!(process("already there!", &settings), "Already there!");
    }

    #[test]
    fn empty_input_stays_empty() {
        assert_eq!(process("   ", &settings()), "");
    }

    #[test]
    fn all_filler_input_stays_empty() {
        assert_eq!(process("um uh hmm", &settings()), "");
    }

    #[test]
    fn trailing_punctuation_handles_unicode() {
        assert_eq!(trailing_punctuation("café,"), ",");
        assert_eq!(trailing_punctuation("café"), "");
        assert_eq!(trailing_punctuation("..."), "...");
    }
}
