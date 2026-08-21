use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::{
    self, Color32, ColorImage, Context, Id, Image, Rect, Sense, TextureHandle, TextureOptions,
    Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, WindowLevel,
};

use crate::Phase;

const MASCOT_SIZE: f32 = 157.0;
const KEY: Color32 = Color32::WHITE;
const FRAME: usize = 256 * 256 * 4;
const FPS: f64 = 12.0;

struct Clip {
    frames: Vec<TextureHandle>,
}

pub struct Mascot {
    still: Option<TextureHandle>,
    idle: Option<Clip>,
    listen: Option<Clip>,
    transcribe: Option<Clip>,
    poke_clip: Option<Clip>,
    placed: bool,
    keyed: bool,
    dragging: bool,
    grab_dx: i32,
    grab_dy: i32,
    poked_at: Option<Instant>,
}

impl Mascot {
    pub fn new() -> Self {
        Self {
            still: None,
            idle: None,
            listen: None,
            transcribe: None,
            poke_clip: None,
            placed: false,
            keyed: false,
            dragging: false,
            grab_dx: 0,
            grab_dy: 0,
            poked_at: None,
        }
    }

    pub fn show(&mut self, ctx: &Context, phase: &Phase) {
        if self.still.is_none() {
            self.still = Some(load_rgba(ctx, "noma-front", "noma-front.rgba"));
            self.idle = load_clip(ctx, "idle");
            self.listen = load_clip(ctx, "listen");
            self.transcribe = load_clip(ctx, "transcribe");
            self.poke_clip = load_clip(ctx, "poke");
        }

        if let Some(started) = self.poked_at {
            if let Some(clip) = &self.poke_clip {
                let frame = (started.elapsed().as_secs_f64() * FPS) as usize;
                if frame >= clip.frames.len() {
                    self.poked_at = None;
                }
            } else if started.elapsed().as_millis() > 700 {
                self.poked_at = None;
            }
        }

        let poked = self.poked_at.is_some();
        let mut poke = false;
        let mut drag_started = false;
        let mut dragging = false;
        let mut drag_stopped = false;
        let tex = pick_frame(
            phase,
            self.poked_at,
            ctx.input(|i| i.time),
            self.still.as_ref(),
            self.idle.as_ref(),
            self.listen.as_ref(),
            self.transcribe.as_ref(),
            self.poke_clip.as_ref(),
        );
        let has_clip = match phase {
            Phase::Listening => self.listen.is_some(),
            Phase::Transcribing => self.transcribe.is_some(),
            _ if poked => self.poke_clip.is_some(),
            _ => self.idle.is_some(),
        };

        ctx.show_viewport_immediate(
            ViewportId::from_hash_of("noma-mascot"),
            ViewportBuilder::default()
                .with_title("Noma Mascot")
                .with_inner_size([MASCOT_SIZE, MASCOT_SIZE])
                .with_decorations(false)
                .with_transparent(true)
                .with_always_on_top()
                .with_taskbar(false)
                .with_resizable(false)
                .with_active(false),
            |ctx, class| {
                if class == ViewportClass::Embedded {
                    return;
                }
                ctx.send_viewport_cmd(ViewportCommand::WindowLevel(WindowLevel::AlwaysOnTop));

                let t = ctx.input(|i| i.time);
                let (offset, scale) = if has_clip {
                    (Vec2::ZERO, 1.0)
                } else {
                    motion(phase, poked, t)
                };

                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE.fill(KEY))
                    .show(ctx, |ui| {
                        ui.painter().rect_filled(ui.max_rect(), 0.0, KEY);
                        let response = ui.interact(
                            ui.max_rect(),
                            Id::new("noma-mascot-hit"),
                            Sense::click_and_drag(),
                        );
                        drag_started = response.drag_started();
                        dragging = response.dragged();
                        drag_stopped = response.drag_stopped();
                        if response.clicked() && !response.dragged() {
                            poke = true;
                        }
                        response.on_hover_cursor(egui::CursorIcon::Grab);

                        if let Some(tex) = tex {
                            let size = Vec2::splat(MASCOT_SIZE * scale);
                            let rect =
                                Rect::from_center_size(ui.max_rect().center() + offset, size);
                            Image::new((tex.id(), size)).paint_at(ui, rect);
                        }
                    });
                ctx.request_repaint();
            },
        );

        if !self.keyed {
            self.keyed = apply_color_key();
        }
        if !self.placed {
            self.placed = place_mascot_once();
        }

        if drag_started {
            if let Some((dx, dy)) = grab_offset() {
                self.dragging = true;
                self.grab_dx = dx;
                self.grab_dy = dy;
            }
        }
        if dragging && self.dragging {
            follow_cursor(self.grab_dx, self.grab_dy);
        }
        if drag_stopped {
            self.dragging = false;
        }
        if poke {
            self.poked_at = Some(Instant::now());
        }
    }
}

fn pick_frame<'a>(
    phase: &Phase,
    poked_at: Option<Instant>,
    t: f64,
    still: Option<&'a TextureHandle>,
    idle: Option<&'a Clip>,
    listen: Option<&'a Clip>,
    transcribe: Option<&'a Clip>,
    poke: Option<&'a Clip>,
) -> Option<&'a TextureHandle> {
    if let Some(started) = poked_at {
        if let Some(clip) = poke {
            let i = (started.elapsed().as_secs_f64() * FPS) as usize;
            return clip.frames.get(i.min(clip.frames.len().saturating_sub(1)));
        }
    }
    let clip = match phase {
        Phase::Listening => listen,
        Phase::Transcribing => transcribe,
        _ => idle,
    };
    if let Some(clip) = clip {
        if !clip.frames.is_empty() {
            let i = ((t * FPS) as usize) % clip.frames.len();
            return Some(&clip.frames[i]);
        }
    }
    still
}

fn motion(phase: &Phase, poked: bool, t: f64) -> (Vec2, f32) {
    if poked {
        let squash = 0.86 + 0.08 * ((t * 18.0).sin().abs() as f32);
        return (Vec2::new(((t * 28.0).sin() as f32) * 3.0, 6.0), squash);
    }
    match phase {
        Phase::Listening => (
            Vec2::new(0.0, ((t * 9.0).sin() as f32) * 5.0),
            1.03 + 0.02 * ((t * 8.0).sin() as f32),
        ),
        Phase::Transcribing => (
            Vec2::new(
                ((t * 3.0).sin() as f32) * 2.0,
                ((t * 5.5).sin() as f32) * 3.0,
            ),
            1.0 + 0.03 * ((t * 6.0).sin() as f32).abs(),
        ),
        Phase::Error(_) => (Vec2::new(((t * 16.0).sin() as f32) * 2.0, 0.0), 0.98),
        Phase::Idle => (Vec2::new(0.0, ((t * 2.2).sin() as f32) * 4.0), 1.0),
    }
}

fn load_rgba(ctx: &Context, name: &str, file: &str) -> TextureHandle {
    let path = asset_path(file);
    let mut bytes = std::fs::read(&path).unwrap_or_default();
    composite_on_white(&mut bytes);
    let color = ColorImage::from_rgba_unmultiplied([256, 256], &bytes);
    ctx.load_texture(name, color, TextureOptions::LINEAR)
}

fn load_clip(ctx: &Context, name: &str) -> Option<Clip> {
    let path = asset_path(&format!("{name}.rgba"));
    let mut bytes = std::fs::read(&path).ok()?;
    if bytes.len() < FRAME {
        return None;
    }
    composite_on_white(&mut bytes);
    let count = bytes.len() / FRAME;
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        let slice = &bytes[i * FRAME..(i + 1) * FRAME];
        let color = ColorImage::from_rgba_unmultiplied([256, 256], slice);
        frames.push(ctx.load_texture(format!("{name}-{i}"), color, TextureOptions::LINEAR));
    }
    Some(Clip { frames })
}

fn asset_path(file: &str) -> PathBuf {
    [
        PathBuf::from("assets/mascot").join(file),
        PathBuf::from("crates/noma-hud/../../assets/mascot").join(file),
    ]
    .into_iter()
    .find(|path| path.exists())
    .unwrap_or_else(|| PathBuf::from("assets/mascot").join(file))
}

fn composite_on_white(bytes: &mut [u8]) {
    for px in bytes.chunks_exact_mut(4) {
        let a = px[3] as u16;
        if a == 0 {
            px[0] = 255;
            px[1] = 255;
            px[2] = 255;
            px[3] = 255;
            continue;
        }
        if a < 255 {
            px[0] = ((px[0] as u16 * a + 255 * (255 - a)) / 255) as u8;
            px[1] = ((px[1] as u16 * a + 255 * (255 - a)) / 255) as u8;
            px[2] = ((px[2] as u16 * a + 255 * (255 - a)) / 255) as u8;
            px[3] = 255;
        }
    }
}

#[cfg(windows)]
fn mascot_hwnd() -> Option<windows::Win32::Foundation::HWND> {
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;
    let hwnd = unsafe { FindWindowW(None, w!("Noma Mascot")) }.unwrap_or_default();
    if hwnd == HWND::default() {
        None
    } else {
        Some(hwnd)
    }
}

#[cfg(windows)]
fn apply_color_key() -> bool {
    use windows::Win32::Foundation::COLORREF;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, SetLayeredWindowAttributes, SetWindowLongPtrW, GWL_EXSTYLE,
        LWA_COLORKEY, WS_EX_LAYERED,
    };
    let Some(hwnd) = mascot_hwnd() else {
        return false;
    };
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0x00FFFFFF), 0, LWA_COLORKEY);
    }
    true
}

#[cfg(windows)]
fn grab_offset() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetWindowRect};
    let hwnd = mascot_hwnd()?;
    let mut cursor = POINT::default();
    let mut wr = RECT::default();
    unsafe {
        GetCursorPos(&mut cursor).ok()?;
        GetWindowRect(hwnd, &mut wr).ok()?;
    }
    Some((wr.left - cursor.x, wr.top - cursor.y))
}

#[cfg(windows)]
fn follow_cursor(dx: i32, dy: i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    let Some(hwnd) = mascot_hwnd() else {
        return;
    };
    let mut cursor = POINT::default();
    unsafe {
        if GetCursorPos(&mut cursor).is_err() {
            return;
        }
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            cursor.x + dx,
            cursor.y + dy,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
}

#[cfg(windows)]
fn place_mascot_once() -> bool {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetWindowRect, SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let Some(hwnd) = mascot_hwnd() else {
        return false;
    };
    apply_color_key();
    unsafe {
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
        let _ = GetWindowRect(hwnd, &mut wr);
        let width = (wr.right - wr.left).max(MASCOT_SIZE as i32);
        let height = (wr.bottom - wr.top).max(MASCOT_SIZE as i32);
        let x = work.right - width - 24;
        let y = work.bottom - height - 24;
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
        );
    }
    true
}

#[cfg(not(windows))]
fn apply_color_key() -> bool {
    true
}

#[cfg(not(windows))]
fn grab_offset() -> Option<(i32, i32)> {
    None
}

#[cfg(not(windows))]
fn follow_cursor(_dx: i32, _dy: i32) {}

#[cfg(not(windows))]
fn place_mascot_once() -> bool {
    true
}
