use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::time::Instant;

use eframe::egui::Context;

use crate::Phase;

const MASCOT_SIZE: i32 = 157;
const SRC: usize = 256;
const FRAME: usize = SRC * SRC * 4;
const FPS: f64 = 8.0;

static HWND_BITS: AtomicIsize = AtomicIsize::new(0);
static DRAGGING: AtomicBool = AtomicBool::new(false);
static DRAG_DX: AtomicI32 = AtomicI32::new(0);
static DRAG_DY: AtomicI32 = AtomicI32::new(0);
static MOVED: AtomicBool = AtomicBool::new(false);
static POKE: AtomicBool = AtomicBool::new(false);

struct Clip {
    frames: Vec<Vec<u8>>,
}

pub struct Mascot {
    still: Vec<u8>,
    idle: Option<Clip>,
    listen: Option<Clip>,
    transcribe: Option<Clip>,
    poke_clip: Option<Clip>,
    layer: Option<Layer>,
    poked_at: Option<Instant>,
    started: Instant,
}

struct Layer {
    hwnd: isize,
}

impl Mascot {
    pub fn new() -> Self {
        Self {
            still: load_frame_file("noma-front.rgba").unwrap_or_else(|| vec![0; FRAME]),
            idle: load_clip_file("idle.rgba"),
            listen: load_clip_file("listen.rgba"),
            transcribe: load_clip_file("transcribe.rgba"),
            poke_clip: load_clip_file("poke.rgba"),
            layer: None,
            poked_at: None,
            started: Instant::now(),
        }
    }

    pub fn show(&mut self, _ctx: &Context, phase: &Phase) {
        if self.layer.is_none() {
            if let Some(hwnd) = create_layer() {
                self.layer = Some(Layer { hwnd });
                place_bottom_right(hwnd);
            }
        }
        let Some(layer) = self.layer.as_ref() else {
            return;
        };

        poll_mouse();

        if POKE.swap(false, Ordering::SeqCst) {
            self.poked_at = Some(Instant::now());
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

        let t = self.started.elapsed().as_secs_f64();
        let pixels = pick_pixels(
            phase,
            self.poked_at,
            t,
            &self.still,
            self.idle.as_ref(),
            self.listen.as_ref(),
            self.transcribe.as_ref(),
            self.poke_clip.as_ref(),
        );
        blit(layer.hwnd, pixels);
    }
}

fn pick_pixels<'a>(
    phase: &Phase,
    poked_at: Option<Instant>,
    t: f64,
    still: &'a [u8],
    idle: Option<&'a Clip>,
    listen: Option<&'a Clip>,
    transcribe: Option<&'a Clip>,
    poke: Option<&'a Clip>,
) -> &'a [u8] {
    if let Some(started) = poked_at {
        if let Some(clip) = poke {
            let i = (started.elapsed().as_secs_f64() * FPS) as usize;
            let i = i.min(clip.frames.len().saturating_sub(1));
            return &clip.frames[i];
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
            return &clip.frames[i];
        }
    }
    still
}

fn load_clip_file(file: &str) -> Option<Clip> {
    let mut bytes = std::fs::read(asset_path(file)).ok()?;
    if bytes.len() < FRAME {
        return None;
    }
    punch_green(&mut bytes);
    let count = bytes.len() / FRAME;
    let mut frames = Vec::with_capacity(count);
    for i in 0..count {
        frames.push(bytes[i * FRAME..(i + 1) * FRAME].to_vec());
    }
    Some(Clip { frames })
}

fn load_frame_file(file: &str) -> Option<Vec<u8>> {
    let mut bytes = std::fs::read(asset_path(file)).ok()?;
    if bytes.len() < FRAME {
        return None;
    }
    bytes.truncate(FRAME);
    punch_green(&mut bytes);
    Some(bytes)
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

/// Punch leftover chroma green. Noma is purple/cyan, so green is safe.
fn punch_green(bytes: &mut [u8]) {
    for px in bytes.chunks_exact_mut(4) {
        let r = px[0];
        let g = px[1];
        let b = px[2];
        if g > 160 && g > r.saturating_add(40) && g > b.saturating_add(40) {
            px[3] = 0;
        }
    }
}

#[cfg(windows)]
fn poll_mouse() {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW,
    };
    if !DRAGGING.load(Ordering::SeqCst) {
        return;
    }
    let down = unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 } & 0x8000 != 0;
    if down {
        let hwnd = HWND_BITS.load(Ordering::SeqCst);
        if hwnd == 0 {
            return;
        }
        let mut cursor = POINT::default();
        unsafe {
            if GetCursorPos(&mut cursor).is_ok() {
                let _ = SetWindowPos(
                    windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void),
                    Some(HWND_TOPMOST),
                    cursor.x + DRAG_DX.load(Ordering::SeqCst),
                    cursor.y + DRAG_DY.load(Ordering::SeqCst),
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                MOVED.store(true, Ordering::SeqCst);
            }
        }
    } else {
        DRAGGING.store(false, Ordering::SeqCst);
        if !MOVED.swap(false, Ordering::SeqCst) {
            POKE.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(not(windows))]
fn poll_mouse() {}

#[cfg(windows)]
fn create_layer() -> Option<isize> {
    use windows::core::w;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, RegisterClassW, ShowWindow, CS_DBLCLKS, SW_SHOWNOACTIVATE, WNDCLASSW,
        WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
    };

    static REGISTERED: AtomicBool = AtomicBool::new(false);
    let class = w!("NomaMascotWnd");
    unsafe {
        if !REGISTERED.swap(true, Ordering::SeqCst) {
            let hinstance = GetModuleHandleW(None).ok()?;
            let wc = WNDCLASSW {
                style: CS_DBLCLKS,
                lpfnWndProc: Some(wndproc),
                hInstance: hinstance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassW(&wc);
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            class,
            w!("Noma Mascot"),
            WS_POPUP,
            0,
            0,
            MASCOT_SIZE,
            MASCOT_SIZE,
            None,
            None,
            None,
            None,
        )
        .ok()?;
        if hwnd == HWND::default() {
            return None;
        }
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        HWND_BITS.store(hwnd.0 as isize, Ordering::SeqCst);
        Some(hwnd.0 as isize)
    }
}

#[cfg(windows)]
unsafe extern "system" fn wndproc(
    hwnd: windows::Win32::Foundation::HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::Foundation::{LRESULT, POINT};
    use windows::Win32::UI::WindowsAndMessaging::{
        DefWindowProcW, GetCursorPos, GetWindowRect, SetWindowPos, HWND_TOPMOST, MA_NOACTIVATE,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_SHOWWINDOW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
        WM_MOUSEMOVE,
    };

    match msg {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_LBUTTONDOWN => unsafe {
            DRAGGING.store(true, Ordering::SeqCst);
            MOVED.store(false, Ordering::SeqCst);
            let mut cursor = POINT::default();
            let mut wr = RECT::default();
            let _ = GetCursorPos(&mut cursor);
            let _ = GetWindowRect(hwnd, &mut wr);
            DRAG_DX.store(wr.left - cursor.x, Ordering::SeqCst);
            DRAG_DY.store(wr.top - cursor.y, Ordering::SeqCst);
            LRESULT(0)
        },
        WM_MOUSEMOVE => {
            if DRAGGING.load(Ordering::SeqCst) {
                let mut cursor = POINT::default();
                unsafe {
                    if GetCursorPos(&mut cursor).is_ok() {
                        let x = cursor.x + DRAG_DX.load(Ordering::SeqCst);
                        let y = cursor.y + DRAG_DY.load(Ordering::SeqCst);
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            x,
                            y,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
                        );
                        MOVED.store(true, Ordering::SeqCst);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(windows)]
fn blit(hwnd_bits: isize, rgba: &[u8]) {
    use windows::Win32::Foundation::{HWND, POINT, SIZE};
    use windows::Win32::Graphics::Gdi::BLENDFUNCTION;
    use windows::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, AC_SRC_ALPHA,
        AC_SRC_OVER, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

    if rgba.len() < FRAME {
        return;
    }
    let hwnd = HWND(hwnd_bits as *mut core::ffi::c_void);
    let w = MASCOT_SIZE;
    let h = MASCOT_SIZE;
    unsafe {
        let hdc_screen = CreateCompatibleDC(None);
        if hdc_screen.0.is_null() {
            return;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(hdc_screen), &info, DIB_RGB_COLORS, &mut bits, None, 0);
        if dib.is_err() || bits.is_null() {
            let _ = DeleteDC(hdc_screen);
            return;
        }
        let dib = dib.unwrap();
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, (w * h * 4) as usize);
        scale_premultiply(rgba, dst, w as usize, h as usize);
        let old = SelectObject(hdc_screen, dib.into());
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let size = SIZE { cx: w, cy: h };
        let src = POINT { x: 0, y: 0 };
        let _ = UpdateLayeredWindow(
            hwnd,
            None,
            None,
            Some(&size),
            Some(hdc_screen),
            Some(&src),
            Default::default(),
            Some(&blend),
            ULW_ALPHA,
        );
        SelectObject(hdc_screen, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(hdc_screen);
    }
}

fn scale_premultiply(src: &[u8], dst: &mut [u8], w: usize, h: usize) {
    for y in 0..h {
        for x in 0..w {
            let sx = x * SRC / w;
            let sy = y * SRC / h;
            let si = (sy * SRC + sx) * 4;
            let di = (y * w + x) * 4;
            let r = src[si] as u16;
            let g = src[si + 1] as u16;
            let b = src[si + 2] as u16;
            let a = src[si + 3] as u16;
            dst[di] = (b * a / 255) as u8;
            dst[di + 1] = (g * a / 255) as u8;
            dst[di + 2] = (r * a / 255) as u8;
            dst[di + 3] = a as u8;
        }
    }
}

#[cfg(windows)]
fn place_bottom_right(hwnd_bits: isize) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SystemParametersInfoW, HWND_TOPMOST, SPI_GETWORKAREA, SWP_NOACTIVATE,
        SWP_NOSIZE, SWP_SHOWWINDOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    };
    let hwnd = HWND(hwnd_bits as *mut core::ffi::c_void);
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
            return;
        }
        let x = work.right - MASCOT_SIZE - 24;
        let y = work.bottom - MASCOT_SIZE - 24;
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
}

#[cfg(not(windows))]
fn create_layer() -> Option<isize> {
    None
}

#[cfg(not(windows))]
fn blit(_hwnd: isize, _rgba: &[u8]) {}

#[cfg(not(windows))]
fn place_bottom_right(_hwnd: isize) {}
