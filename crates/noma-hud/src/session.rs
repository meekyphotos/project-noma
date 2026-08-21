//! The dictation loop: key down, record, decode, paste.
//!
//! Everything here runs off the UI thread. The HUD only ever reads [`UiState`].

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use egui::Context;
use noma_asr::{AsrEngine, EngineSlot};
use noma_audio::Recorder;
use noma_config::{Entry, History, PasteMode, Settings};
use noma_hotkey::PttEvent;
use noma_inject::Delivery;

use crate::{Phase, UiState, PEAK_BINS};

/// A clip longer than this stops getting partial decodes: each pass re-decodes
/// the whole buffer, so past a point the partials cost more than they are worth
/// and would delay the final result.
const MAX_PARTIAL_SECS: f32 = 30.0;

pub struct Session {
    pub ui: Arc<Mutex<UiState>>,
    pub recorder: Recorder,
    pub engine: EngineSlot,
    pub settings: Arc<Settings>,
    pub history: Arc<Mutex<History>>,
    pub wakeup: Arc<Mutex<Option<Context>>>,
    /// True between key down and key up.
    capturing: AtomicBool,
    /// True while a final decode is in flight; a second press is ignored.
    busy: AtomicBool,
}

impl Session {
    pub fn new(
        ui: Arc<Mutex<UiState>>,
        recorder: Recorder,
        engine: EngineSlot,
        settings: Arc<Settings>,
        history: Arc<Mutex<History>>,
        wakeup: Arc<Mutex<Option<Context>>>,
    ) -> Arc<Session> {
        Arc::new(Session {
            ui,
            recorder,
            engine,
            settings,
            history,
            wakeup,
            capturing: AtomicBool::new(false),
            busy: AtomicBool::new(false),
        })
    }

    fn set_phase(&self, phase: Phase) {
        self.ui.lock().expect("ui state").phase = phase;
        self.wake();
    }

    pub fn wake(&self) {
        if let Some(ctx) = self.wakeup.lock().expect("wakeup").as_ref() {
            ctx.request_repaint();
        }
    }

    fn delivery(&self) -> Delivery {
        match self.settings.paste_mode {
            PasteMode::Paste => Delivery::Paste,
            PasteMode::Type => Delivery::Type,
        }
    }
}

/// Listen for the talk key for as long as the app runs.
pub fn spawn(session: Arc<Session>, ptt_rx: Receiver<PttEvent>) {
    thread::Builder::new()
        .name("noma-session".into())
        .spawn(move || {
            while let Ok(event) = ptt_rx.recv() {
                match event {
                    PttEvent::Pressed => press(&session),
                    PttEvent::Released => release(&session),
                }
            }
        })
        .expect("spawn session thread");
}

fn press(session: &Arc<Session>) {
    if session.busy.load(Ordering::SeqCst) {
        return;
    }
    // Say so up front rather than recording into a void.
    if !session.engine.is_ready() {
        session.set_phase(Phase::Error(session.engine.status().message().to_string()));
        return;
    }

    match session.recorder.start() {
        Ok(()) => {
            session.capturing.store(true, Ordering::SeqCst);
            {
                let mut state = session.ui.lock().expect("ui state");
                state.phase = Phase::Listening;
                state.peaks = vec![0.0; PEAK_BINS];
                state.partial.clear();
            }
            eprintln!("noma: listening");
            if session.settings.partials {
                spawn_partials(Arc::clone(session));
            }
        }
        Err(err) => {
            session.set_phase(Phase::Error(format!("mic: {err:#}")));
            eprintln!("noma: mic start failed: {err:#}");
            return;
        }
    }
    session.wake();
}

fn release(session: &Arc<Session>) {
    if !session.capturing.swap(false, Ordering::SeqCst) {
        return;
    }
    let clip = match session.recorder.stop() {
        Ok(clip) => clip,
        Err(err) => {
            session.set_phase(Phase::Error(format!("mic stop: {err:#}")));
            return;
        }
    };

    // A tap of the key, or a held key with nothing said into it.
    if clip.is_silent() {
        let device = session.recorder.device_name();
        eprintln!(
            "noma: nothing to transcribe ({:.1}s from {device}) - is that the mic you spoke into?",
            clip.duration_secs()
        );
        let mut state = session.ui.lock().expect("ui state");
        // A tap of the key is not worth an error; a real hold that recorded
        // silence means something is wrong and saying nothing looks like a
        // broken app. Name the device, because the usual cause is that it is
        // the wrong one.
        state.phase = if clip.duration_secs() < 0.4 {
            Phase::Idle
        } else if device.is_empty() {
            Phase::Error("No sound from the microphone".to_string())
        } else {
            Phase::Error(format!("No sound from {device}"))
        };
        state.partial.clear();
        drop(state);
        session.wake();
        return;
    }

    session.busy.store(true, Ordering::SeqCst);
    session.set_phase(Phase::Transcribing);
    eprintln!("noma: transcribing {:.1}s", clip.duration_secs());

    let session = Arc::clone(session);
    thread::spawn(move || {
        // Pasting while the talk key is still physically down turns our Ctrl+V
        // into a different chord in the target app.
        noma_hotkey::wait_until_released(Duration::from_millis(400));

        let started = Instant::now();
        let outcome = session.engine.transcribe(&clip).map(|transcript| {
            let text = noma_text::process(&transcript.text, &session.settings.text);
            (transcript.text, text)
        });

        match outcome {
            Ok((raw, text)) if text.is_empty() => {
                eprintln!("noma: decoded nothing from {:.1}s (raw {raw:?})", clip.duration_secs());
                session.set_phase(Phase::Idle);
            }
            Ok((raw, text)) => {
                eprintln!(
                    "noma: decoded {:.1}s in {:.2}s -> {text:?}",
                    clip.duration_secs(),
                    started.elapsed().as_secs_f32()
                );
                match noma_inject::deliver(&text, session.delivery()) {
                    Ok(()) => {
                        record(&session, clip.duration_secs(), &raw, &text);
                        let mut state = session.ui.lock().expect("ui state");
                        state.last = text;
                        state.phase = Phase::Idle;
                    }
                    Err(err) => {
                        eprintln!("noma: paste failed: {err:#}");
                        // The text is still worth keeping even if it did not land.
                        record(&session, clip.duration_secs(), &raw, &text);
                        session.ui.lock().expect("ui state").phase =
                            Phase::Error(format!("paste: {err}"));
                    }
                }
            }
            Err(err) => {
                eprintln!("noma: transcribe failed: {err:#}");
                session.ui.lock().expect("ui state").phase = Phase::Error(err.to_string());
            }
        }

        session.ui.lock().expect("ui state").partial.clear();
        session.busy.store(false, Ordering::SeqCst);
        session.wake();
    });
}

fn record(session: &Session, seconds: f32, raw: &str, text: &str) {
    let entry = Entry::new(seconds, raw, text);
    if let Err(err) = session.history.lock().expect("history").record(entry) {
        // Losing a history line is not worth interrupting a dictation over.
        eprintln!("noma: could not write history: {err:#}");
    }
}

/// Decode what has been said so far, over and over, until the key comes up.
///
/// Parakeet has no streaming mode, so each pass re-decodes the whole buffer.
/// That is affordable because the engine is far faster than real time, and the
/// interval adapts so a slow pass never queues up behind itself.
fn spawn_partials(session: Arc<Session>) {
    thread::Builder::new()
        .name("noma-partials".into())
        .spawn(move || {
            let floor = session.settings.partial_interval();
            let mut wait = floor;
            while session.capturing.load(Ordering::SeqCst) {
                thread::sleep(wait);
                if !session.capturing.load(Ordering::SeqCst) {
                    break;
                }

                let clip = session.recorder.snapshot();
                if clip.is_silent() {
                    continue;
                }
                if clip.duration_secs() > MAX_PARTIAL_SECS {
                    break;
                }

                let started = Instant::now();
                let Ok(transcript) = session.engine.transcribe(&clip) else {
                    break;
                };
                // The key may have come up mid-decode; the final result owns
                // the text from here.
                if !session.capturing.load(Ordering::SeqCst) {
                    break;
                }
                {
                    let mut state = session.ui.lock().expect("ui state");
                    state.partial = transcript.text;
                }
                session.wake();

                // Never spend more time decoding than waiting.
                wait = floor.max(started.elapsed());
            }
        })
        .expect("spawn partials thread");
}
