//! Linux evdev key codes, from `include/uapi/linux/input-event-codes.h`.
//!
//! Hardcoded rather than imported from a crate so the keymap can be built,
//! inspected and tested on a machine that will never type a character -- and
//! because these numbers are kernel ABI and therefore do not move.

use std::collections::HashMap;
use std::sync::OnceLock;

pub const KEY_ESC: u16 = 1;
pub const KEY_A: u16 = 30;
pub const KEY_Z: u16 = 44;
pub const KEY_BACKSPACE: u16 = 14;
pub const KEY_TAB: u16 = 15;
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_V: u16 = 47;
pub const KEY_SPACE: u16 = 57;

#[rustfmt::skip]
pub static KEY_CODES: &[(&str, u16)] = &[
    ("KEY_ESC", 1), ("KEY_1", 2), ("KEY_2", 3), ("KEY_3", 4), ("KEY_4", 5), ("KEY_5", 6),
    ("KEY_6", 7), ("KEY_7", 8), ("KEY_8", 9), ("KEY_9", 10), ("KEY_0", 11),
    ("KEY_MINUS", 12), ("KEY_EQUAL", 13), ("KEY_BACKSPACE", 14), ("KEY_TAB", 15),
    ("KEY_Q", 16), ("KEY_W", 17), ("KEY_E", 18), ("KEY_R", 19), ("KEY_T", 20),
    ("KEY_Y", 21), ("KEY_U", 22), ("KEY_I", 23), ("KEY_O", 24), ("KEY_P", 25),
    ("KEY_LEFTBRACE", 26), ("KEY_RIGHTBRACE", 27), ("KEY_ENTER", 28),
    ("KEY_LEFTCTRL", 29), ("KEY_A", 30), ("KEY_S", 31), ("KEY_D", 32), ("KEY_F", 33),
    ("KEY_G", 34), ("KEY_H", 35), ("KEY_J", 36), ("KEY_K", 37), ("KEY_L", 38),
    ("KEY_SEMICOLON", 39), ("KEY_APOSTROPHE", 40), ("KEY_GRAVE", 41),
    ("KEY_LEFTSHIFT", 42), ("KEY_BACKSLASH", 43), ("KEY_Z", 44), ("KEY_X", 45),
    ("KEY_C", 46), ("KEY_V", 47), ("KEY_B", 48), ("KEY_N", 49), ("KEY_M", 50),
    ("KEY_COMMA", 51), ("KEY_DOT", 52), ("KEY_SLASH", 53), ("KEY_RIGHTSHIFT", 54),
    ("KEY_KPASTERISK", 55), ("KEY_LEFTALT", 56), ("KEY_SPACE", 57),
    ("KEY_CAPSLOCK", 58), ("KEY_F1", 59), ("KEY_F2", 60), ("KEY_F3", 61), ("KEY_F4", 62),
    ("KEY_F5", 63), ("KEY_F6", 64), ("KEY_F7", 65), ("KEY_F8", 66), ("KEY_F9", 67),
    ("KEY_F10", 68), ("KEY_NUMLOCK", 69), ("KEY_SCROLLLOCK", 70),
    ("KEY_KP7", 71), ("KEY_KP8", 72), ("KEY_KP9", 73), ("KEY_KPMINUS", 74),
    ("KEY_KP4", 75), ("KEY_KP5", 76), ("KEY_KP6", 77), ("KEY_KPPLUS", 78),
    ("KEY_KP1", 79), ("KEY_KP2", 80), ("KEY_KP3", 81), ("KEY_KP0", 82),
    ("KEY_KPDOT", 83), ("KEY_102ND", 86), ("KEY_F11", 87), ("KEY_F12", 88),
    ("KEY_KPENTER", 96), ("KEY_RIGHTCTRL", 97), ("KEY_KPSLASH", 98),
    ("KEY_SYSRQ", 99), ("KEY_RIGHTALT", 100), ("KEY_LINEFEED", 101),
    ("KEY_HOME", 102), ("KEY_UP", 103), ("KEY_PAGEUP", 104), ("KEY_LEFT", 105),
    ("KEY_RIGHT", 106), ("KEY_END", 107), ("KEY_DOWN", 108), ("KEY_PAGEDOWN", 109),
    ("KEY_INSERT", 110), ("KEY_DELETE", 111), ("KEY_PAUSE", 119),
    ("KEY_LEFTMETA", 125), ("KEY_RIGHTMETA", 126), ("KEY_COMPOSE", 127),
];

/// Look a key up by its evdev name, e.g. `"KEY_ESC"`.
pub fn key_code(name: &str) -> Option<u16> {
    KEY_CODES.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// The modifiers a layout may ask to be held. These double as key tokens: the
/// schedule presses them by name so Shift can lead the letter it modifies.
pub static MODIFIER_NAMES: &[&str] = &[
    "KEY_LEFTSHIFT",
    "KEY_RIGHTSHIFT",
    "KEY_LEFTCTRL",
    "KEY_RIGHTCTRL",
    "KEY_LEFTALT",
    "KEY_RIGHTALT",
    "KEY_LEFTMETA",
    "KEY_RIGHTMETA",
];

pub fn is_modifier(token: &str) -> bool {
    MODIFIER_NAMES.contains(&token)
}

/// Every modifier the typing path may hold, for the panic release.
pub fn modifier_codes() -> impl Iterator<Item = u16> {
    MODIFIER_NAMES.iter().filter_map(|n| key_code(n))
}

/// Engine-level key names the backends understand, in addition to characters.
pub static NAMED_KEYS: &[&str] = &["enter", "tab", "backspace", "escape", "space"];

/// Engine-level key names mapped to evdev codes.
pub fn named_key_code(name: &str) -> Option<u16> {
    match name {
        "enter" => Some(KEY_ENTER),
        "tab" => Some(KEY_TAB),
        "backspace" => Some(KEY_BACKSPACE),
        "escape" => Some(KEY_ESC),
        "space" => Some(KEY_SPACE),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// HID usage codes, for MSC_SCAN
// ---------------------------------------------------------------------------
// Every real keyboard emits an `EV_MSC / MSC_SCAN` event carrying the raw
// scancode immediately before each `EV_KEY`, press and release alike. This is
// `hid_keyboard[]` from `drivers/hid/hid-input.c`, indexed by HID usage and
// giving the evdev keycode; a USB keyboard reports `0x70000 | usage`.
// Inverting it gives the scancode to announce for a key.
#[rustfmt::skip]
static HID_KEYBOARD: [u16; 104] = [
      0,  0,  0,  0, 30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38,
     50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,  2,  3,
      4,  5,  6,  7,  8,  9, 10, 11, 28,  1, 14, 15, 57, 12, 13, 26,
     27, 43, 43, 39, 40, 41, 51, 52, 53, 58, 59, 60, 61, 62, 63, 64,
     65, 66, 67, 68, 87, 88, 99, 70, 119, 110, 102, 104, 111, 107, 109, 106,
    105, 108, 103, 69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71,
     72, 73, 82, 83, 86, 127, 116, 117,
];

const HID_KEYBOARD_PAGE: u32 = 0x70000;

/// evdev keycode -> the `MSC_SCAN` value a USB keyboard would report for it.
pub fn scancode(code: u16) -> Option<u32> {
    static TABLE: OnceLock<HashMap<u16, u32>> = OnceLock::new();
    TABLE
        .get_or_init(|| {
            let mut out: HashMap<u16, u32> = HashMap::new();
            // Modifier usages live at 0xE0-0xE7, past the block tabulated above.
            for (name, usage) in [
                ("KEY_LEFTCTRL", 0xE0),
                ("KEY_LEFTSHIFT", 0xE1),
                ("KEY_LEFTALT", 0xE2),
                ("KEY_LEFTMETA", 0xE3),
                ("KEY_RIGHTCTRL", 0xE4),
                ("KEY_RIGHTSHIFT", 0xE5),
                ("KEY_RIGHTALT", 0xE6),
                ("KEY_RIGHTMETA", 0xE7),
            ] {
                if let Some(code) = key_code(name) {
                    out.insert(code, usage);
                }
            }
            for (usage, &keycode) in HID_KEYBOARD.iter().enumerate() {
                // The table repeats a keycode for two usages in one place
                // (Backslash and the ISO hash key); the first wins, as it does
                // on real hardware.
                if keycode != 0 {
                    out.entry(keycode).or_insert(usage as u32);
                }
            }
            out.into_iter()
                .map(|(k, u)| (k, HID_KEYBOARD_PAGE | u))
                .collect()
        })
        .get(&code)
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_report_their_hid_usage() {
        assert_eq!(scancode(30), Some(0x70004)); // KEY_A is usage 4
        assert_eq!(scancode(KEY_LEFTSHIFT), Some(0x700E1));
    }

    #[test]
    fn every_named_modifier_has_a_code() {
        assert_eq!(modifier_codes().count(), MODIFIER_NAMES.len());
    }

    #[test]
    fn the_first_backslash_usage_wins() {
        assert_eq!(scancode(43), Some(0x70031));
    }

    #[test]
    fn modifier_codes_are_all_real_keys() {
        for code in modifier_codes() {
            assert!(KEY_CODES.iter().any(|(_, c)| *c == code));
        }
    }

    #[test]
    fn every_key_worth_typing_has_a_scancode() {
        // MSC_SCAN is emitted for each key; a gap would be a silent omission.
        let missing: Vec<&str> = KEY_CODES
            .iter()
            .filter(|(name, code)| *name != "KEY_LINEFEED" && scancode(*code).is_none())
            .map(|(name, _)| *name)
            .collect();
        assert!(missing.is_empty(), "{missing:?}");
    }

    #[test]
    fn scancodes_are_hid_keyboard_usages() {
        // Spot-checked against HID usage page 7, which is what USB reports.
        for (name, usage) in [
            ("KEY_A", 0x04),
            ("KEY_Z", 0x1D),
            ("KEY_1", 0x1E),
            ("KEY_ENTER", 0x28),
            ("KEY_ESC", 0x29),
            ("KEY_BACKSPACE", 0x2A),
            ("KEY_TAB", 0x2B),
            ("KEY_SPACE", 0x2C),
            ("KEY_LEFTSHIFT", 0xE1),
            ("KEY_LEFTCTRL", 0xE0),
        ] {
            assert_eq!(
                scancode(key_code(name).unwrap()),
                Some(0x70000 | usage),
                "{name}"
            );
        }
    }

    #[test]
    fn no_two_keys_claim_the_same_scancode() {
        let mut seen = std::collections::HashSet::new();
        for (name, code) in KEY_CODES {
            if let Some(scan) = scancode(*code) {
                assert!(seen.insert(scan), "{name} repeats scancode {scan:#x}");
            }
        }
    }

    #[test]
    fn key_names_and_codes_are_each_unique() {
        let names: std::collections::HashSet<_> = KEY_CODES.iter().map(|(n, _)| n).collect();
        let codes: std::collections::HashSet<_> = KEY_CODES.iter().map(|(_, c)| c).collect();
        assert_eq!(names.len(), KEY_CODES.len());
        assert_eq!(codes.len(), KEY_CODES.len());
    }
}
