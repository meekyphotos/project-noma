//! Puts transcribed text into whatever window has focus.

use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};

/// How the text should arrive in the focused app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Delivery {
    /// Clipboard plus Ctrl+V. Instant regardless of length, and correct for
    /// any character the keyboard layout cannot type.
    #[default]
    Paste,
    /// Synthesized keystrokes, for apps that ignore or block paste.
    Type,
}

/// Give the focused window `text`.
pub fn deliver(text: &str, how: Delivery) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    match how {
        Delivery::Paste => paste_text(text),
        Delivery::Type => type_text(text),
    }
}

/// Copy `text` and send Ctrl+V to the focused window, then restore the clipboard.
pub fn paste_text(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("open clipboard")?;
    let previous = clipboard.get_text().ok();
    clipboard.set_text(text).context("set clipboard text")?;

    // Give the target app a moment to see the new clipboard contents.
    thread::sleep(Duration::from_millis(30));

    let mut enigo = Enigo::new(&Settings::default()).context("create input device")?;
    enigo.key(Key::Control, Press).context("ctrl down")?;
    enigo.key(Key::Unicode('v'), Click).context("press v")?;
    enigo.key(Key::Control, Release).context("ctrl up")?;

    // Restoring too early would put the old contents back before the paste has
    // been read, which loses the dictation entirely.
    thread::sleep(Duration::from_millis(80));
    if let Some(previous) = previous {
        let _ = clipboard.set_text(previous);
    }
    Ok(())
}

/// Type `text` one keystroke at a time, leaving the clipboard untouched.
pub fn type_text(text: &str) -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default()).context("create input device")?;
    enigo.text(text).context("type text")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_text_touches_nothing() {
        // No clipboard, no synthetic keys, no error: there is nothing to send.
        assert!(deliver("", Delivery::Paste).is_ok());
        assert!(deliver("", Delivery::Type).is_ok());
    }

    #[test]
    fn paste_is_the_default() {
        assert_eq!(Delivery::default(), Delivery::Paste);
    }
}
