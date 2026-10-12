//! Auto push-to-talk: hold the user's Discord PTT key for as long as a clip is playing.
//!
//! Discord's voice-activity detector drops the non-speech parts of clips. With push-to-talk the
//! client transmits everything while the key is down, so Hotmic presses it for the user. Keys
//! are injected with `SendInput`, which Discord's global hotkey listener accepts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What to press: a keyboard virtual-key code or a mouse X button (1 or 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    Key(u16),
    MouseX(u16),
}

/// Translate a `KeyboardEvent.code` style name (plus "Mouse4"/"Mouse5") into an input.
pub fn parse(code: &str) -> Option<Input> {
    let c = code.trim();
    if let Some(rest) = c.strip_prefix("Key") {
        let ch = rest.chars().next()?;
        if rest.len() == 1 && ch.is_ascii_uppercase() {
            return Some(Input::Key(0x41 + (ch as u16 - b'A' as u16)));
        }
    }
    if let Some(rest) = c.strip_prefix("Digit") {
        let n: u16 = rest.parse().ok()?;
        return (n <= 9).then_some(Input::Key(0x30 + n));
    }
    if let Some(rest) = c.strip_prefix("Numpad") {
        return match rest {
            "Add" => Some(Input::Key(0x6B)),
            "Subtract" => Some(Input::Key(0x6D)),
            "Multiply" => Some(Input::Key(0x6A)),
            "Divide" => Some(Input::Key(0x6F)),
            "Decimal" => Some(Input::Key(0x6E)),
            "Enter" => Some(Input::Key(0x0D)),
            _ => rest.parse::<u16>().ok().filter(|n| *n <= 9).map(|n| Input::Key(0x60 + n)),
        };
    }
    if let Some(rest) = c.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u16>() {
            if (1..=24).contains(&n) {
                return Some(Input::Key(0x70 + n - 1));
            }
        }
    }
    let vk = match c {
        "Space" => 0x20,
        "Tab" => 0x09,
        "Enter" => 0x0D,
        "Backspace" => 0x08,
        "Escape" => 0x1B,
        "CapsLock" => 0x14,
        "ScrollLock" => 0x91,
        "NumLock" => 0x90,
        "Pause" => 0x13,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "ControlLeft" => 0xA2,
        "ControlRight" => 0xA3,
        "ShiftLeft" => 0xA0,
        "ShiftRight" => 0xA1,
        "AltLeft" => 0xA4,
        "AltRight" => 0xA5,
        "Backquote" => 0xC0,
        "Minus" => 0xBD,
        "Equal" => 0xBB,
        "BracketLeft" => 0xDB,
        "BracketRight" => 0xDD,
        "Backslash" => 0xDC,
        "Semicolon" => 0xBA,
        "Quote" => 0xDE,
        "Comma" => 0xBC,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Mouse4" => return Some(Input::MouseX(1)),
        "Mouse5" => return Some(Input::MouseX(2)),
        _ => return None,
    };
    Some(Input::Key(vk))
}

#[cfg(windows)]
pub fn send(input: Input, down: bool) {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, VIRTUAL_KEY,
    };
    let inp = match input {
        Input::Key(vk) => INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: 0,
                    dwFlags: if down { KEYBD_EVENT_FLAGS(0) } else { KEYEVENTF_KEYUP },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
        Input::MouseX(btn) => INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0,
                    dy: 0,
                    mouseData: btn as u32,
                    dwFlags: if down { MOUSEEVENTF_XDOWN } else { MOUSEEVENTF_XUP },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        },
    };
    unsafe {
        SendInput(&[inp], std::mem::size_of::<INPUT>() as i32);
    }
}

#[cfg(not(windows))]
pub fn send(_input: Input, _down: bool) {}

/// Shared PTT state the UI can read.
#[derive(Default)]
pub struct PttState {
    pub held: AtomicBool,
}

/// Run forever: watch `active()` and keep the key down while clips play, releasing
/// `release_after` once the last clip ends so the tail is transmitted too.
pub fn run_loop(
    state: Arc<PttState>,
    mut settings: impl FnMut() -> Option<Input>,
    mut active: impl FnMut() -> u32,
    release_after: Duration,
) {
    let mut held: Option<Input> = None;
    let mut idle_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(15));
        let wanted = settings();
        let playing = active() > 0;
        match (held, wanted) {
            (Some(k), None) => {
                // feature switched off or key changed: release
                send(k, false);
                held = None;
                state.held.store(false, Ordering::Relaxed);
            }
            (Some(k), Some(w)) if k != w => {
                send(k, false);
                held = None;
                state.held.store(false, Ordering::Relaxed);
            }
            _ => {}
        }
        let Some(key) = wanted else { continue };
        if playing {
            idle_since = None;
            if held.is_none() {
                send(key, true);
                held = Some(key);
                state.held.store(true, Ordering::Relaxed);
            }
        } else if held.is_some() {
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= release_after {
                send(key, false);
                held = None;
                idle_since = None;
                state.held.store(false, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_codes() {
        assert_eq!(parse("KeyV"), Some(Input::Key(0x56)));
        assert_eq!(parse("Digit3"), Some(Input::Key(0x33)));
        assert_eq!(parse("F13"), Some(Input::Key(0x7C)));
        assert_eq!(parse("Numpad0"), Some(Input::Key(0x60)));
        assert_eq!(parse("NumpadAdd"), Some(Input::Key(0x6B)));
        assert_eq!(parse("Mouse4"), Some(Input::MouseX(1)));
        assert_eq!(parse("CapsLock"), Some(Input::Key(0x14)));
        assert_eq!(parse("Nope"), None);
        assert_eq!(parse("F25"), None);
    }
}
