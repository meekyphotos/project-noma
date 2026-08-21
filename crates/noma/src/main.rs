use std::sync::Arc;

use anyhow::Context;
use noma_asr::FakeEngine;
use noma_audio::Recorder;
use noma_hud::HudConfig;

fn main() -> anyhow::Result<()> {
    let ptt_rx = noma_hotkey::spawn().context("start hold-to-talk listener")?;
    noma_hud::run(HudConfig {
        ptt_rx,
        recorder: Recorder::new(),
        asr: Arc::new(FakeEngine),
    })
}
