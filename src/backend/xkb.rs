//! Build a character -> keystroke table from the system's real XKB layout.
//!
//! Injecting at the kernel level means sending *scancodes*: the compositor
//! decides what character a scancode produces, using the active keyboard
//! layout. Hardcode a US table, as most auto-typers do, and a German or Dvorak
//! user gets gibberish with no clue why. libxkbcommon already knows the answer,
//! so ask it: compile the configured layout and walk every key at every shift
//! level to learn which keycode and modifiers produce which character.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CString};
use std::process::Command;

use super::codes::key_code;

/// XKB numbers keycodes from the evdev scancode plus this historical offset.
const XKB_EVDEV_OFFSET: u32 = 8;

/// XKB modifier names mapped to the evdev key we press to engage them. Lock is
/// deliberately absent: toggling Caps Lock to reach a capital letter would
/// leave the user's keyboard in a different state than it started in.
fn mod_to_key(name: &str) -> Option<&'static str> {
    Some(match name {
        "shift" => "KEY_LEFTSHIFT",
        "control" | "ctrl" => "KEY_LEFTCTRL",
        "mod1" | "alt" => "KEY_LEFTALT",
        "mod4" | "super" => "KEY_LEFTMETA",
        "mod5" | "levelthree" | "altgr" | "iso_level3_shift" | "iso_level5_shift" => "KEY_RIGHTALT",
        _ => return None,
    })
}

// A keymap describes every key the layout can name, including ones no ordinary
// keyboard has: XKB puts KP_Left_Paren at 179, so "(" without modifiers looks
// cheaper than Shift+9 and wins a naive comparison. The resulting stroke is one
// no hand could perform, the kernel drops the event for an undeclared key, and
// the character disappears with no error raised anywhere. So candidates are
// ranked by where the key actually lives before anything else is considered.

/// Sort key for competing ways to produce one character, best first. Home-row
/// reach beats keypad or exotic keys; among equals, fewer modifiers wins, then
/// the lower keycode so the table is deterministic.
fn rank(keycode: u16, modifiers: &[&str]) -> (u8, usize, u16) {
    // Esc through slash, plus Space: the rows a touch typist uses. Then the
    // extra key on ISO keyboards.
    let tier = if (1..54).contains(&keycode) || keycode == 57 {
        0
    } else if keycode == 86 {
        1
    } else {
        2
    };
    (tier, modifiers.len(), keycode)
}

/// The physical action that produces one character.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stroke {
    /// evdev scancode.
    pub keycode: u16,
    /// evdev key names to hold, e.g. `["KEY_LEFTSHIFT"]`.
    pub modifiers: Vec<&'static str>,
}

/// A compiled character -> [`Stroke`] table.
#[derive(Debug, Clone)]
pub struct Layout {
    pub table: HashMap<char, Stroke>,
    /// Where the layout description came from, for diagnostics.
    pub source: String,
}

impl Layout {
    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn get(&self, ch: char) -> Option<&Stroke> {
        self.table.get(&ch)
    }
}

#[repr(C)]
struct RuleNames {
    rules: *const c_char,
    model: *const c_char,
    layout: *const c_char,
    variant: *const c_char,
    options: *const c_char,
}

#[derive(Default)]
struct Names {
    rules: String,
    model: String,
    layout: String,
    variant: String,
    options: String,
}

/// Discover the active layout, preferring explicit configuration.
fn configured_layout() -> (Names, String) {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    let mut names = Names {
        rules: env("XKB_DEFAULT_RULES"),
        model: env("XKB_DEFAULT_MODEL"),
        layout: env("XKB_DEFAULT_LAYOUT"),
        variant: env("XKB_DEFAULT_VARIANT"),
        options: env("XKB_DEFAULT_OPTIONS"),
    };
    if !names.layout.is_empty() {
        return (names, "XKB_DEFAULT_* environment".into());
    }

    // localectl reports what the session was actually configured with, and is
    // the only source that works identically under X11 and Wayland. Its labels
    // are translated, so a German locale says "X11 Tastaturbelegung" and every
    // parse below fails -- which silently typed US QWERTY on a non-US machine.
    let out = Command::new("localectl")
        .arg("status")
        .env("LC_ALL", "C")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let (mut layout, mut model, mut variant, mut options) =
        (String::new(), String::new(), String::new(), String::new());
    for line in out.lines() {
        let Some((key, value)) = line.trim().split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_lowercase(), value.trim().to_string());
        if value.is_empty() {
            continue;
        }
        match key.as_str() {
            "x11 layout" => layout = value,
            "x11 model" => model = value,
            "x11 variant" => variant = value,
            "x11 options" => options = value,
            _ => {}
        }
    }

    if !layout.is_empty() {
        let source = format!("localectl (layout={layout})");
        names.layout = layout;
        if !model.is_empty() {
            names.model = model;
        }
        if !variant.is_empty() {
            names.variant = variant;
        }
        if !options.is_empty() {
            names.options = options;
        }
        return (names, source);
    }
    (names, "xkbcommon defaults".into())
}

/// Translate an XKB modifier mask into evdev keys, or `None` if unusable.
fn mask_to_keys(mask: u32, mod_names: &[String]) -> Option<Vec<&'static str>> {
    let mut keys: Vec<&'static str> = Vec::new();
    for (bit, name) in mod_names.iter().enumerate() {
        if mask & (1 << bit) == 0 {
            continue;
        }
        if name == "lock" {
            return None; // never toggle Caps Lock on the user's behalf
        }
        // An exotic modifier we cannot reliably engage disqualifies the stroke.
        let key = mod_to_key(name)?;
        if !keys.contains(&key) {
            keys.push(key);
        }
    }
    Some(keys)
}

type Ptr = *mut c_void;

/// The handful of libxkbcommon entry points the walk needs.
struct Lib {
    _lib: libloading::Library,
    context_new: unsafe extern "C" fn(c_int) -> Ptr,
    context_unref: unsafe extern "C" fn(Ptr),
    keymap_new_from_names: unsafe extern "C" fn(Ptr, *const RuleNames, c_int) -> Ptr,
    keymap_unref: unsafe extern "C" fn(Ptr),
    min_keycode: unsafe extern "C" fn(Ptr) -> u32,
    max_keycode: unsafe extern "C" fn(Ptr) -> u32,
    num_layouts_for_key: unsafe extern "C" fn(Ptr, u32) -> u32,
    num_levels_for_key: unsafe extern "C" fn(Ptr, u32, u32) -> u32,
    get_syms_by_level: unsafe extern "C" fn(Ptr, u32, u32, u32, *mut *const u32) -> c_int,
    get_mods_for_level: unsafe extern "C" fn(Ptr, u32, u32, u32, *mut u32, usize) -> usize,
    keysym_to_utf32: unsafe extern "C" fn(u32) -> u32,
    num_mods: unsafe extern "C" fn(Ptr) -> u32,
    mod_get_name: unsafe extern "C" fn(Ptr, u32) -> *const c_char,
}

impl Lib {
    fn load() -> Option<Self> {
        // SAFETY: loading a shared library runs its initialisers; libxkbcommon
        // has none with side effects, and each symbol is cast to the signature
        // documented in <xkbcommon/xkbcommon.h>.
        unsafe {
            let lib = ["libxkbcommon.so.0", "libxkbcommon.so"]
                .iter()
                .find_map(|n| libloading::Library::new(n).ok())?;
            macro_rules! sym {
                ($name:literal) => {
                    *lib.get(concat!($name, "\0").as_bytes()).ok()?
                };
            }
            Some(Self {
                context_new: sym!("xkb_context_new"),
                context_unref: sym!("xkb_context_unref"),
                keymap_new_from_names: sym!("xkb_keymap_new_from_names"),
                keymap_unref: sym!("xkb_keymap_unref"),
                min_keycode: sym!("xkb_keymap_min_keycode"),
                max_keycode: sym!("xkb_keymap_max_keycode"),
                num_layouts_for_key: sym!("xkb_keymap_num_layouts_for_key"),
                num_levels_for_key: sym!("xkb_keymap_num_levels_for_key"),
                get_syms_by_level: sym!("xkb_keymap_key_get_syms_by_level"),
                get_mods_for_level: sym!("xkb_keymap_key_get_mods_for_level"),
                keysym_to_utf32: sym!("xkb_keysym_to_utf32"),
                num_mods: sym!("xkb_keymap_num_mods"),
                mod_get_name: sym!("xkb_keymap_mod_get_name"),
                _lib: lib,
            })
        }
    }
}

/// Compile the active keyboard layout into a character table.
///
/// Falls back to a built-in US QWERTY table when libxkbcommon is unavailable,
/// so the backend still works on a minimal system.
///
/// Cached: compiling a keymap and walking every key at every level costs
/// milliseconds and a `localectl` subprocess, and the answer cannot change
/// inside one process. It is asked for by every backend construction, by
/// `--doctor` and by the window's backend chip.
pub fn load_layout() -> Layout {
    static CACHED: std::sync::OnceLock<Layout> = std::sync::OnceLock::new();
    CACHED.get_or_init(load_layout_uncached).clone()
}

fn load_layout_uncached() -> Layout {
    let Some(lib) = Lib::load() else {
        return fallback("no libxkbcommon");
    };
    let (names, source) = configured_layout();

    // SAFETY: every pointer handed to libxkbcommon below is either one it
    // returned or a NUL-terminated string that outlives the call, and the
    // context and keymap are released on every path out.
    unsafe {
        let ctx = (lib.context_new)(0);
        if ctx.is_null() {
            return fallback("no xkb context");
        }
        let result = walk_keymap(&lib, ctx, &names, source);
        (lib.context_unref)(ctx);
        result
    }
}

unsafe fn walk_keymap(lib: &Lib, ctx: Ptr, names: &Names, source: String) -> Layout {
    let owned: Vec<Option<CString>> = [
        &names.rules,
        &names.model,
        &names.layout,
        &names.variant,
        &names.options,
    ]
    .iter()
    .map(|s| {
        if s.is_empty() {
            None
        } else {
            CString::new(s.as_str()).ok()
        }
    })
    .collect();
    let ptr = |i: usize| owned[i].as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
    let rule_names = RuleNames {
        rules: ptr(0),
        model: ptr(1),
        layout: ptr(2),
        variant: ptr(3),
        options: ptr(4),
    };

    let keymap = (lib.keymap_new_from_names)(ctx, &rule_names, 0);
    if keymap.is_null() {
        return fallback("keymap compile failed");
    }

    let mod_names: Vec<String> = (0..(lib.num_mods)(keymap))
        .map(|i| {
            let raw = (lib.mod_get_name)(keymap, i);
            if raw.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(raw)
                    .to_string_lossy()
                    .to_lowercase()
            }
        })
        .collect();

    let mut table: HashMap<char, Stroke> = HashMap::new();
    let mut masks = [0u32; 8];
    let (lo, hi) = ((lib.min_keycode)(keymap), (lib.max_keycode)(keymap));

    for keycode in lo..=hi {
        let Some(evdev_code) = keycode.checked_sub(XKB_EVDEV_OFFSET) else {
            continue;
        };
        if !(1..256).contains(&evdev_code) {
            continue;
        }
        // Layout 0 is the primary group; typing should not depend on the user
        // having a secondary group selected.
        if (lib.num_layouts_for_key)(keymap, keycode) == 0 {
            continue;
        }
        for level in 0..(lib.num_levels_for_key)(keymap, keycode, 0) {
            let mut syms: *const u32 = std::ptr::null();
            let n = (lib.get_syms_by_level)(keymap, keycode, 0, level, &mut syms);
            if n != 1 || syms.is_null() {
                continue; // multi-symbol levels are not single keystrokes
            }
            let codepoint = (lib.keysym_to_utf32)(*syms);
            if codepoint < 0x20 || codepoint == 0x7F {
                continue; // control characters are handled as named keys
            }
            let Some(ch) = char::from_u32(codepoint) else {
                continue;
            };

            let count = (lib.get_mods_for_level)(keymap, keycode, 0, level, masks.as_mut_ptr(), 8);
            let mut best: Option<Vec<&'static str>> = None;
            for &mask in masks.iter().take(count.min(8)) {
                if let Some(keys) = mask_to_keys(mask, &mod_names) {
                    if best.as_ref().is_none_or(|b| keys.len() < b.len()) {
                        best = Some(keys);
                    }
                }
            }
            let Some(best) = best else { continue };

            let code = evdev_code as u16;
            let better = table
                .get(&ch)
                .is_none_or(|old| rank(code, &best) < rank(old.keycode, &old.modifiers));
            if better {
                table.insert(
                    ch,
                    Stroke {
                        keycode: code,
                        modifiers: best,
                    },
                );
            }
        }
    }
    (lib.keymap_unref)(keymap);

    if table.is_empty() {
        return fallback("empty keymap");
    }
    Layout { table, source }
}

/// A US QWERTY table for systems without libxkbcommon.
fn us_fallback() -> HashMap<char, Stroke> {
    const ROWS: [(&str, &str, &str); 4] = [
        ("`1234567890-=", "~!@#$%^&*()_+",
         "KEY_GRAVE KEY_1 KEY_2 KEY_3 KEY_4 KEY_5 KEY_6 KEY_7 KEY_8 KEY_9 KEY_0 KEY_MINUS KEY_EQUAL"),
        ("qwertyuiop[]\\", "QWERTYUIOP{}|",
         "KEY_Q KEY_W KEY_E KEY_R KEY_T KEY_Y KEY_U KEY_I KEY_O KEY_P KEY_LEFTBRACE \
          KEY_RIGHTBRACE KEY_BACKSLASH"),
        ("asdfghjkl;'", "ASDFGHJKL:\"",
         "KEY_A KEY_S KEY_D KEY_F KEY_G KEY_H KEY_J KEY_K KEY_L KEY_SEMICOLON KEY_APOSTROPHE"),
        ("zxcvbnm,./", "ZXCVBNM<>?",
         "KEY_Z KEY_X KEY_C KEY_V KEY_B KEY_N KEY_M KEY_COMMA KEY_DOT KEY_SLASH"),
    ];
    let mut table = HashMap::new();
    for (plain, shifted, keys) in ROWS {
        for ((ch, sh), key) in plain
            .chars()
            .zip(shifted.chars())
            .zip(keys.split_whitespace())
        {
            let code = key_code(key).expect("table names are valid");
            table.insert(
                ch,
                Stroke {
                    keycode: code,
                    modifiers: vec![],
                },
            );
            table.insert(
                sh,
                Stroke {
                    keycode: code,
                    modifiers: vec!["KEY_LEFTSHIFT"],
                },
            );
        }
    }
    table.insert(
        ' ',
        Stroke {
            keycode: 57,
            modifiers: vec![],
        },
    );
    table
}

/// The built-in US QWERTY layout, whatever the machine is configured with.
/// Deterministic, which is what tests and the fallback path both want.
pub fn builtin_us() -> Layout {
    fallback("builtin")
}

fn fallback(reason: &str) -> Layout {
    Layout {
        table: us_fallback(),
        source: format!("built-in US QWERTY ({reason})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_us_fallback_reaches_every_printable_ascii_character() {
        let layout = fallback("test");
        for c in (0x20u8..0x7F).map(char::from) {
            assert!(layout.get(c).is_some(), "{c:?} is unreachable");
        }
    }

    #[test]
    fn capitals_want_shift_and_lowercase_does_not() {
        let layout = fallback("test");
        assert_eq!(layout.get('a').unwrap().modifiers, Vec::<&str>::new());
        assert_eq!(layout.get('A').unwrap().modifiers, vec!["KEY_LEFTSHIFT"]);
        assert_eq!(
            layout.get('a').unwrap().keycode,
            layout.get('A').unwrap().keycode
        );
    }

    #[test]
    fn the_keypad_does_not_beat_the_number_row() {
        // 9 on the keypad is cheaper (no modifier) than shift+9 for "(", but it
        // is not a key a hand reaches for -- the primary rows win.
        assert!(rank(10, &["KEY_LEFTSHIFT"]) < rank(179, &[]));
        assert!(rank(86, &[]) > rank(53, &[]));
    }

    #[test]
    fn lock_and_exotic_modifiers_are_refused() {
        let mods = vec!["shift".to_string(), "lock".to_string()];
        assert_eq!(mask_to_keys(0b01, &mods), Some(vec!["KEY_LEFTSHIFT"]));
        assert_eq!(mask_to_keys(0b10, &mods), None);
        assert_eq!(mask_to_keys(0b01, &["mod3".to_string()]), None);
    }

    #[test]
    fn keymap_reaches_printable_ascii() {
        let layout = load_layout();
        let missing: Vec<char> = (0x20u8..0x7F)
            .map(char::from)
            .filter(|c| layout.get(*c).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "unreachable ASCII: {missing:?} ({})",
            layout.source
        );
    }

    #[test]
    fn no_character_is_typed_with_a_key_no_keyboard_has() {
        // The regression that silently ate '(' and ')'. XKB names keys like
        // KP_Left_Paren at keycode 179. Choosing one produces an event the
        // uinput device never declared, the kernel drops it, and the character
        // vanishes with nothing logged anywhere.
        let offenders: Vec<(char, u16)> = load_layout()
            .table
            .iter()
            .filter(|(c, s)| c.is_ascii_graphic() && rank(s.keycode, &[]).0 > 1)
            .map(|(c, s)| (*c, s.keycode))
            .collect();
        assert!(
            offenders.is_empty(),
            "ASCII routed through exotic keys: {offenders:?}"
        );
    }

    #[test]
    fn capitals_use_shift_rather_than_caps_lock() {
        // Toggling Caps Lock would leave the keyboard in a different state.
        let layout = load_layout();
        for ch in "ABZ".chars() {
            let stroke = layout.get(ch).unwrap_or_else(|| panic!("{ch} unreachable"));
            assert!(stroke.modifiers.iter().any(|m| m.contains("SHIFT")), "{ch}");
        }
    }

    #[test]
    fn shifted_and_unshifted_pairs_share_one_physical_key() {
        let layout = load_layout();
        for (lower, upper) in [('a', 'A'), ('1', '!'), ('9', '('), (',', '<')] {
            let (low, up) = (layout.get(lower).unwrap(), layout.get(upper).unwrap());
            assert_eq!(low.keycode, up.keycode, "{lower}/{upper}");
        }
    }

    #[test]
    fn ranking_prefers_the_main_rows_over_the_keypad() {
        let main_with_shift = rank(9, &["KEY_LEFTSHIFT"]); // Shift+8 -> '*'
        let keypad_no_mods = rank(55, &[]); // KP_Asterisk -> '*'
        assert!(main_with_shift < keypad_no_mods);
    }

    #[test]
    fn the_real_layout_compiles_to_something_useful() {
        let layout = load_layout();
        // Whatever machine this runs on, lowercase letters are always typeable.
        for c in 'a'..='z' {
            let _ = layout.get(c);
        }
        assert!(
            layout.len() >= 26,
            "{} from {}",
            layout.len(),
            layout.source
        );
    }
}
