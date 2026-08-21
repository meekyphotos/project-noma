use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use eframe::egui::{
    self, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, ViewportBuilder, ViewportCommand,
};
use noma_asr::{EngineSlot, EngineStatus};
use noma_audio::Recorder;
use noma_config::{History, Settings};
use noma_hotkey::PttEvent;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

mod history_window;
mod session;

use session::Session;

const HUD_WIDTH: f32 = 380.0;
const HUD_HEIGHT: f32 = 64.0;
pub(crate) const PEAK_BINS: usize = 22;
const PARKED: Pos2 = Pos2::new(-4000.0, -4000.0);
/// Roughly how many characters of partial text fit on the subtitle line.
const SUBTITLE_CHARS: usize = 46;

#[derive(Clone, Debug)]
pub enum Phase {
    Idle,
    /// The model is still being fetched or opened.
    Loading { message: String, percent: f32 },
    Listening,
    Transcribing,
    Error(String),
}

pub struct UiState {
    pub phase: Phase,
    pub peaks: Vec<f32>,
    /// Text decoded so far, while the key is still held.
    pub partial: String,
    /// The last thing pasted, for the tray's copy action.
    pub last: String,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            phase: Phase::Idle,
            peaks: vec![0.0; PEAK_BINS],
            partial: String::new(),
            last: String::new(),
        }
    }
}

pub struct HudConfig {
    pub ptt_rx: Receiver<PttEvent>,
    pub recorder: Recorder,
    /// The engine, which may still be loading.
    pub engine: EngineSlot,
    pub settings: Settings,
    pub history: History,
}

pub fn run(config: HudConfig) -> Result<()> {
    let ui = Arc::new(Mutex::new(UiState::default()));
    let wakeup = Arc::new(Mutex::new(None::<egui::Context>));
    let settings = Arc::new(config.settings);
    let hud_alpha = settings.hud_alpha();
    let history = Arc::new(Mutex::new(config.history));

    let session = Session::new(
        Arc::clone(&ui),
        config.recorder.clone(),
        config.engine.clone(),
        Arc::clone(&settings),
        Arc::clone(&history),
        Arc::clone(&wakeup),
    );
    session::spawn(Arc::clone(&session), config.ptt_rx);

    let tray = Tray::build().context("create tray icon")?;

    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_title("Noma")
            .with_inner_size([HUD_WIDTH, HUD_HEIGHT])
            .with_position(PARKED)
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_taskbar(false)
            .with_resizable(false)
            .with_mouse_passthrough(false)
            .with_visible(true)
            .with_active(false),
        centered: false,
        ..Default::default()
    };

    let recorder = config.recorder;
    let engine = config.engine;
    eframe::run_native(
        "noma",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            Ok(Box::new(HudApp {
                ui,
                recorder,
                engine,
                hud_alpha,
                history,
                wakeup,
                tray,
                show_history: Arc::new(AtomicBool::new(false)),
                preview_until: None,
                error_until: None,
                last_shown: None,
                engine_settled: false,
            }))
        }),
    )
    .map_err(|err| anyhow::anyhow!("hud event loop: {err}"))
}

/// The tray icon and the menu items we need to compare events against.
struct Tray {
    #[allow(dead_code)]
    icon: TrayIcon,
    preview: MenuItem,
    history: MenuItem,
    copy_last: MenuItem,
    open_folder: MenuItem,
    quit: MenuItem,
}

impl Tray {
    fn build() -> Result<Tray> {
        let preview = MenuItem::new("Preview HUD", true, None);
        let history = MenuItem::new("History...", true, None);
        let copy_last = MenuItem::new("Copy last transcript", true, None);
        let open_folder = MenuItem::new("Open settings folder", true, None);
        let quit = MenuItem::new("Quit", true, None);

        let menu = Menu::new();
        menu.append(&preview).context("tray preview item")?;
        menu.append(&history).context("tray history item")?;
        menu.append(&copy_last).context("tray copy item")?;
        menu.append(&PredefinedMenuItem::separator())
            .context("tray separator")?;
        menu.append(&open_folder).context("tray folder item")?;
        menu.append(&quit).context("tray quit item")?;

        let icon = TrayIconBuilder::new()
            .with_tooltip(format!("Noma - hold {} to talk", noma_hotkey::key_label()))
            .with_icon(tray_icon_image())
            .with_menu(Box::new(menu))
            .build()
            .context("build tray icon")?;

        Ok(Tray {
            icon,
            preview,
            history,
            copy_last,
            open_folder,
            quit,
        })
    }
}

struct HudApp {
    ui: Arc<Mutex<UiState>>,
    recorder: Recorder,
    engine: EngineSlot,
    /// Pill opacity, resolved from settings at startup.
    hud_alpha: u8,
    history: Arc<Mutex<History>>,
    wakeup: Arc<Mutex<Option<egui::Context>>>,
    tray: Tray,
    /// Shared with the history window so it can close itself.
    show_history: Arc<AtomicBool>,
    preview_until: Option<Instant>,
    error_until: Option<Instant>,
    last_shown: Option<bool>,
    /// True once the engine has reported ready or failed at least once.
    engine_settled: bool,
}

impl eframe::App for HudApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        *self.wakeup.lock().expect("wakeup") = Some(ctx.clone());
        self.poll_menu(ctx);
        self.sync_engine();
        self.tick_timers();
        self.sync_peaks(ctx);

        let snapshot = {
            let state = self.ui.lock().expect("ui state");
            (state.phase.clone(), state.peaks.clone(), state.partial.clone())
        };

        let show = !matches!(snapshot.0, Phase::Idle);
        if self.last_shown != Some(show) && place_hud(show) {
            self.last_shown = Some(show);
        }

        paint_hud(ctx, &snapshot.0, &snapshot.1, &snapshot.2, self.hud_alpha);

        if self.show_history.load(Ordering::SeqCst) {
            history_window::show(ctx, &self.history, &self.show_history);
        }

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
            if event.id == self.tray.quit.id() {
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else if event.id == self.tray.preview.id() {
                self.preview_until = Some(Instant::now() + Duration::from_secs(3));
                let mut state = self.ui.lock().expect("ui state");
                if matches!(state.phase, Phase::Idle | Phase::Error(_)) {
                    state.phase = Phase::Listening;
                }
            } else if event.id == self.tray.history.id() {
                self.show_history.store(true, Ordering::SeqCst);
                ctx.request_repaint();
            } else if event.id == self.tray.copy_last.id() {
                let last = self.ui.lock().expect("ui state").last.clone();
                if last.is_empty() {
                    eprintln!("noma: nothing dictated yet");
                } else {
                    ctx.copy_text(last);
                }
            } else if event.id == self.tray.open_folder.id() {
                open_settings_folder();
            }
        }
    }

    /// Mirror the engine's loading state into the HUD.
    ///
    /// Downloading Parakeet takes minutes, and a first run with no feedback
    /// looks exactly like a broken install.
    fn sync_engine(&mut self) {
        let status = self.engine.status();
        let mut state = self.ui.lock().expect("ui state");
        // Never interrupt a dictation that is already under way.
        if matches!(state.phase, Phase::Listening | Phase::Transcribing) {
            return;
        }
        match status {
            EngineStatus::Loading { message, percent } => {
                state.phase = Phase::Loading { message, percent };
            }
            EngineStatus::Ready(label) => {
                if !self.engine_settled {
                    self.engine_settled = true;
                    eprintln!("noma: {label} ready");
                    state.phase = Phase::Idle;
                }
            }
            EngineStatus::Failed(reason) => {
                if !self.engine_settled {
                    self.engine_settled = true;
                    state.phase = Phase::Error(reason);
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
                let t = ctx.input(|i| i.time);
                let live = self.recorder.peaks();
                state.peaks = (0..PEAK_BINS)
                    .map(|i| {
                        let sample = live.get(i).copied().unwrap_or(0.0).powf(0.65) * 1.7;
                        let idle = 0.08 + 0.06 * ((t * 3.2 + i as f64 * 0.45).sin().abs() as f32);
                        sample.max(idle)
                    })
                    .collect();
            }
            Phase::Transcribing => {
                let t = ctx.input(|i| i.time);
                state.peaks = (0..PEAK_BINS)
                    .map(|i| {
                        let x = i as f64 / (PEAK_BINS.max(1) as f64);
                        let traveling = ((x * std::f64::consts::TAU * 1.2) - t * 7.0).sin();
                        0.12 + 0.55 * ((traveling * 0.5 + 0.5) as f32)
                    })
                    .collect();
            }
            _ => {}
        }
    }
}

/// Ask the shell to open the folder holding `settings.toml`.
fn open_settings_folder() {
    let Ok(dir) = noma_config::config_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(windows)]
    let opened = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(not(windows))]
    let opened = std::process::Command::new("xdg-open").arg(&dir).spawn();
    if let Err(err) = opened {
        eprintln!("noma: could not open {}: {err}", dir.display());
    }
}

#[cfg(windows)]
fn place_hud(show: bool) -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };

    let Some(hwnd) = find_noma_hwnd() else {
        return false;
    };
    clear_dwm_backdrop(hwnd);

    unsafe {
        if show {
            let mut work = RECT::default();
            if SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut work as *mut RECT as *mut core::ffi::c_void),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .is_err()
            {
                return false;
            }
            let mut wr = RECT::default();
            if GetWindowRect(hwnd, &mut wr).is_err() {
                return false;
            }
            let width = (wr.right - wr.left).max(1);
            let height = (wr.bottom - wr.top).max(1);
            let x = work.left + (work.right - work.left - width) / 2;
            let y = work.bottom - height - 48;
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        } else {
            let _ = SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                -32000,
                -32000,
                0,
                0,
                SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }
    true
}

/// Stop DWM painting anything of its own behind the HUD.
///
/// `with_transparent(true)` makes winit mark the client area DWM-transparent,
/// and that is exactly the condition that lets a system backdrop paint
/// *underneath* it. Asking for acrylic here did not add glass over the desktop,
/// it replaced the desktop with a slab of material. Worse, the HUD is created
/// inactive and every `SetWindowPos` passes `SWP_NOACTIVATE`, and Windows
/// degrades acrylic on a deactivated window to a flat solid colour - so the
/// slab did not even blur.
///
/// The pill paints its own translucency, so DWM only has to stay out of the way.
#[cfg(windows)]
fn clear_dwm_backdrop(hwnd: windows::Win32::Foundation::HWND) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMSBT_NONE, DWMWA_SYSTEMBACKDROP_TYPE,
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND,
    };
    // The window is a bare rounded rectangle we draw ourselves; DWM rounding
    // would only clip pixels that are already transparent, and can leave a
    // hairline border on a borderless window.
    let preference = DWMWCP_DONOTROUND;
    let backdrop = DWMSBT_NONE;
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&preference) as u32,
        )
    };
    let _ = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_SYSTEMBACKDROP_TYPE,
            &backdrop as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&backdrop) as u32,
        )
    };
}

#[cfg(windows)]
fn find_noma_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::System::Threading::GetCurrentProcessId;
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextW, GetWindowThreadProcessId};

    struct Search {
        pid: u32,
        hwnd: HWND,
    }

    let mut search = Search {
        pid: unsafe { GetCurrentProcessId() },
        hwnd: HWND::default(),
    };

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid != search.pid {
            return true.into();
        }
        let mut buf = [0u16; 64];
        let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
        if len <= 0 {
            return true.into();
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        // Exactly "Noma": the history window must not be moved off-screen.
        if title == "Noma" {
            search.hwnd = hwnd;
            return false.into();
        }
        true.into()
    }

    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(enum_proc),
            LPARAM(&mut search as *mut Search as isize),
        )
    };
    if search.hwnd.0.is_null() {
        None
    } else {
        Some(search.hwnd)
    }
}

#[cfg(not(windows))]
fn place_hud(_show: bool) -> bool {
    true
}

fn paint_hud(ctx: &egui::Context, phase: &Phase, peaks: &[f32], partial: &str, alpha: u8) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            let painter = ui.painter();
            let radius = (rect.height() * 0.5) as u8;
            // The only thing between the text and the desktop: DWM paints no
            // backdrop behind this window, by design.
            painter.rect_filled(
                rect,
                CornerRadius::same(radius),
                Color32::from_rgba_unmultiplied(18, 24, 36, alpha),
            );
            painter.rect_stroke(
                rect,
                CornerRadius::same(radius),
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 22)),
                StrokeKind::Inside,
            );

            let (title, accent) = match phase {
                Phase::Loading { .. } => ("Setting up", Color32::from_rgb(196, 181, 253)),
                Phase::Listening => ("Listening", Color32::from_rgb(52, 211, 190)),
                Phase::Transcribing => ("Transcribing", Color32::from_rgb(125, 211, 252)),
                Phase::Error(_) => ("Error", Color32::from_rgb(248, 113, 113)),
                Phase::Idle => ("Noma", Color32::from_rgb(148, 163, 184)),
            };

            // While the key is held, the words matter more than the hint.
            let subtitle = match phase {
                Phase::Error(message) => tail(message, SUBTITLE_CHARS),
                Phase::Loading { message, .. } => tail(message, SUBTITLE_CHARS),
                Phase::Listening if !partial.is_empty() => tail(partial, SUBTITLE_CHARS),
                Phase::Listening => noma_hotkey::key_label().to_string(),
                Phase::Transcribing if !partial.is_empty() => tail(partial, SUBTITLE_CHARS),
                Phase::Transcribing => "Almost done".to_string(),
                Phase::Idle => format!("Hold {}", noma_hotkey::key_label()),
            };

            let dot = Pos2::new(rect.left() + 22.0, rect.center().y);
            painter.circle_filled(dot, 5.0, accent);
            painter.circle_filled(dot, 2.2, Color32::from_rgb(16, 20, 28));

            painter.text(
                Pos2::new(rect.left() + 36.0, rect.center().y - 11.0),
                egui::Align2::LEFT_CENTER,
                title,
                egui::FontId::proportional(15.0),
                Color32::from_rgb(248, 250, 252),
            );
            painter.text(
                Pos2::new(rect.left() + 36.0, rect.center().y + 10.0),
                egui::Align2::LEFT_CENTER,
                &subtitle,
                egui::FontId::proportional(11.0),
                Color32::from_rgb(148, 163, 184),
            );

            let right = Rect::from_min_max(
                Pos2::new(rect.left() + 158.0, rect.top() + 16.0),
                Pos2::new(rect.right() - 22.0, rect.bottom() - 16.0),
            );
            match phase {
                Phase::Loading { percent, .. } => paint_progress(painter, right, *percent, accent),
                _ => paint_waveform(painter, right, peaks, accent),
            }
        });
}

/// The end of a long line, which is where the newest words are.
fn tail(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let skip = count - max_chars + 1;
    format!("...{}", text.chars().skip(skip).collect::<String>())
}

fn paint_progress(painter: &egui::Painter, rect: Rect, percent: f32, color: Color32) {
    let height = 6.0;
    let track = Rect::from_min_max(
        Pos2::new(rect.left(), rect.center().y - height * 0.5),
        Pos2::new(rect.right(), rect.center().y + height * 0.5),
    );
    let radius = (height * 0.5) as u8;
    painter.rect_filled(
        track,
        CornerRadius::same(radius),
        Color32::from_rgba_unmultiplied(255, 255, 255, 26),
    );
    let filled = track.width() * (percent / 100.0).clamp(0.0, 1.0);
    if filled > 0.5 {
        painter.rect_filled(
            Rect::from_min_max(
                track.min,
                Pos2::new(track.left() + filled.max(height), track.max.y),
            ),
            CornerRadius::same(radius),
            color,
        );
    }
}

fn paint_waveform(painter: &egui::Painter, rect: Rect, peaks: &[f32], color: Color32) {
    if peaks.is_empty() {
        return;
    }
    let n = peaks.len() as f32;
    let gap = 3.0;
    let bar_w = ((rect.width() - gap * (n - 1.0)) / n).max(2.5);
    let mid_y = rect.center().y;
    let max_h = rect.height() * 0.5;
    let radius = (bar_w * 0.5).clamp(1.0, 4.0) as u8;

    for (i, peak) in peaks.iter().enumerate() {
        let x = rect.left() + i as f32 * (bar_w + gap);
        let h = (peak.clamp(0.08, 1.0) * max_h).max(3.0);
        let bar = Rect::from_min_max(Pos2::new(x, mid_y - h), Pos2::new(x + bar_w, mid_y + h));
        painter.rect_filled(bar, CornerRadius::same(radius), color);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_subtitles_are_left_alone() {
        assert_eq!(tail("Hold Right Ctrl", 46), "Hold Right Ctrl");
    }

    #[test]
    fn long_subtitles_keep_the_newest_words() {
        let text = "one two three four five six seven eight nine ten eleven twelve";
        let shown = tail(text, 20);
        assert_eq!(shown.chars().count(), 20 + 2);
        assert!(shown.starts_with("..."));
        assert!(text.ends_with(shown.trim_start_matches('.')));
    }

    #[test]
    fn truncation_counts_characters_not_bytes() {
        let text = "aéîöu".repeat(20);
        let shown = tail(&text, 10);
        assert_eq!(shown.chars().count(), 12);
    }
}
