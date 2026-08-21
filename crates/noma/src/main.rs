//! Wires the pieces together and gets out of the way.

use std::sync::Arc;
use std::thread;

use anyhow::{Context, Result};
use noma_asr::{AsrEngine, EngineSlot, FakeEngine, ParakeetConfig, ParakeetEngine};
use noma_audio::Recorder;
use noma_config::{History, Settings};
use noma_hud::HudConfig;

fn main() -> Result<()> {
    let settings = Settings::load().context("load settings")?;
    let key = noma_hotkey::key_or_default(&settings.hotkey);
    let ptt_rx = noma_hotkey::spawn(key).context("start hold-to-talk listener")?;
    let history = History::load(settings.history_limit).context("load history")?;

    // The model can take minutes to arrive on a first run, so the app starts
    // with an empty slot and fills it in from a background thread. The tray,
    // the HUD and the hotkey are all live in the meantime.
    let args: Vec<String> = std::env::args().collect();
    let engine = if args.iter().any(|arg| arg == "--fake") {
        eprintln!("noma: --fake, transcription is a placeholder");
        EngineSlot::ready("fake engine", Arc::new(FakeEngine))
    } else if args.iter().any(|arg| arg == "--preview") {
        // Pin the HUD in its download state so it can be looked at, and
        // screenshotted, without waiting for a real first run.
        eprintln!("noma: --preview, holding the HUD on screen");
        let slot = EngineSlot::loading("Preview");
        slot.set_progress("Downloading model 5% (26 MB of 465 MB)", 4.5);
        slot
    } else {
        let slot = EngineSlot::loading("Looking for the model");
        spawn_engine_loader(slot.clone(), settings.clone());
        slot
    };

    eprintln!("noma: hold {} to talk", key.label);
    match noma_audio::current_device_name(settings.microphone().as_deref()) {
        Some(name) => eprintln!("noma: microphone is {name}"),
        None => eprintln!("noma: no microphone available"),
    }
    noma_hud::run(HudConfig {
        ptt_rx,
        recorder: Recorder::with_device(settings.microphone()),
        engine,
        settings,
        history,
    })
}

/// Fetch the model if needed, open it, and hand it to the slot.
fn spawn_engine_loader(slot: EngineSlot, settings: Settings) {
    thread::Builder::new()
        .name("noma-engine".into())
        .spawn(move || {
            if let Err(err) = load_engine(&slot, &settings) {
                eprintln!("noma: engine unavailable: {err:#}");
                slot.fail(format!("{err}"));
            }
        })
        .expect("spawn engine thread");
}

fn load_engine(slot: &EngineSlot, settings: &Settings) -> Result<()> {
    let spec = noma_model::find(&settings.model)
        .with_context(|| format!("unknown model {:?} in settings", settings.model))?;

    if noma_model::installed(&spec).is_none() {
        eprintln!(
            "noma: fetching {} ({})",
            spec.label,
            noma_model::human_bytes(spec.download_bytes)
        );
    }
    let paths = noma_model::ensure(&spec, &mut |progress| {
        slot.set_progress(progress.message(), progress.percent());
    })
    .with_context(|| format!("get {}", spec.label))?;

    slot.set_progress("Loading model", 97.0);
    let engine = ParakeetEngine::load(ParakeetConfig::new(
        paths,
        settings.provider.as_sherpa(),
        settings.threads,
    ))
    .context("open Parakeet")?;

    // Warm up before announcing readiness: ONNX Runtime allocates on the first
    // decode, and that second belongs here rather than in the first dictation.
    slot.set_progress("Warming up", 99.0);
    let label = engine.label().to_string();
    let engine: Arc<dyn AsrEngine> = Arc::new(engine);
    if let Err(err) = engine.warm_up() {
        eprintln!("noma: warm-up decode failed: {err:#}");
    }
    slot.install(label, engine);
    Ok(())
}
