use std::sync::mpsc::Receiver;
use std::time::Duration;

use anyhow::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PttEvent {
    Pressed,
    Released,
}

/// A key that can be held to talk.
///
/// Only keys that are useless as modifiers make good candidates: Noma swallows
/// the key while it runs, so binding, say, left Ctrl would break every shortcut
/// on the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PttKey {
    /// What goes in `settings.toml`.
    pub id: &'static str,
    /// What the HUD shows.
    pub label: &'static str,
    /// Windows virtual key code for the specific left/right key.
    vk: u16,
    /// The generic code Windows sometimes reports instead, paired with the
    /// extended-key flag. Right Ctrl arrives as VK_CONTROL + extended on some
    /// keyboards, for instance.
    generic_vk: Option<u16>,
}

/// Every key Noma will bind to, in the order a settings menu should list them.
pub const KEYS: &[PttKey] = &[
    PttKey {
        id: "right-ctrl",
        label: "Right Ctrl",
        vk: 0xA3, // VK_RCONTROL
        generic_vk: Some(0x11), // VK_CONTROL
    },
    PttKey {
        id: "right-alt",
        label: "Right Alt",
        vk: 0xA5, // VK_RMENU
        generic_vk: Some(0x12), // VK_MENU
    },
    PttKey {
        id: "right-shift",
        label: "Right Shift",
        vk: 0xA1, // VK_RSHIFT
        generic_vk: None,
    },
    PttKey {
        id: "caps-lock",
        label: "Caps Lock",
        vk: 0x14, // VK_CAPITAL
        generic_vk: None,
    },
    PttKey {
        id: "f13",
        label: "F13",
        vk: 0x7C,
        generic_vk: None,
    },
];

/// The key Noma binds when settings say nothing useful.
///
/// Right Ctrl - the default everywhere else - does not exist as a physical
/// key on most Mac keyboards (the bottom row is fn/control/option/command,
/// with no right-hand control key), so it would silently never fire. Right
/// Alt/Option is on virtually every Mac keyboard and keeps the same
/// hold-to-talk behavior; Caps Lock is more universal still but toggles
/// instead of holding on macOS (see `platform::modifier_flag`), which is a
/// worse first impression for a "hold to talk" app.
#[cfg(target_os = "macos")]
pub const DEFAULT_KEY: PttKey = KEYS[1];
#[cfg(not(target_os = "macos"))]
pub const DEFAULT_KEY: PttKey = KEYS[0];

/// Look up a key by its settings id.
pub fn key_from_id(id: &str) -> Option<PttKey> {
    KEYS.iter().copied().find(|key| key.id == id)
}

/// Look up a key by id, falling back to Right Ctrl with a warning.
pub fn key_or_default(id: &str) -> PttKey {
    key_from_id(id).unwrap_or_else(|| {
        eprintln!("noma: unknown hotkey {id:?}, using {}", DEFAULT_KEY.label);
        DEFAULT_KEY
    })
}

/// The label of the key currently bound, for the HUD.
pub fn key_label() -> &'static str {
    platform::active_key().label
}

/// Start a background listener that emits press/release for `key`.
pub fn spawn(key: PttKey) -> Result<Receiver<PttEvent>> {
    platform::spawn(key)
}

/// Block until the talk key is physically up, then settle before pasting.
///
/// Pasting while the key is still down would send Ctrl+V with Ctrl already
/// held by the user, which some apps read as a different shortcut entirely.
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
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    use super::{PttEvent, PttKey, DEFAULT_KEY};

    const LLKHF_EXTENDED: u32 = 0x01;
    const LLKHF_INJECTED: u32 = 0x10;

    static TX: OnceLock<Sender<PttEvent>> = OnceLock::new();
    static KEY: OnceLock<PttKey> = OnceLock::new();
    static DOWN: AtomicBool = AtomicBool::new(false);

    pub fn active_key() -> PttKey {
        KEY.get().copied().unwrap_or(DEFAULT_KEY)
    }

    pub fn spawn(key: PttKey) -> Result<Receiver<PttEvent>> {
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        std::thread::Builder::new()
            .name("noma-hotkey".into())
            .spawn(move || {
                if let Err(err) = install(tx, key) {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
                let _ = ready_tx.send(Ok(()));
                // The hook only fires on the thread that owns a message loop.
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

    fn install(tx: Sender<PttEvent>, key: PttKey) -> Result<()> {
        KEY.set(key).map_err(|_| anyhow!("PTT key already bound"))?;
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
            if !key_is_down() {
                break;
            }
            thread::sleep(Duration::from_millis(8));
        }
        thread::sleep(Duration::from_millis(25));
    }

    /// True while the bound key, or the modifier it belongs to, is held.
    fn key_is_down() -> bool {
        let key = active_key();
        let mut codes = vec![key.vk];
        codes.extend(key.generic_vk);
        unsafe {
            codes
                .into_iter()
                .any(|code| GetAsyncKeyState(i32::from(code)) as u16 & 0x8000 != 0)
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

    fn is_ptt(kb: &KBDLLHOOKSTRUCT) -> bool {
        let key = active_key();
        if kb.vkCode == u32::from(key.vk) {
            return true;
        }
        // A right-hand modifier can arrive as its generic code with the
        // extended flag set, depending on the keyboard driver.
        key.generic_vk.is_some_and(|generic| {
            kb.vkCode == u32::from(generic) && (kb.flags.0 & LLKHF_EXTENDED) != 0
        })
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code >= 0 && lparam.0 != 0 {
            let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            // Our own Ctrl+V for pasting comes back through this hook.
            if (kb.flags.0 & LLKHF_INJECTED) != 0 {
                return unsafe { CallNextHookEx(None, code, wparam, lparam) };
            }
            if is_ptt(kb) {
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
                // Swallow it so the talk key does nothing else while Noma runs.
                return LRESULT(1);
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::OnceLock;
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{anyhow, Context, Result};
    use core_foundation::runloop::{kCFRunLoopCommonModes, CFRunLoop};
    use core_graphics::event::{
        CGEventFlags, CGEventTap, CGEventTapLocation, CGEventTapOptions, CGEventTapPlacement,
        CGEventType, CallbackResult, EventField,
    };

    use super::{PttEvent, PttKey, DEFAULT_KEY};

    // Carbon/HIToolbox virtual keycodes, matched against
    // `EventField::KEYBOARD_EVENT_KEYCODE`. Right Ctrl/Alt/Shift and Caps Lock
    // are modifier-only keys: physically pressing or releasing one never
    // produces KeyDown/KeyUp, only FlagsChanged with the matching bit toggled.
    // F13 is a plain key and arrives as ordinary KeyDown/KeyUp.
    const KC_RIGHT_CONTROL: i64 = 0x3E;
    const KC_RIGHT_OPTION: i64 = 0x3D;
    const KC_RIGHT_SHIFT: i64 = 0x3C;
    const KC_CAPS_LOCK: i64 = 0x39;
    const KC_F13: i64 = 0x69;

    fn macos_keycode(key: PttKey) -> i64 {
        match key.id {
            "right-ctrl" => KC_RIGHT_CONTROL,
            "right-alt" => KC_RIGHT_OPTION,
            "right-shift" => KC_RIGHT_SHIFT,
            "caps-lock" => KC_CAPS_LOCK,
            "f13" => KC_F13,
            other => unreachable!("every PttKey needs a macOS keycode, got {other:?}"),
        }
    }

    /// The device-independent flag a modifier key toggles, or `None` for a
    /// plain key that reports through ordinary KeyDown/KeyUp instead.
    fn modifier_flag(key: PttKey) -> Option<CGEventFlags> {
        match key.id {
            "right-ctrl" => Some(CGEventFlags::CGEventFlagControl),
            "right-alt" => Some(CGEventFlags::CGEventFlagAlternate),
            "right-shift" => Some(CGEventFlags::CGEventFlagShift),
            "caps-lock" => Some(CGEventFlags::CGEventFlagAlphaShift),
            _ => None,
        }
    }

    static KEY: OnceLock<PttKey> = OnceLock::new();
    static DOWN: AtomicBool = AtomicBool::new(false);

    pub fn active_key() -> PttKey {
        KEY.get().copied().unwrap_or(DEFAULT_KEY)
    }

    pub fn spawn(key: PttKey) -> Result<Receiver<PttEvent>> {
        KEY.set(key).map_err(|_| anyhow!("PTT key already bound"))?;
        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        thread::Builder::new()
            .name("noma-hotkey".into())
            .spawn(move || {
                let tap = match install(tx, key) {
                    Ok(tap) => tap,
                    Err(err) => {
                        let _ = ready_tx.send(Err(err));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                // The tap only fires on the thread that runs its run loop.
                CFRunLoop::run_current();
                drop(tap);
            })
            .context("spawn hotkey thread")?;

        ready_rx
            .recv()
            .context("hotkey thread exited before ready")??;
        Ok(rx)
    }

    fn install(tx: Sender<PttEvent>, key: PttKey) -> Result<CGEventTap<'static>> {
        let keycode = macos_keycode(key);
        let flag = modifier_flag(key);

        let tap = CGEventTap::new(
            CGEventTapLocation::HID,
            CGEventTapPlacement::HeadInsertEventTap,
            CGEventTapOptions::Default,
            vec![
                CGEventType::KeyDown,
                CGEventType::KeyUp,
                CGEventType::FlagsChanged,
            ],
            move |_proxy, etype, event| {
                let code = event.get_integer_value_field(EventField::KEYBOARD_EVENT_KEYCODE);
                if code == keycode {
                    let is_down = match (etype, flag) {
                        (CGEventType::FlagsChanged, Some(flag)) => {
                            event.get_flags().contains(flag)
                        }
                        (CGEventType::KeyDown, None) => true,
                        (CGEventType::KeyUp, None) => false,
                        _ => return CallbackResult::Keep,
                    };
                    emit(&tx, is_down);
                    return CallbackResult::Drop;
                }
                CallbackResult::Keep
            },
        )
        .map_err(|()| {
            anyhow!(
                "could not install the hold-to-talk listener - grant Noma Accessibility \
                 access in System Settings > Privacy & Security > Accessibility, then restart it"
            )
        })?;

        let loop_source = tap
            .mach_port()
            .create_runloop_source(0)
            .map_err(|()| anyhow!("create hotkey run loop source"))?;
        CFRunLoop::get_current().add_source(&loop_source, unsafe { kCFRunLoopCommonModes });
        tap.enable();
        Ok(tap)
    }

    pub fn wait_until_released(timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !DOWN.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(8));
        }
        thread::sleep(Duration::from_millis(25));
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
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use anyhow::{bail, Result};

    use super::{PttEvent, PttKey, DEFAULT_KEY};

    pub fn active_key() -> PttKey {
        DEFAULT_KEY
    }

    pub fn spawn(_key: PttKey) -> Result<Receiver<PttEvent>> {
        bail!("Noma's hold-to-talk listener currently supports Windows and macOS only");
    }

    pub fn wait_until_released(timeout: Duration) {
        std::thread::sleep(timeout.min(Duration::from_millis(25)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_is_findable_by_its_id() {
        for key in KEYS {
            assert_eq!(key_from_id(key.id), Some(*key));
        }
    }

    #[test]
    fn ids_and_virtual_key_codes_are_unique() {
        for (index, key) in KEYS.iter().enumerate() {
            for other in &KEYS[index + 1..] {
                assert_ne!(key.id, other.id, "duplicate id {}", key.id);
                assert_ne!(key.vk, other.vk, "duplicate vk for {}", key.id);
            }
        }
    }

    #[test]
    fn an_unknown_id_falls_back_to_the_default_key() {
        assert!(key_from_id("left-ctrl").is_none());
        assert_eq!(key_or_default("left-ctrl"), DEFAULT_KEY);
    }
}
