//! Joins processed pieces back into a string a person would have typed.

use crate::TextSettings;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Piece {
    /// A word, still carrying whatever punctuation the model attached to it.
    Word(String),
    /// Punctuation that sticks to the previous word.
    Punctuation(String),
    /// A line break, spoken out loud.
    Break(String),
}

pub(crate) fn render(pieces: &[Piece], settings: &TextSettings) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    let mut capitalize = settings.capitalize_sentences;

    for piece in pieces {
        match piece {
            Piece::Word(word) => {
                if pending_space {
                    out.push(' ');
                }
                if capitalize {
                    out.push_str(&capitalized(word));
                } else {
                    out.push_str(word);
                }
                pending_space = true;
                capitalize = settings.capitalize_sentences && ends_sentence(word);
            }
            Piece::Punctuation(mark) => {
                if out.is_empty() {
                    continue;
                }
                out.push_str(mark);
                pending_space = true;
                capitalize = settings.capitalize_sentences && ends_sentence(mark);
            }
            Piece::Break(newlines) => {
                if out.is_empty() {
                    continue;
                }
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push_str(newlines);
                pending_space = false;
                capitalize = settings.capitalize_sentences;
            }
        }
    }

    while out.ends_with(' ') {
        out.pop();
    }

    if settings.ensure_final_punctuation && wants_final_period(&out) {
        out.push('.');
    }
    out
}

/// Uppercase the first letter, leaving any leading quote or bracket alone.
fn capitalized(word: &str) -> String {
    let Some(index) = word.find(char::is_alphabetic) else {
        return word.to_string();
    };
    let mut out = String::with_capacity(word.len());
    out.push_str(&word[..index]);
    let mut chars = word[index..].chars();
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
        out.push_str(chars.as_str());
    }
    out
}

/// True when the text ends a sentence, ignoring closing quotes or brackets.
fn ends_sentence(text: &str) -> bool {
    text.trim_end_matches(['"', '\'', ')', ']', '}', '»'])
        .ends_with(['.', '!', '?'])
}

fn wants_final_period(text: &str) -> bool {
    text.chars().next_back().is_some_and(char::is_alphanumeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() -> TextSettings {
        TextSettings {
            capitalize_sentences: false,
            ensure_final_punctuation: false,
            ..TextSettings::default()
        }
    }

    fn word(text: &str) -> Piece {
        Piece::Word(text.to_string())
    }

    #[test]
    fn punctuation_hugs_the_previous_word() {
        let pieces = vec![word("hello"), Piece::Punctuation("?".into()), word("yes")];
        assert_eq!(render(&pieces, &plain()), "hello? yes");
    }

    #[test]
    fn leading_punctuation_is_dropped() {
        let pieces = vec![Piece::Punctuation("?".into()), word("yes")];
        assert_eq!(render(&pieces, &plain()), "yes");
    }

    #[test]
    fn breaks_eat_the_space_before_them() {
        let pieces = vec![word("one"), Piece::Break("\n".into()), word("two")];
        assert_eq!(render(&pieces, &plain()), "one\ntwo");
    }

    #[test]
    fn capitalizes_after_a_break_and_after_a_full_stop() {
        let pieces = vec![
            word("one."),
            word("two"),
            Piece::Break("\n".into()),
            word("three"),
        ];
        assert_eq!(render(&pieces, &TextSettings::default()), "One. Two\nThree");
    }

    #[test]
    fn quoted_sentence_end_still_counts() {
        assert!(ends_sentence("done.\""));
        assert!(!ends_sentence("mid,"));
    }

    #[test]
    fn capitalizing_skips_a_leading_quote() {
        assert_eq!(capitalized("\"hello"), "\"Hello");
        assert_eq!(capitalized("42"), "42");
    }

    #[test]
    fn no_final_period_after_a_break() {
        let pieces = vec![word("one"), Piece::Break("\n".into())];
        let settings = TextSettings {
            ensure_final_punctuation: true,
            ..TextSettings::default()
        };
        assert_eq!(render(&pieces, &settings), "One\n");
    }
}
