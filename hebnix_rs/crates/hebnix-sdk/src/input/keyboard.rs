//! hotkey detection via evdev, linux only.
//!
//! Key names match the python `keyboard` lib ("tab", "f2", "ctrl", etc) so
//! old configs still work.
//!
//! Requires read access to `/dev/input/event*`. On Arch that means the
//! current user must be in the `input` group (`sudo usermod -aG input $USER`,
//! then re-login) -- this module does not attempt to elevate privileges, it
//! just returns "not pressed"/empty results if a device can't be opened.

use std::time::{Duration, Instant};

use evdev::{Device, KeyCode};

// (name, evdev KeyCode) for named keys. names line up with the windows vk
// table so existing binds keep working.
fn named_keys() -> &'static [(&'static str, KeyCode)] {
    &[
        ("backspace", KeyCode::KEY_BACKSPACE),
        ("tab", KeyCode::KEY_TAB),
        ("enter", KeyCode::KEY_ENTER),
        ("return", KeyCode::KEY_ENTER),
        ("shift", KeyCode::KEY_LEFTSHIFT),
        ("ctrl", KeyCode::KEY_LEFTCTRL),
        ("control", KeyCode::KEY_LEFTCTRL),
        ("alt", KeyCode::KEY_LEFTALT),
        ("pause", KeyCode::KEY_PAUSE),
        ("caps lock", KeyCode::KEY_CAPSLOCK),
        ("esc", KeyCode::KEY_ESC),
        ("escape", KeyCode::KEY_ESC),
        ("space", KeyCode::KEY_SPACE),
        ("page up", KeyCode::KEY_PAGEUP),
        ("page down", KeyCode::KEY_PAGEDOWN),
        ("end", KeyCode::KEY_END),
        ("home", KeyCode::KEY_HOME),
        ("left", KeyCode::KEY_LEFT),
        ("up", KeyCode::KEY_UP),
        ("right", KeyCode::KEY_RIGHT),
        ("down", KeyCode::KEY_DOWN),
        ("print screen", KeyCode::KEY_SYSRQ),
        ("insert", KeyCode::KEY_INSERT),
        ("delete", KeyCode::KEY_DELETE),
        ("f1", KeyCode::KEY_F1),
        ("f2", KeyCode::KEY_F2),
        ("f3", KeyCode::KEY_F3),
        ("f4", KeyCode::KEY_F4),
        ("f5", KeyCode::KEY_F5),
        ("f6", KeyCode::KEY_F6),
        ("f7", KeyCode::KEY_F7),
        ("f8", KeyCode::KEY_F8),
        ("f9", KeyCode::KEY_F9),
        ("f10", KeyCode::KEY_F10),
        ("f11", KeyCode::KEY_F11),
        ("f12", KeyCode::KEY_F12),
        ("num lock", KeyCode::KEY_NUMLOCK),
        ("scroll lock", KeyCode::KEY_SCROLLLOCK),
        ("left shift", KeyCode::KEY_LEFTSHIFT),
        ("right shift", KeyCode::KEY_RIGHTSHIFT),
        ("left ctrl", KeyCode::KEY_LEFTCTRL),
        ("right ctrl", KeyCode::KEY_RIGHTCTRL),
        ("left alt", KeyCode::KEY_LEFTALT),
        ("right alt", KeyCode::KEY_RIGHTALT),
        ("left windows", KeyCode::KEY_LEFTMETA),
        ("right windows", KeyCode::KEY_RIGHTMETA),
        (";", KeyCode::KEY_SEMICOLON),
        ("=", KeyCode::KEY_EQUAL),
        (",", KeyCode::KEY_COMMA),
        ("-", KeyCode::KEY_MINUS),
        (".", KeyCode::KEY_DOT),
        ("/", KeyCode::KEY_SLASH),
        ("`", KeyCode::KEY_GRAVE),
        ("[", KeyCode::KEY_LEFTBRACE),
        ("\\", KeyCode::KEY_BACKSLASH),
        ("]", KeyCode::KEY_RIGHTBRACE),
        ("'", KeyCode::KEY_APOSTROPHE),
        ("+", KeyCode::KEY_EQUAL),
    ]
}

fn letter_digit_code(c: char) -> Option<KeyCode> {
    Some(match c.to_ascii_lowercase() {
        'a' => KeyCode::KEY_A,
        'b' => KeyCode::KEY_B,
        'c' => KeyCode::KEY_C,
        'd' => KeyCode::KEY_D,
        'e' => KeyCode::KEY_E,
        'f' => KeyCode::KEY_F,
        'g' => KeyCode::KEY_G,
        'h' => KeyCode::KEY_H,
        'i' => KeyCode::KEY_I,
        'j' => KeyCode::KEY_J,
        'k' => KeyCode::KEY_K,
        'l' => KeyCode::KEY_L,
        'm' => KeyCode::KEY_M,
        'n' => KeyCode::KEY_N,
        'o' => KeyCode::KEY_O,
        'p' => KeyCode::KEY_P,
        'q' => KeyCode::KEY_Q,
        'r' => KeyCode::KEY_R,
        's' => KeyCode::KEY_S,
        't' => KeyCode::KEY_T,
        'u' => KeyCode::KEY_U,
        'v' => KeyCode::KEY_V,
        'w' => KeyCode::KEY_W,
        'x' => KeyCode::KEY_X,
        'y' => KeyCode::KEY_Y,
        'z' => KeyCode::KEY_Z,
        '0' => KeyCode::KEY_0,
        '1' => KeyCode::KEY_1,
        '2' => KeyCode::KEY_2,
        '3' => KeyCode::KEY_3,
        '4' => KeyCode::KEY_4,
        '5' => KeyCode::KEY_5,
        '6' => KeyCode::KEY_6,
        '7' => KeyCode::KEY_7,
        '8' => KeyCode::KEY_8,
        '9' => KeyCode::KEY_9,
        _ => return None,
    })
}

/// key name to evdev KeyCode
pub fn name_to_code(key: &str) -> Option<KeyCode> {
    let lower = key.trim().to_lowercase();
    if let Some((_, code)) = named_keys().iter().find(|(n, _)| *n == lower) {
        return Some(*code);
    }
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return letter_digit_code(c);
    }
    None
}

/// evdev KeyCode back to key name, used by hotkey capture
pub fn code_to_name(code: KeyCode) -> Option<String> {
    for c in 'a'..='z' {
        if letter_digit_code(c) == Some(code) {
            return Some(c.to_string());
        }
    }
    for c in '0'..='9' {
        if letter_digit_code(c) == Some(code) {
            return Some(c.to_string());
        }
    }
    named_keys()
        .iter()
        .find(|(_, cc)| *cc == code)
        .map(|(n, _)| n.to_string())
}

/// all keyboard-capable evdev devices (has EV_KEY + looks like a keyboard,
/// not a mouse/joystick-only device).
///
/// `evdev::enumerate()` walks and opens every `/dev/input/event*` node,
/// which in practice takes several hundred ms (confirmed live: a poller
/// calling this every 35ms was actually only completing a cycle every
/// ~400-450ms, and was a measurable source of app-wide sluggishness, plus
/// plugin key-bind polls competing for the same enumeration). Cache the
/// opened `Device` handles and reuse them -- `get_key_state()` on an
/// already-open fd is a cheap ioctl. Re-enumerate periodically to pick up
/// hot-plugged keyboards.
fn with_keyboard_devices<T>(f: impl FnOnce(&mut Vec<Device>) -> T) -> T {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    static CACHE: OnceLock<Mutex<(Instant, Vec<Device>)>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(60), Vec::new())));
    let mut guard = cache.lock().unwrap();
    if guard.1.is_empty() || guard.0.elapsed() > Duration::from_secs(10) {
        let mut fresh = Vec::new();
        for (_path, dev) in evdev::enumerate() {
            if dev
                .supported_keys()
                .map(|keys| keys.contains(KeyCode::KEY_ENTER) && keys.contains(KeyCode::KEY_A))
                .unwrap_or(false)
            {
                fresh.push(dev);
            }
        }
        guard.1 = fresh;
        guard.0 = Instant::now();
    }
    f(&mut guard.1)
}

/// true if key is held down right now, across all keyboard devices.
pub fn is_key_pressed(key: &str) -> bool {
    let Some(code) = name_to_code(key) else {
        return false;
    };
    with_keyboard_devices(|devs| {
        for dev in devs {
            if let Ok(state) = dev.get_key_state() {
                if state.contains(code) {
                    return true;
                }
            }
        }
        false
    })
}

/// single non-blocking scan, name of whatever key is held or None.
pub fn scan_pressed_key() -> Option<String> {
    with_keyboard_devices(|devs| {
        for dev in devs {
            if let Ok(state) = dev.get_key_state() {
                for code in state.iter() {
                    if let Some(name) = code_to_name(code) {
                        return Some(name);
                    }
                }
            }
        }
        None
    })
}

/// blocks until a key is pressed, returns its name. None if timeout hits first.
pub fn detect_hotkey(timeout: Option<Duration>) -> Option<String> {
    let start = Instant::now();

    // wait for everything to release first so the click that opened capture
    // doesn't count
    loop {
        if scan_pressed_key().is_none() {
            break;
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    loop {
        if let Some(name) = scan_pressed_key() {
            return Some(name);
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

// --- synthetic input (uinput virtual keyboard) ---
//
// used for things like quick-chat automation: tapping the chat-open key and
// typing the message. gated by callers (not here) to whatever policy
// applies, e.g. hebnix's "not while a match is in progress" rule for raw
// input. Requires rw access to /dev/uinput (the `input` group on most
// distros, same requirement as reading /dev/input/event* above).

use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, KeyEvent};

fn all_known_codes() -> AttributeSet<KeyCode> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for &(_, code) in named_keys() {
        keys.insert(code);
    }
    for c in ('a'..='z').chain('0'..='9') {
        if let Some(code) = letter_digit_code(c) {
            keys.insert(code);
        }
    }
    keys
}

fn virtual_keyboard() -> Option<&'static std::sync::Mutex<VirtualDevice>> {
    static DEVICE: std::sync::OnceLock<Option<std::sync::Mutex<VirtualDevice>>> =
        std::sync::OnceLock::new();
    DEVICE
        .get_or_init(|| {
            match VirtualDevice::builder()
                .and_then(|b| b.name("Hebnix Virtual Keyboard").with_keys(&all_known_codes()))
                .and_then(|b| b.build())
            {
                Ok(dev) => Some(std::sync::Mutex::new(dev)),
                Err(e) => {
                    tracing::warn!(
                        "synthetic input unavailable, couldn't create uinput virtual keyboard \
                         (need rw on /dev/uinput, usually the 'input' group): {e}"
                    );
                    None
                }
            }
        })
        .as_ref()
}

const TAP_GAP: Duration = Duration::from_millis(1);

/// press+release an evdev key code
fn tap_code(dev: &mut VirtualDevice, code: KeyCode) {
    let _ = dev.emit(&[*KeyEvent::new(code, 1)]);
    std::thread::sleep(TAP_GAP);
    let _ = dev.emit(&[*KeyEvent::new(code, 0)]);
    std::thread::sleep(TAP_GAP);
}

/// press+release a named key ("enter", "t", "f1", ...). false if the name
/// isn't recognized or the virtual device couldn't be created.
pub fn tap_key(name: &str) -> bool {
    let Some(code) = name_to_code(name) else {
        return false;
    };
    let Some(mutex) = virtual_keyboard() else {
        return false;
    };
    let mut dev = mutex.lock().unwrap();
    tap_code(&mut dev, code);
    true
}

/// ascii char to (KeyCode, needs_shift). None for anything not on a
/// standard US layout key (good enough for chat text).
fn char_to_code(c: char) -> Option<(KeyCode, bool)> {
    if c.is_ascii_alphabetic() {
        return letter_digit_code(c.to_ascii_lowercase())
            .map(|code| (code, c.is_ascii_uppercase()));
    }
    if c.is_ascii_digit() {
        return letter_digit_code(c).map(|code| (code, false));
    }
    let (code, shift) = match c {
        ' ' => (KeyCode::KEY_SPACE, false),
        '\'' => (KeyCode::KEY_APOSTROPHE, false),
        '"' => (KeyCode::KEY_APOSTROPHE, true),
        ',' => (KeyCode::KEY_COMMA, false),
        '<' => (KeyCode::KEY_COMMA, true),
        '.' => (KeyCode::KEY_DOT, false),
        '>' => (KeyCode::KEY_DOT, true),
        '/' => (KeyCode::KEY_SLASH, false),
        '?' => (KeyCode::KEY_SLASH, true),
        ';' => (KeyCode::KEY_SEMICOLON, false),
        ':' => (KeyCode::KEY_SEMICOLON, true),
        '-' => (KeyCode::KEY_MINUS, false),
        '_' => (KeyCode::KEY_MINUS, true),
        '=' => (KeyCode::KEY_EQUAL, false),
        '+' => (KeyCode::KEY_EQUAL, true),
        '[' => (KeyCode::KEY_LEFTBRACE, false),
        '{' => (KeyCode::KEY_LEFTBRACE, true),
        ']' => (KeyCode::KEY_RIGHTBRACE, false),
        '}' => (KeyCode::KEY_RIGHTBRACE, true),
        '\\' => (KeyCode::KEY_BACKSLASH, false),
        '|' => (KeyCode::KEY_BACKSLASH, true),
        '`' => (KeyCode::KEY_GRAVE, false),
        '~' => (KeyCode::KEY_GRAVE, true),
        '1' => (KeyCode::KEY_1, false),
        '!' => (KeyCode::KEY_1, true),
        '2' => (KeyCode::KEY_2, false),
        '@' => (KeyCode::KEY_2, true),
        '3' => (KeyCode::KEY_3, false),
        '#' => (KeyCode::KEY_3, true),
        '4' => (KeyCode::KEY_4, false),
        '$' => (KeyCode::KEY_4, true),
        '5' => (KeyCode::KEY_5, false),
        '%' => (KeyCode::KEY_5, true),
        '6' => (KeyCode::KEY_6, false),
        '^' => (KeyCode::KEY_6, true),
        '7' => (KeyCode::KEY_7, false),
        '&' => (KeyCode::KEY_7, true),
        '8' => (KeyCode::KEY_8, false),
        '*' => (KeyCode::KEY_8, true),
        '9' => (KeyCode::KEY_9, false),
        '(' => (KeyCode::KEY_9, true),
        '0' => (KeyCode::KEY_0, false),
        ')' => (KeyCode::KEY_0, true),
        _ => return None,
    };
    Some((code, shift))
}

/// type text by tapping real key codes through the virtual keyboard (shift
/// held for caps/punctuation as needed), US layout only. Unmappable
/// characters (anything outside ascii) are skipped.
///
/// deliberately real key events, not some higher-level "insert text" API:
/// games that read keyboard via raw input (most UE titles, including RL)
/// only see real key events, same as this app's own bind-capture reading
/// real key events rather than IME/text composition.
pub fn type_text(text: &str) {
    let _ = type_text_while(text, || true);
}

/// type text while the supplied guard remains true.
pub fn type_text_while<F>(text: &str, should_continue: F) -> bool
where
    F: Fn() -> bool,
{
    let Some(mutex) = virtual_keyboard() else {
        return false;
    };
    let mut dev = mutex.lock().unwrap();
    for c in text.chars() {
        if !should_continue() {
            return false;
        }
        let Some((code, need_shift)) = char_to_code(c) else {
            continue;
        };
        if need_shift {
            let _ = dev.emit(&[*KeyEvent::new(KeyCode::KEY_LEFTSHIFT, 1)]);
            std::thread::sleep(TAP_GAP);
        }
        tap_code(&mut dev, code);
        if need_shift {
            let _ = dev.emit(&[*KeyEvent::new(KeyCode::KEY_LEFTSHIFT, 0)]);
            std::thread::sleep(TAP_GAP);
        }
    }
    true
}
