use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::{
    self, Color32, ColorImage, Context, Id, Image, Rect, Sense, TextureHandle, TextureOptions,
    Vec2, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, WindowLevel,
};

use crate::Phase;

const MASCOT_SIZE: f32 = 157.0;
const KEY: Color32 = Color32::from_rgb(255, 0, 255);

pub struct Mascot {
    texture: Option<TextureHandle>,
    placed: bool,
    keyed: bool,
    dragging: bool,
    grab_dx: i32,
    grab_dy: i32,
    poked_until: Option<Instant>,
}

impl Mascot {
    pub fn new() -> Self {
        Self {
            texture: None,
            placed: false,
            keyed: false,
            dragging: false,
            grab_dx: 0,
            grab_dy: 0,
            poked_until: None,
        }
    }

    pub fn show(&mut self, ctx: &Context, phase: &Phase) {
        if self.texture.is_none() {
            self.texture = Some(load_still(ctx));
        }

        let texture = self.texture.clone();
        let poked = self
            .poked_until
            .map(|until| Instant::now() < until)
            .unwrap_or(false);
        let mut poke = false;
        let mut drag_started = false;
        let mut dragging = false;
        let mut drag_stopped = false;

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
                let (offset, scale) = motion(phase, poked, t);

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

                        if let Some(tex) = &texture {
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
            self.poked_until = Some(Instant::now() + std::time::Duration::from_millis(700));
        }
    }
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

fn load_still(ctx: &Context) -> TextureHandle {
    const WIDTH: usize = 256;
    const HEIGHT: usize = 256;
    let path = [
        PathBuf::from("assets/mascot/noma-front.rgba"),
        PathBuf::from("crates/noma-hud/../../assets/mascot/noma-front.rgba"),
    ]
    .into_iter()
    .find(|path| path.exists())
    .expect("noma-front.rgba");
    let bytes = std::fs::read(&path).expect("read noma sprite");
    let color = ColorImage::from_rgba_unmultiplied([WIDTH, HEIGHT], &bytes);
    ctx.load_texture("noma-front", color, TextureOptions::LINEAR)
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
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0x00FF00FF), 0, LWA_COLORKEY);
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
