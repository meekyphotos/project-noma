//! An append-only log of what was dictated.
//!
//! JSON Lines rather than one big document: a dictation is appended with a
//! single write, and a truncated last line costs at most one entry.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One dictation.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Entry {
    /// Unix seconds. Stored as a number so the file never depends on a timezone.
    pub at: u64,
    /// How long the key was held.
    pub seconds: f32,
    /// Exactly what the model decoded, before any cleanup.
    pub raw: String,
    /// What was actually pasted.
    pub text: String,
}

impl Entry {
    pub fn new(seconds: f32, raw: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            at: now_secs(),
            seconds,
            raw: raw.into(),
            text: text.into(),
        }
    }

    /// "just now", "5 min ago", "3 h ago", "yesterday", "4 d ago".
    pub fn age(&self, now: u64) -> String {
        let elapsed = now.saturating_sub(self.at);
        match elapsed {
            0..=59 => "just now".to_string(),
            60..=3_599 => format!("{} min ago", elapsed / 60),
            3_600..=86_399 => format!("{} h ago", elapsed / 3_600),
            86_400..=172_799 => "yesterday".to_string(),
            _ => format!("{} d ago", elapsed / 86_400),
        }
    }

    /// True when the cleanup stage changed the model's output.
    pub fn was_edited(&self) -> bool {
        self.raw.trim() != self.text
    }
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Recent dictations, newest first.
pub struct History {
    path: PathBuf,
    limit: usize,
    entries: Vec<Entry>,
}

impl History {
    /// Load history from the standard location.
    pub fn load(limit: usize) -> Result<History> {
        History::at(super::config_dir()?.join("history.jsonl"), limit)
    }

    /// Load from an explicit path.
    pub fn at(path: PathBuf, limit: usize) -> Result<History> {
        let mut entries = read_entries(&path);
        if entries.len() > limit {
            entries.drain(..entries.len() - limit);
        }
        entries.reverse();
        Ok(History {
            path,
            limit,
            entries,
        })
    }

    /// Newest first.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Append a dictation. A history limit of zero turns recording off.
    pub fn record(&mut self, entry: Entry) -> Result<()> {
        if self.limit == 0 {
            return Ok(());
        }
        self.append(&entry)?;
        self.entries.insert(0, entry);
        if self.entries.len() > self.limit {
            self.entries.truncate(self.limit);
        }
        // Rewriting on every append would cost a full file write per dictation,
        // so let the file run long and compact it occasionally.
        if count_lines(&self.path) > self.limit.saturating_mul(2) {
            self.compact()?;
        }
        Ok(())
    }

    /// Forget everything, on disk and in memory.
    pub fn clear(&mut self) -> Result<()> {
        self.entries.clear();
        if self.path.exists() {
            fs::remove_file(&self.path)
                .with_context(|| format!("remove {}", self.path.display()))?;
        }
        Ok(())
    }

    fn append(&self, entry: &Entry) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let line = serde_json::to_string(entry).context("serialize history entry")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open {}", self.path.display()))?;
        writeln!(file, "{line}").with_context(|| format!("append to {}", self.path.display()))?;
        Ok(())
    }

    /// Rewrite the file with only the entries we still keep, oldest first.
    fn compact(&self) -> Result<()> {
        let mut text = String::new();
        for entry in self.entries.iter().rev() {
            text.push_str(&serde_json::to_string(entry).context("serialize history entry")?);
            text.push('\n');
        }
        fs::write(&self.path, text)
            .with_context(|| format!("rewrite {}", self.path.display()))?;
        Ok(())
    }
}

/// Oldest first. Unreadable lines are skipped rather than failing the load:
/// a corrupt entry should not cost someone their whole history.
fn read_entries(path: &Path) -> Vec<Entry> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn count_lines(path: &Path) -> usize {
    fs::read_to_string(path)
        .map(|text| text.lines().filter(|line| !line.trim().is_empty()).count())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A path in the temp directory that no other test shares.
    fn scratch() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "noma-history-{}-{unique}",
            std::process::id()
        ));
        let _ = fs::create_dir_all(&dir);
        dir.join("history.jsonl")
    }

    fn entry(text: &str) -> Entry {
        Entry::new(1.0, text, text)
    }

    #[test]
    fn missing_file_loads_as_empty() {
        let history = History::at(scratch(), 10).expect("load");
        assert!(history.is_empty());
    }

    #[test]
    fn records_newest_first_and_survives_a_reload() {
        let path = scratch();
        let mut history = History::at(path.clone(), 10).expect("load");
        history.record(entry("first")).expect("record");
        history.record(entry("second")).expect("record");
        assert_eq!(
            history.entries().iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["second", "first"]
        );

        let reloaded = History::at(path, 10).expect("reload");
        assert_eq!(
            reloaded.entries().iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["second", "first"]
        );
    }

    #[test]
    fn the_limit_keeps_the_newest_and_compacts_the_file() {
        let path = scratch();
        let mut history = History::at(path.clone(), 2).expect("load");
        for index in 0..6 {
            history.record(entry(&format!("line {index}"))).expect("record");
        }
        assert_eq!(history.entries().len(), 2);
        assert_eq!(history.entries()[0].text, "line 5");
        // Compaction keeps the file from growing without bound.
        assert!(count_lines(&path) <= 4, "file kept {} lines", count_lines(&path));

        let reloaded = History::at(path, 2).expect("reload");
        assert_eq!(reloaded.entries()[0].text, "line 5");
    }

    #[test]
    fn a_zero_limit_records_nothing() {
        let path = scratch();
        let mut history = History::at(path.clone(), 0).expect("load");
        history.record(entry("secret")).expect("record");
        assert!(history.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn a_corrupt_line_costs_only_itself() {
        let path = scratch();
        let good = serde_json::to_string(&entry("kept")).expect("serialize");
        fs::write(&path, format!("{good}\nnot json at all\n{good}\n")).expect("write");
        let history = History::at(path, 10).expect("load");
        assert_eq!(history.entries().len(), 2);
    }

    #[test]
    fn clear_forgets_the_file_too() {
        let path = scratch();
        let mut history = History::at(path.clone(), 10).expect("load");
        history.record(entry("gone")).expect("record");
        history.clear().expect("clear");
        assert!(history.is_empty());
        assert!(!path.exists());
        assert!(History::at(path, 10).expect("reload").is_empty());
    }

    #[test]
    fn ages_read_the_way_a_person_would_say_them() {
        let now = 1_000_000;
        let at = |seconds: u64| Entry {
            at: now - seconds,
            seconds: 1.0,
            raw: String::new(),
            text: String::new(),
        };
        assert_eq!(at(5).age(now), "just now");
        assert_eq!(at(300).age(now), "5 min ago");
        assert_eq!(at(7_200).age(now), "2 h ago");
        assert_eq!(at(90_000).age(now), "yesterday");
        assert_eq!(at(400_000).age(now), "4 d ago");
    }

    #[test]
    fn a_clock_that_went_backwards_reads_as_just_now() {
        let entry = Entry {
            at: 2_000,
            seconds: 1.0,
            raw: String::new(),
            text: String::new(),
        };
        assert_eq!(entry.age(1_000), "just now");
    }

    #[test]
    fn edits_are_visible() {
        assert!(Entry::new(1.0, "um hello", "Hello").was_edited());
        assert!(!Entry::new(1.0, "Hello", "Hello").was_edited());
    }
}
