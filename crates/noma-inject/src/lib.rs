use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};

/// Copy `text` and send Ctrl+V to the focused window, then restore the clipboard.
pub fn paste_text(text: &str) -> Result<()> {
    let mut clipboard = Clipboard::new().context("open clipboard")?;
    let previous = clipboard.get_text().ok();
    clipboard.set_text(text).context("set clipboard text")?;

    thread::sleep(Duration::from_millis(30));

    let mut enigo = Enigo::new(&Settings::default()).context("create input device")?;
    enigo.key(Key::Control, Press).context("ctrl down")?;
    enigo.key(Key::Unicode('v'), Click).context("press v")?;
    enigo.key(Key::Control, Release).context("ctrl up")?;

    thread::sleep(Duration::from_millis(80));
    if let Some(previous) = previous {
        let _ = clipboard.set_text(previous);
    }
    Ok(())
}
