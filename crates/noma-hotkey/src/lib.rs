use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttEvent {
    Pressed,
    Released,
}

/// Default hold-to-talk key: Right Ctrl.
pub const PTT_KEY_NAME: &str = "Right Ctrl";

/// Start a background listener that emits press/release for the PTT key.
pub fn spawn() -> Result<Receiver<PttEvent>> {
    platform::spawn()
}

/// Block until Right Ctrl is physically up, then settle before paste.
pub fn wait_until_released(timeout: Duration) {
    platform::wait_until_released(timeout);
}

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::OnceLock;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, Context, Result};
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_CONTROL, VK_LCONTROL, VK_RCONTROL,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    use super::PttEvent;

    const LLKHF_EXTENDED: u32 = 0x01;
    const LLKHF_INJECTED: u32 = 0x10;

    static TX: OnceLock<Sender<PttEvent>> = OnceLock::new();
    static DOWN: AtomicBool = AtomicBool::new(false);

    pub fn spawn() -> Result<Receiver<PttEvent>> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("noma-hotkey".into())
            .spawn(move || {
                if let Err(err) = install(tx) {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
                let _ = ready_tx.send(Ok(()));
                unsafe {
                    let mut msg = MSG::default();
                    while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            })
            .context("spawn hotkey thread")?;

        ready_rx
            .recv()
            .context("hotkey thread exited before ready")??;
        Ok(rx)
    }

    fn install(tx: Sender<PttEvent>) -> Result<()> {
        TX.set(tx)
            .map_err(|_| anyhow!("PTT listener already started"))?;
        unsafe {
            let module = GetModuleHandleW(None)
                .ok()
                .map(|module| HINSTANCE(module.0));
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), module, 0)
                .context("SetWindowsHookExW WH_KEYBOARD_LL")?;
        }
        Ok(())
    }

    pub fn wait_until_released(timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !ctrl_down() {
                break;
            }
            thread::sleep(Duration::from_millis(8));
        }
        thread::sleep(Duration::from_millis(25));
    }

    fn ctrl_down() -> bool {
        unsafe {
            [VK_CONTROL, VK_LCONTROL, VK_RCONTROL]
                .into_iter()
                .any(|key| GetAsyncKeyState(key.0 as i32) as u16 & 0x8000 != 0)
        }
    }

    fn emit(tx: &Sender<PttEvent>, down: bool) {
        if down {
            if !DOWN.swap(true, Ordering::SeqCst) {
                eprintln!("noma: PTT pressed");
                let _ = tx.send(PttEvent::Pressed);
            }
        } else if DOWN.swap(false, Ordering::SeqCst) {
            eprintln!("noma: PTT released");
            let _ = tx.send(PttEvent::Released);
        }
    }

    fn is_right_ctrl(kb: &KBDLLHOOKSTRUCT) -> bool {
        kb.vkCode == u32::from(VK_RCONTROL.0)
            || (kb.vkCode == u32::from(VK_CONTROL.0) && (kb.flags.0 & LLKHF_EXTENDED) != 0)
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && lparam.0 != 0 {
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if (kb.flags.0 & LLKHF_INJECTED) != 0 {
                return unsafe { CallNextHookEx(None, code, wparam, lparam) };
            }
            if is_right_ctrl(kb) {
                let msg = wparam.0 as u32;
                let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                if let Some(tx) = TX.get() {
                    if is_down {
                        emit(tx, true);
                    } else if is_up {
                        emit(tx, false);
                    }
                }
                return LRESULT(1);
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

#[cfg(not(windows))]
mod platform {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use anyhow::{bail, Result};

    use super::PttEvent;

    pub fn spawn() -> Result<Receiver<PttEvent>> {
        bail!("Noma's hold-to-talk listener currently supports Windows only");
    }

    pub fn wait_until_released(timeout: Duration) {
        std::thread::sleep(timeout.min(Duration::from_millis(25)));
    }
}
