use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, ViewportBuilder, ViewportCommand,
};
use noma_asr::AsrEngine;
use noma_audio::Recorder;
use noma_hotkey::PttEvent;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

const HUD_WIDTH: f32 = 320.0;
const HUD_HEIGHT: f32 = 72.0;
const PEAK_BINS: usize = 48;
const PARKED: Pos2 = Pos2::new(-4000.0, -4000.0);

#[derive(Clone, Debug)]
pub enum Phase {
    Idle,
    Listening,
    Transcribing,
    Error(String),
}

pub struct UiState {
    pub phase: Phase,
    pub peaks: Vec<f32>,
}

pub struct HudConfig {
    pub ptt_rx: Receiver<PttEvent>,
    pub recorder: Recorder,
    pub asr: Arc<dyn AsrEngine>,
}

struct Session {
    ui: Arc<Mutex<UiState>>,
    recorder: Recorder,
    asr: Arc<dyn AsrEngine>,
    wakeup: Arc<Mutex<Option<egui::Context>>>,
    capturing: AtomicBool,
    busy: AtomicBool,
}

pub fn run(config: HudConfig) -> Result<()> {
    let ui = Arc::new(Mutex::new(UiState {
        phase: Phase::Idle,
        peaks: vec![0.0; PEAK_BINS],
    }));
    let wakeup = Arc::new(Mutex::new(None::<egui::Context>));
    let session = Arc::new(Session {
        ui: Arc::clone(&ui),
        recorder: config.recorder.clone(),
        asr: Arc::clone(&config.asr),
        wakeup: Arc::clone(&wakeup),
        capturing: AtomicBool::new(false),
        busy: AtomicBool::new(false),
    });
    spawn_session(Arc::clone(&session), config.ptt_rx);

    let (tray, preview_item, quit_item) = build_tray().context("create tray icon")?;

    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("Noma")
            .with_inner_size([HUD_WIDTH, HUD_HEIGHT])
            .with_position(PARKED)
            .with_decorations(false)
            .with_transparent(false)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_mouse_passthrough(true)
            .with_visible(true)
            .with_active(false),
        centered: false,
        ..Default::default()
    };

    let recorder = config.recorder;
    eframe::run_native(
        "noma",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(HudApp {
                ui,
                recorder,
                wakeup,
                tray,
                preview_item,
                quit_item,
                preview_until: None,
                error_until: None,
            }))
        }),
    )
    .map_err(|err| anyhow::anyhow!("hud event loop: {err}"))
}

fn spawn_session(session: Arc<Session>, ptt_rx: Receiver<PttEvent>) {
    thread::Builder::new()
        .name("noma-session".into())
        .spawn(move || {
            while let Ok(event) = ptt_rx.recv() {
                match event {
                    PttEvent::Pressed => session_press(&session),
                    PttEvent::Released => session_release(&session),
                }
            }
        })
        .expect("spawn session thread");
}

fn session_press(session: &Session) {
    if session.busy.load(Ordering::SeqCst) {
        return;
    }
    match session.recorder.start() {
        Ok(()) => {
            session.capturing.store(true, Ordering::SeqCst);
            let mut state = session.ui.lock().expect("ui state");
            state.phase = Phase::Listening;
            state.peaks = vec![0.0; PEAK_BINS];
            eprintln!("noma: listening");
        }
        Err(err) => {
            let mut state = session.ui.lock().expect("ui state");
            state.phase = Phase::Error(format!("mic: {err:#}"));
            eprintln!("noma: mic start failed: {err:#}");
        }
    }
    wake(session);
}

fn session_release(session: &Arc<Session>) {
    if !session.capturing.swap(false, Ordering::SeqCst) {
        return;
    }
    let clip = match session.recorder.stop() {
        Ok(clip) => clip,
        Err(err) => {
            session.ui.lock().expect("ui state").phase = Phase::Error(format!("mic stop: {err:#}"));
            wake(session);
            return;
        }
    };

    session.busy.store(true, Ordering::SeqCst);
    session.ui.lock().expect("ui state").phase = Phase::Transcribing;
    wake(session);
    eprintln!("noma: transcribing {:.1}s", clip.duration_secs());

    let session = Arc::clone(session);
    thread::spawn(move || {
        let result = session
            .asr
            .transcribe(&clip)
            .and_then(|transcript| noma_inject::paste_text(&transcript.text).map(|_| transcript));
        {
            let mut state = session.ui.lock().expect("ui state");
            match result {
                Ok(transcript) => {
                    eprintln!("noma: pasted {}", transcript.text);
                    state.phase = Phase::Idle;
                }
                Err(err) => {
                    eprintln!("noma: paste/transcribe failed: {err:#}");
                    state.phase = Phase::Error(err.to_string());
                }
            }
        }
        session.busy.store(false, Ordering::SeqCst);
        wake(&session);
    });
}

fn wake(session: &Session) {
    if let Some(ctx) = session.wakeup.lock().expect("wakeup").as_ref() {
        ctx.request_repaint();
    }
}

struct HudApp {
    ui: Arc<Mutex<UiState>>,
    recorder: Recorder,
    wakeup: Arc<Mutex<Option<egui::Context>>>,
    #[allow(dead_code)]
    tray: TrayIcon,
    preview_item: MenuItem,
    quit_item: MenuItem,
    preview_until: Option<Instant>,
    error_until: Option<Instant>,
}

impl eframe::App for HudApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [10.0 / 255.0, 14.0 / 255.0, 20.0 / 255.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        *self.wakeup.lock().expect("wakeup") = Some(ctx.clone());
        self.poll_menu(ctx);
        self.tick_timers();
        self.sync_peaks(ctx);

        let snapshot = {
            let state = self.ui.lock().expect("ui state");
            (state.phase.clone(), state.peaks.clone())
        };

        let show = !matches!(snapshot.0, Phase::Idle);
        let pos = if show { on_screen_pos(ctx) } else { PARKED };
        ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(true));

        paint_hud(ctx, &snapshot.0, &snapshot.1);

        ctx.request_repaint_after(if show {
            Duration::from_millis(16)
        } else {
            Duration::from_millis(33)
        });
    }
}

impl HudApp {
    fn poll_menu(&mut self, ctx: &egui::Context) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.quit_item.id() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else if event.id == self.preview_item.id() {
                self.preview_until = Some(Instant::now() + Duration::from_secs(3));
                let mut state = self.ui.lock().expect("ui state");
                if matches!(state.phase, Phase::Idle | Phase::Error(_)) {
                    state.phase = Phase::Listening;
                }
            }
        }
    }

    fn tick_timers(&mut self) {
        if let Some(until) = self.preview_until {
            if Instant::now() >= until {
                self.preview_until = None;
                let mut state = self.ui.lock().expect("ui state");
                if matches!(state.phase, Phase::Listening) {
                    state.phase = Phase::Idle;
                }
            }
        }

        if let Phase::Error(_) = self.ui.lock().expect("ui state").phase {
            if self.error_until.is_none() {
                self.error_until = Some(Instant::now() + Duration::from_millis(2500));
            }
        }

        if let Some(until) = self.error_until {
            if Instant::now() >= until {
                self.error_until = None;
                let mut state = self.ui.lock().expect("ui state");
                if matches!(state.phase, Phase::Error(_)) {
                    state.phase = Phase::Idle;
                }
            }
        }
    }

    fn sync_peaks(&mut self, ctx: &egui::Context) {
        let mut state = self.ui.lock().expect("ui state");
        match state.phase {
            Phase::Listening if self.preview_until.is_some() => {
                let t = ctx.input(|i| i.time);
                state.peaks = (0..PEAK_BINS)
                    .map(|i| 0.18 + 0.35 * ((t * 7.0 + i as f64 * 0.28).sin().abs() as f32))
                    .collect();
            }
            Phase::Listening => {
                state.peaks = self.recorder.peaks();
            }
            Phase::Transcribing => {
                let t = ctx.input(|i| i.time);
                let pulse = 0.12 + 0.12 * ((t * 4.0).sin().abs() as f32);
                state.peaks = vec![pulse; PEAK_BINS];
            }
            _ => {}
        }
    }
}

fn on_screen_pos(ctx: &egui::Context) -> Pos2 {
    if let Some(size) = ctx.input(|i| i.viewport().monitor_size) {
        return Pos2::new((size.x - HUD_WIDTH) * 0.5, size.y - HUD_HEIGHT - 56.0);
    }
    let (width, height) = primary_screen_px();
    let scale = primary_scale();
    Pos2::new(
        (width as f32 / scale - HUD_WIDTH) * 0.5,
        height as f32 / scale - HUD_HEIGHT - 56.0,
    )
}

fn paint_hud(ctx: &egui::Context, phase: &Phase, peaks: &[f32]) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            let rect = ui.max_rect().shrink(4.0);
            let painter = ui.painter();
            painter.rect_filled(rect, CornerRadius::same(18), Color32::from_rgb(10, 14, 20));
            painter.rect_stroke(
                rect,
                CornerRadius::same(18),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(46, 196, 184, 90)),
                StrokeKind::Inside,
            );

            let (title, accent) = match phase {
                Phase::Listening => ("Listening", Color32::from_rgb(46, 196, 184)),
                Phase::Transcribing => ("Transcribing", Color32::from_rgb(125, 211, 252)),
                Phase::Error(_) => ("Error", Color32::from_rgb(248, 113, 113)),
                Phase::Idle => ("Noma", Color32::from_rgb(148, 163, 184)),
            };

            painter.text(
                Pos2::new(rect.left() + 16.0, rect.top() + 14.0),
                egui::Align2::LEFT_TOP,
                title,
                egui::FontId::proportional(16.0),
                Color32::from_rgb(241, 245, 249),
            );

            let subtitle = match phase {
                Phase::Error(message) => message.as_str(),
                Phase::Listening => noma_hotkey::PTT_KEY_NAME,
                Phase::Transcribing => "local engine",
                Phase::Idle => "hold Right Ctrl",
            };
            painter.text(
                Pos2::new(rect.left() + 16.0, rect.top() + 36.0),
                egui::Align2::LEFT_TOP,
                subtitle,
                egui::FontId::proportional(11.0),
                Color32::from_rgb(148, 163, 184),
            );

            let wave = Rect::from_min_max(
                Pos2::new(rect.left() + 140.0, rect.top() + 14.0),
                Pos2::new(rect.right() - 16.0, rect.bottom() - 14.0),
            );
            paint_waveform(painter, wave, peaks, accent);
        });
}

fn paint_waveform(painter: &egui::Painter, rect: Rect, peaks: &[f32], color: Color32) {
    if peaks.is_empty() {
        return;
    }
    let n = peaks.len() as f32;
    let gap = 2.0;
    let bar_w = ((rect.width() - gap * (n - 1.0)) / n).max(1.5);
    let mid_y = rect.center().y;
    let max_h = rect.height() * 0.5;

    for (i, peak) in peaks.iter().enumerate() {
        let x = rect.left() + i as f32 * (bar_w + gap);
        let h = (peak.clamp(0.04, 1.0) * max_h).max(2.0);
        let bar = Rect::from_min_max(Pos2::new(x, mid_y - h), Pos2::new(x + bar_w, mid_y + h));
        painter.rect_filled(bar, CornerRadius::same(2), color);
    }
}

fn build_tray() -> Result<(TrayIcon, MenuItem, MenuItem)> {
    let preview = MenuItem::new("Preview HUD", true, None);
    let quit = MenuItem::new("Quit", true, None);
    let menu = Menu::new();
    menu.append(&preview).context("tray preview item")?;
    menu.append(&PredefinedMenuItem::separator())
        .context("tray separator")?;
    menu.append(&quit).context("tray quit item")?;

    let tray = TrayIconBuilder::new()
        .with_tooltip("Noma — hold Right Ctrl to talk")
        .with_icon(tray_icon_image())
        .with_menu(Box::new(menu))
        .build()
        .context("build tray icon")?;

    Ok((tray, preview, quit))
}

fn tray_icon_image() -> Icon {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    let cx = 15.5_f32;
    let cy = 15.5_f32;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            let dist = (dx * dx + dy * dy).sqrt();
            let i = ((y * size + x) * 4) as usize;
            if dist < 13.0 {
                rgba[i] = 0x2E;
                rgba[i + 1] = 0xC4;
                rgba[i + 2] = 0xB8;
                rgba[i + 3] = 255;
            }
        }
    }
    Icon::from_rgba(rgba, size, size).expect("tray rgba icon")
}

#[cfg(windows)]
fn primary_screen_px() -> (i32, i32) {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) }
}

#[cfg(not(windows))]
fn primary_screen_px() -> (i32, i32) {
    (1920, 1080)
}

#[cfg(windows)]
fn primary_scale() -> f32 {
    use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, LOGPIXELSX};
    unsafe {
        let hdc = GetDC(None);
        let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSX);
        (dpi as f32 / 96.0).max(1.0)
    }
}

#[cfg(not(windows))]
fn primary_scale() -> f32 {
    1.0
}
