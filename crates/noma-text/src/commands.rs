//! Phrases a speaker can say to place characters no microphone can hear.
//!
//! Only phrases that are hard to say by accident are listed. "period" and
//! "comma" are deliberately absent: Parakeet punctuates on its own, and
//! dictating a sentence that happens to contain the word "period" is far more
//! likely than wanting a bare full stop.

use crate::normalize;
use crate::render::Piece;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpokenCommand {
    /// The phrase to say, lowercase, words separated by single spaces.
    pub phrase: &'static str,
    /// What lands in the text.
    pub emits: &'static str,
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// Sticks to the previous word with no space in front.
    Punctuation,
    /// Ends the line.
    Break,
}

impl SpokenCommand {
    pub(crate) fn piece(&self) -> Piece {
        match self.kind {
            Kind::Punctuation => Piece::Punctuation(self.emits.to_string()),
            Kind::Break => Piece::Break(self.emits.to_string()),
        }
    }

    /// How the phrase reads in a settings UI.
    pub fn description(&self) -> String {
        match self.kind {
            Kind::Punctuation | Kind::Break if self.emits == "\n" => {
                format!("\"{}\" - line break", self.phrase)
            }
            _ if self.emits == "\n\n" => format!("\"{}\" - blank line", self.phrase),
            _ => format!("\"{}\" - {}", self.phrase, self.emits),
        }
    }
}

const COMMANDS: &[SpokenCommand] = &[
    SpokenCommand {
        phrase: "new line",
        emits: "\n",
        kind: Kind::Break,
    },
    SpokenCommand {
        phrase: "newline",
        emits: "\n",
        kind: Kind::Break,
    },
    SpokenCommand {
        phrase: "new paragraph",
        emits: "\n\n",
        kind: Kind::Break,
    },
    SpokenCommand {
        phrase: "question mark",
        emits: "?",
        kind: Kind::Punctuation,
    },
    SpokenCommand {
        phrase: "exclamation mark",
        emits: "!",
        kind: Kind::Punctuation,
    },
    SpokenCommand {
        phrase: "exclamation point",
        emits: "!",
        kind: Kind::Punctuation,
    },
    SpokenCommand {
        phrase: "semicolon",
        emits: ";",
        kind: Kind::Punctuation,
    },
    SpokenCommand {
        phrase: "ellipsis",
        emits: "...",
        kind: Kind::Punctuation,
    },
];

/// Every command, for showing in settings or docs.
pub fn spoken_commands() -> &'static [SpokenCommand] {
    COMMANDS
}

/// Longest command matching at `start`, with the number of words it consumed.
///
/// A command only matches when the spoken words carry no punctuation of their
/// own, so a decoded "new line." stays literal text.
pub(crate) fn match_at(words: &[&str], start: usize) -> Option<(&'static SpokenCommand, usize)> {
    let mut best: Option<(&'static SpokenCommand, usize)> = None;
    for command in COMMANDS {
        let phrase: Vec<&str> = command.phrase.split(' ').collect();
        if start + phrase.len() > words.len() {
            continue;
        }
        let matched = phrase.iter().enumerate().all(|(offset, want)| {
            let word = words[start + offset];
            normalize(word) == *want && trailing_is_clean(word, offset + 1 == phrase.len())
        });
        if matched && best.is_none_or(|(_, len)| phrase.len() > len) {
            best = Some((command, phrase.len()));
        }
    }
    best
}

/// Words inside a command must be bare. The last word may carry a comma, which
/// is what Parakeet tends to emit around a dictated aside.
fn trailing_is_clean(word: &str, is_last: bool) -> bool {
    let tail = crate::trailing_punctuation(word);
    tail.is_empty() || (is_last && tail == ",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_longest_phrase() {
        let words = ["new", "paragraph", "here"];
        let (command, len) = match_at(&words, 0).expect("match");
        assert_eq!(command.emits, "\n\n");
        assert_eq!(len, 2);
    }

    #[test]
    fn ignores_a_phrase_the_model_punctuated_mid_sentence() {
        let words = ["a", "new.", "line"];
        assert!(match_at(&words, 1).is_none());
    }

    #[test]
    fn tolerates_a_trailing_comma_on_the_last_word() {
        let words = ["new", "line,", "then"];
        let (command, len) = match_at(&words, 0).expect("match");
        assert_eq!(command.emits, "\n");
        assert_eq!(len, 2);
    }

    #[test]
    fn no_match_past_the_end() {
        let words = ["new"];
        assert!(match_at(&words, 0).is_none());
    }

    #[test]
    fn descriptions_are_human_readable() {
        assert!(spoken_commands()
            .iter()
            .any(|command| command.description() == "\"new line\" - line break"));
    }
}
