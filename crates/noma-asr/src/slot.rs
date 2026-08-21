//! A handle to an engine that may not exist yet.
//!
//! The first run spends a few minutes downloading Parakeet. The HUD, the tray
//! and the hotkey are all live during that time, so they need something to hold
//! that can answer "not yet" without any of them knowing about downloads.

use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use noma_audio::AudioClip;

use crate::{AsrEngine, Transcript};

/// What the slot can tell the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum EngineStatus {
    /// Still getting ready: a line to show and how far along it is, 0 to 100.
    Loading { message: String, percent: f32 },
    /// Ready to decode. Carries the engine's label, e.g. "Parakeet on cpu".
    Ready(String),
    /// Gave up. Carries the reason.
    Failed(String),
}

impl EngineStatus {
    pub fn is_ready(&self) -> bool {
        matches!(self, EngineStatus::Ready(_))
    }

    /// The line to put in front of the user.
    pub fn message(&self) -> &str {
        match self {
            EngineStatus::Loading { message, .. }
            | EngineStatus::Ready(message)
            | EngineStatus::Failed(message) => message,
        }
    }
}

enum State {
    Pending(EngineStatus),
    Ready { label: String, engine: Arc<dyn AsrEngine> },
}

/// Cheap to clone; every clone sees the same engine.
#[derive(Clone)]
pub struct EngineSlot {
    state: Arc<RwLock<State>>,
}

impl EngineSlot {
    /// A slot that is not ready yet, showing `message`.
    pub fn loading(message: impl Into<String>) -> Self {
        Self {
            state: Arc::new(RwLock::new(State::Pending(EngineStatus::Loading {
                message: message.into(),
                percent: 0.0,
            }))),
        }
    }

    /// A slot that already holds an engine.
    pub fn ready(label: impl Into<String>, engine: Arc<dyn AsrEngine>) -> Self {
        Self {
            state: Arc::new(RwLock::new(State::Ready {
                label: label.into(),
                engine,
            })),
        }
    }

    /// Update what is shown while loading. Ignored once an engine is in.
    pub fn set_progress(&self, message: impl Into<String>, percent: f32) {
        let mut state = self.write();
        if let State::Pending(status) = &mut *state {
            *status = EngineStatus::Loading {
                message: message.into(),
                percent: percent.clamp(0.0, 100.0),
            };
        }
    }

    /// Hand over the engine. From here on decoding works.
    pub fn install(&self, label: impl Into<String>, engine: Arc<dyn AsrEngine>) {
        *self.write() = State::Ready {
            label: label.into(),
            engine,
        };
    }

    /// Record that the engine will never arrive, and why.
    pub fn fail(&self, reason: impl Into<String>) {
        let mut state = self.write();
        if !matches!(*state, State::Ready { .. }) {
            *state = State::Pending(EngineStatus::Failed(reason.into()));
        }
    }

    pub fn status(&self) -> EngineStatus {
        match &*self.read() {
            State::Pending(status) => status.clone(),
            State::Ready { label, .. } => EngineStatus::Ready(label.clone()),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(*self.read(), State::Ready { .. })
    }

    /// The engine, if there is one.
    pub fn engine(&self) -> Option<Arc<dyn AsrEngine>> {
        match &*self.read() {
            State::Ready { engine, .. } => Some(Arc::clone(engine)),
            State::Pending(_) => None,
        }
    }

    // A poisoned lock here means another thread panicked mid-swap. The stored
    // value is a plain enum that is always valid, so carrying on is safe.
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.state.read().unwrap_or_else(|err| err.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.state.write().unwrap_or_else(|err| err.into_inner())
    }
}

impl AsrEngine for EngineSlot {
    fn transcribe(&self, clip: &AudioClip) -> Result<Transcript> {
        let engine = self
            .engine()
            .ok_or_else(|| anyhow!("{}", self.status().message()))?;
        engine.transcribe(clip)
    }

    fn warm_up(&self) -> Result<()> {
        match self.engine() {
            Some(engine) => engine.warm_up(),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeEngine;

    #[test]
    fn a_loading_slot_refuses_with_its_own_message() {
        let slot = EngineSlot::loading("Downloading model 12%");
        assert!(!slot.is_ready());
        let clip = AudioClip {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
        };
        let err = slot.transcribe(&clip).expect_err("not ready yet");
        assert_eq!(err.to_string(), "Downloading model 12%");
    }

    #[test]
    fn progress_updates_the_message_and_the_bar() {
        let slot = EngineSlot::loading("starting");
        slot.set_progress("Downloading model 50%", 45.0);
        assert_eq!(
            slot.status(),
            EngineStatus::Loading {
                message: "Downloading model 50%".into(),
                percent: 45.0
            }
        );
    }

    #[test]
    fn a_nonsense_percent_is_clamped() {
        let slot = EngineSlot::loading("starting");
        slot.set_progress("weird", 900.0);
        match slot.status() {
            EngineStatus::Loading { percent, .. } => assert_eq!(percent, 100.0),
            other => panic!("expected loading, got {other:?}"),
        }
    }

    #[test]
    fn installing_makes_it_decode() {
        let slot = EngineSlot::loading("starting");
        slot.install("fake", Arc::new(FakeEngine));
        assert!(slot.is_ready());
        assert_eq!(slot.status(), EngineStatus::Ready("fake".into()));

        let clip = AudioClip {
            samples: vec![0.0; 16_000],
            sample_rate: 16_000,
        };
        assert_eq!(
            slot.transcribe(&clip).expect("decode").text,
            "Hello from Noma (1.0s)"
        );
    }

    #[test]
    fn failure_is_reported_to_the_caller() {
        let slot = EngineSlot::loading("starting");
        slot.fail("no network");
        assert_eq!(slot.status(), EngineStatus::Failed("no network".into()));
        let clip = AudioClip {
            samples: vec![0.0; 8_000],
            sample_rate: 16_000,
        };
        assert_eq!(
            slot.transcribe(&clip).expect_err("failed").to_string(),
            "no network"
        );
    }

    #[test]
    fn a_late_failure_cannot_unseat_a_working_engine() {
        let slot = EngineSlot::ready("fake", Arc::new(FakeEngine));
        slot.fail("too late");
        slot.set_progress("also too late", 10.0);
        assert!(slot.is_ready());
    }

    #[test]
    fn clones_share_one_engine() {
        let slot = EngineSlot::loading("starting");
        let other = slot.clone();
        slot.install("fake", Arc::new(FakeEngine));
        assert!(other.is_ready());
    }
}
