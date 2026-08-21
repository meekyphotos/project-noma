use std::path::PathBuf;
use std::time::Instant;

use eframe::egui::{
    self, ColorImage, Context, Id, Image, Rect, Sense, TextureHandle, TextureOptions, Vec2,
    ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, WindowLevel,
};

use crate::Phase;

const MASCOT_SIZE: f32 = 196.0;

pub struct Mascot {
    texture: Option<TextureHandle>,
    placed: bool,
    poked_until: Option<Instant>,
}

impl Mascot {
    pub fn new() -> Self {
        Self {
            texture: None,
            placed: false,
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
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| {
                        let response = ui.interact(
                            ui.max_rect(),
                            Id::new("noma-mascot-hit"),
                            Sense::click_and_drag(),
                        );
                        if response.dragged() {
                            if let Some(outer) = ctx.input(|i| i.viewport().outer_rect) {
                                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(
                                    outer.min + response.drag_delta(),
                                ));
                            }
                        }
                        if response.clicked() {
                            poke = true;
                        }
                        response.on_hover_cursor(egui::CursorIcon::PointingHand);

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

        if poke {
            self.poked_until = Some(Instant::now() + std::time::Duration::from_millis(700));
        }
        if !self.placed && place_mascot_once() {
            self.placed = true;
        }
    }
}

fn motion(phase: &Phase, poked: bool, t: f64) -> (Vec2, f32) {
    if poked {
        let squash = 0.86 + 0.08 * ((t * 18.0).sin().abs() as f32);
        return (Vec2::new(((t * 28.0).sin() as f32) * 4.0, 8.0), squash);
    }
    match phase {
        Phase::Listening => (
            Vec2::new(0.0, ((t * 9.0).sin() as f32) * 7.0),
            1.04 + 0.03 * ((t * 8.0).sin() as f32),
        ),
        Phase::Transcribing => (
            Vec2::new(
                ((t * 3.0).sin() as f32) * 3.0,
                ((t * 5.5).sin() as f32) * 4.0,
            ),
            1.0 + 0.04 * ((t * 6.0).sin() as f32).abs(),
        ),
        Phase::Error(_) => (Vec2::new(((t * 16.0).sin() as f32) * 3.0, 0.0), 0.98),
        Phase::Idle => (Vec2::new(0.0, ((t * 2.2).sin() as f32) * 5.0), 1.0),
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
fn place_mascot_once() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetWindowRect, SetWindowPos, SystemParametersInfoW, HWND_TOPMOST,
        SPI_GETWORKAREA, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let hwnd = unsafe { FindWindowW(None, w!("Noma Mascot")) }.unwrap_or_default();
    if hwnd == HWND::default() {
        return false;
    }
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
fn place_mascot_once() -> bool {
    true
}
