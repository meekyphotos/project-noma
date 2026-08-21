use std::sync::mpsc::Receiver;

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

#[cfg(windows)]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::OnceLock;

    use anyhow::{anyhow, Context, Result};
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::VK_RCONTROL;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    use super::PttEvent;

    static TX: OnceLock<Sender<PttEvent>> = OnceLock::new();
    static DOWN: AtomicBool = AtomicBool::new(false);
    static HOOK: OnceLock<usize> = OnceLock::new();

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
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0)
                .context("SetWindowsHookExW WH_KEYBOARD_LL")?;
            HOOK.set(hook.0 as usize)
                .map_err(|_| anyhow!("hook already installed"))?;
            // Keep the hook installed for process lifetime.
            let _ = std::mem::forget(UnhookGuard(hook));
        }
        Ok(())
    }

    struct UnhookGuard(HHOOK);

    impl Drop for UnhookGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = UnhookWindowsHookEx(self.0);
            }
        }
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && lparam.0 != 0 {
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            if kb.vkCode == u32::from(VK_RCONTROL.0) {
                let msg = wparam.0 as u32;
                let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
                let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
                if is_down && !DOWN.swap(true, Ordering::SeqCst) {
                    if let Some(tx) = TX.get() {
                        let _ = tx.send(PttEvent::Pressed);
                    }
                } else if is_up && DOWN.swap(false, Ordering::SeqCst) {
                    if let Some(tx) = TX.get() {
                        let _ = tx.send(PttEvent::Released);
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

    use anyhow::{bail, Result};

    use super::PttEvent;

    pub fn spawn() -> Result<Receiver<PttEvent>> {
        bail!("Noma's hold-to-talk listener currently supports Windows only");
    }
}
