//! Linux fallback: XTEST.
//!
//! Only correct on a real X11 session. Under Wayland this still "works" in the
//! sense that `XOpenDisplay` succeeds -- XWayland is running -- but the
//! keystrokes reach XWayland clients only. So the probe reports the session
//! type and refuses to run under Wayland at all, by name or otherwise.
//!
//! The keycode table is shared with the uinput backend: X11 keycodes are evdev
//! scancodes plus 8, so one XKB layout serves both.
//!
//! This backend deliberately does not implement `modifiers_for`, so Shift is
//! pressed with the key it modifies rather than scheduled ahead of it as the
//! uinput backend does. Not an oversight: XTEST announces itself a layer below
//! anything keystroke timing could fix -- an X client watching through XRecord
//! sees the injection itself -- so giving Shift a human lead here would buy
//! nothing and would duplicate the trickiest state machine in the program into
//! the one path that cannot be exercised from a Wayland session. Use uinput.

use std::collections::HashSet;
use std::ffi::{c_int, c_uchar, c_uint, c_ulong, c_void};
use std::time::Duration;

use rand::Rng;

use super::xkb::{self, Layout};
use super::{Backend, BackendError};

const XKB_EVDEV_OFFSET: u16 = 8;

/// Keysyms for the modifiers we may need to hold.
fn mod_keysyms(name: &str) -> &'static [u64] {
    match name {
        "KEY_LEFTSHIFT" => &[0xFFE1],
        "KEY_LEFTCTRL" => &[0xFFE3],
        "KEY_LEFTALT" => &[0xFFE9],
        "KEY_RIGHTALT" => &[0xFE03, 0xFFEA], // ISO_Level3_Shift, then Alt_R
        "KEY_LEFTMETA" => &[0xFFEB],
        _ => &[],
    }
}

const MODIFIER_NAMES: [&str; 5] = [
    "KEY_LEFTSHIFT",
    "KEY_LEFTCTRL",
    "KEY_LEFTALT",
    "KEY_RIGHTALT",
    "KEY_LEFTMETA",
];

fn named_keysym(token: &str) -> Option<u64> {
    Some(match token {
        "enter" => 0xFF0D,
        "tab" => 0xFF09,
        "backspace" => 0xFF08,
        "escape" => 0xFF1B,
        "space" => 0x0020,
        _ => return None,
    })
}

/// The two Xlib calls the backend needs, so the logic above them can be tested
/// without an X server.
trait Xlib: Send {
    fn fake_key(&mut self, keycode: u8, press: bool);
    fn keysym_to_keycode(&mut self, keysym: u64) -> Option<u8>;
}

/// The real thing: libX11 and libXtst, loaded at run time.
struct Display {
    _x11: libloading::Library,
    _xtst: libloading::Library,
    display: *mut c_void,
    flush: unsafe extern "C" fn(*mut c_void) -> c_int,
    close: unsafe extern "C" fn(*mut c_void) -> c_int,
    keysym_to_keycode: unsafe extern "C" fn(*mut c_void, c_ulong) -> c_uchar,
    fake_key_event: unsafe extern "C" fn(*mut c_void, c_uint, c_int, c_ulong) -> c_int,
}

// SAFETY: the display pointer is only ever used from the one thread that owns
// the backend at a time; Xlib is not shared across threads here.
unsafe impl Send for Display {}

fn load_libs() -> Result<(libloading::Library, libloading::Library), String> {
    // SAFETY: loading libX11/libXtst runs their initialisers, which have no
    // side effects beyond registering the library.
    unsafe {
        let x11 = ["libX11.so.6", "libX11.so"]
            .iter()
            .find_map(|n| libloading::Library::new(n).ok())
            .ok_or("libX11 not found")?;
        let xtst = ["libXtst.so.6", "libXtst.so"]
            .iter()
            .find_map(|n| libloading::Library::new(n).ok())
            .ok_or("libXtst not found (install libxtst)")?;
        Ok((x11, xtst))
    }
}

impl Display {
    fn open() -> Result<Self, BackendError> {
        let (x11, xtst) =
            load_libs().map_err(|e| BackendError(format!("libX11 or libXtst is missing: {e}")))?;
        // SAFETY: each symbol is cast to the signature in <X11/Xlib.h> and
        // <X11/extensions/XTest.h>; the display is closed in `Drop`.
        unsafe {
            let open: unsafe extern "C" fn(*const std::ffi::c_char) -> *mut c_void = *x11
                .get(b"XOpenDisplay\0")
                .map_err(|e| BackendError(format!("could not load X libraries: {e}")))?;
            let display = open(std::ptr::null());
            if display.is_null() {
                return Err(BackendError("could not open the X display".into()));
            }
            let sym =
                |e: libloading::Error| BackendError(format!("could not load X libraries: {e}"));
            Ok(Self {
                flush: *x11.get(b"XFlush\0").map_err(sym)?,
                close: *x11.get(b"XCloseDisplay\0").map_err(sym)?,
                keysym_to_keycode: *x11.get(b"XKeysymToKeycode\0").map_err(sym)?,
                fake_key_event: *xtst.get(b"XTestFakeKeyEvent\0").map_err(sym)?,
                display,
                _x11: x11,
                _xtst: xtst,
            })
        }
    }
}

impl Xlib for Display {
    fn fake_key(&mut self, keycode: u8, press: bool) {
        // SAFETY: `display` is open until `Drop`.
        unsafe {
            (self.fake_key_event)(self.display, c_uint::from(keycode), c_int::from(press), 0);
            (self.flush)(self.display);
        }
    }

    fn keysym_to_keycode(&mut self, keysym: u64) -> Option<u8> {
        // SAFETY: `display` is open until `Drop`.
        let code = unsafe { (self.keysym_to_keycode)(self.display, keysym as c_ulong) };
        (code != 0).then_some(code)
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        // SAFETY: closing the display we opened, once.
        unsafe { (self.close)(self.display) };
    }
}

/// Types through the X11 XTEST extension.
pub struct X11Backend {
    hold: (f64, f64),
    layout: Option<Layout>,
    xlib: Option<Box<dyn Xlib>>,
    /// X keycodes currently held. Several at once is normal.
    down: HashSet<u8>,
    modifiers: Vec<u8>,
}

impl Default for X11Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl X11Backend {
    pub fn new() -> Self {
        Self {
            hold: (0.010, 0.024),
            layout: None,
            xlib: None,
            down: HashSet::new(),
            modifiers: Vec::new(),
        }
    }

    pub fn available() -> (bool, String) {
        if std::env::var_os("DISPLAY").is_none_or(|d| d.is_empty()) {
            return (false, "DISPLAY is not set".into());
        }
        if let Err(e) = load_libs() {
            return (false, e);
        }
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland")) {
            return (
                false,
                "session is Wayland \u{2014} XTEST reaches XWayland clients only; use the uinput backend"
                    .into(),
            );
        }
        (true, "ready".into())
    }

    #[cfg(test)]
    fn with_xlib(xlib: Box<dyn Xlib>, layout: Layout) -> Self {
        let mut backend = Self::new();
        backend.xlib = Some(xlib);
        backend.layout = Some(layout);
        backend
    }

    fn x(&mut self) -> Result<&mut Box<dyn Xlib>, BackendError> {
        self.xlib
            .as_mut()
            .ok_or_else(|| BackendError("backend is not open".into()))
    }

    fn modifier_keycode(&mut self, evdev_name: &str) -> Option<u8> {
        let x = self.xlib.as_mut()?;
        mod_keysyms(evdev_name)
            .iter()
            .find_map(|ks| x.keysym_to_keycode(*ks))
    }

    /// The X keycode and modifier names that produce `token`.
    fn resolve(&mut self, token: &str) -> Option<(u8, Vec<&'static str>)> {
        if let Some(keysym) = named_keysym(token) {
            return self
                .xlib
                .as_mut()?
                .keysym_to_keycode(keysym)
                .map(|c| (c, Vec::new()));
        }
        let mut chars = token.chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else {
            return None;
        };
        let stroke = self.layout.as_ref()?.get(ch)?;
        let code = u8::try_from(stroke.keycode + XKB_EVDEV_OFFSET).ok()?;
        Some((code, stroke.modifiers.clone()))
    }

    /// Hold exactly the modifiers `names` asks for, and nothing else.
    ///
    /// As in the uinput backend, the modifiers belong to the key going down
    /// rather than to the set of keys held: a character is fixed by the
    /// modifier state at the instant of its press, so Shift may lift as soon as
    /// the press has landed even while the key itself rolls over into the next
    /// one. Deriving them from everything still held would capitalise whatever
    /// followed a shifted key.
    fn sync_modifiers(&mut self, names: &[&str]) -> Result<(), BackendError> {
        let mut wanted: Vec<u8> = Vec::new();
        for name in names {
            if let Some(code) = self.modifier_keycode(name) {
                if !wanted.contains(&code) {
                    wanted.push(code);
                }
            }
        }
        let stale: Vec<u8> = self
            .modifiers
            .iter()
            .copied()
            .filter(|c| !wanted.contains(c))
            .collect();
        for code in stale {
            self.x()?.fake_key(code, false);
            self.modifiers.retain(|c| *c != code);
        }
        for code in wanted {
            if !self.modifiers.contains(&code) {
                self.x()?.fake_key(code, true);
                self.modifiers.push(code);
            }
        }
        Ok(())
    }

    fn send_ctrl_v(&mut self) -> bool {
        let ctrl = self.modifier_keycode("KEY_LEFTCTRL");
        let v = self.xlib.as_mut().and_then(|x| x.keysym_to_keycode(0x0076));
        let (Some(ctrl), Some(v)) = (ctrl, v) else {
            return false;
        };
        let hold = rand::thread_rng().gen_range(self.hold.0..=self.hold.1);
        let Some(x) = self.xlib.as_mut() else {
            return false;
        };
        x.fake_key(ctrl, true);
        x.fake_key(v, true);
        std::thread::sleep(Duration::from_secs_f64(hold));
        x.fake_key(v, false);
        x.fake_key(ctrl, false);
        true
    }
}

impl Backend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn open(&mut self) -> Result<(), BackendError> {
        if self.xlib.is_some() {
            return Ok(());
        }
        self.xlib = Some(Box::new(Display::open()?));
        if self.layout.is_none() {
            self.layout = Some(xkb::load_layout());
        }
        Ok(())
    }

    fn close(&mut self) {
        self.xlib = None;
    }

    fn key_down(&mut self, token: &str) -> Result<bool, BackendError> {
        self.x()?;
        let Some((keycode, modifiers)) = self.resolve(token) else {
            return Ok(false);
        };
        // Pressing a key that is already down would strand it there forever.
        if self.down.remove(&keycode) {
            self.x()?.fake_key(keycode, false);
        }
        self.down.insert(keycode);
        self.sync_modifiers(&modifiers)?; // modifiers first, then the key
        self.x()?.fake_key(keycode, true);
        Ok(true)
    }

    fn key_up(&mut self, token: &str) -> Result<bool, BackendError> {
        self.x()?;
        let Some((keycode, _)) = self.resolve(token) else {
            return Ok(false);
        };
        if !self.down.remove(&keycode) {
            return Ok(false);
        }
        self.x()?.fake_key(keycode, false);
        // The next press re-syncs to what it needs; dropping the modifiers here
        // would break up a run of capitals for no gain.
        if self.down.is_empty() {
            self.sync_modifiers(&[])?;
        }
        Ok(true)
    }

    fn release_all(&mut self) {
        if self.xlib.is_none() {
            return;
        }
        let held: Vec<u8> = self.down.drain().collect();
        if let Some(x) = self.xlib.as_mut() {
            for code in held {
                x.fake_key(code, false);
            }
        }
        for name in MODIFIER_NAMES {
            if let Some(code) = self.modifier_keycode(name) {
                if let Some(x) = self.xlib.as_mut() {
                    x.fake_key(code, false);
                }
            }
        }
        self.modifiers.clear();
    }

    fn paste(&mut self, text: &str) -> bool {
        crate::clipboard::paste_via_clipboard(text, || self.send_ctrl_v())
    }
}

impl Drop for X11Backend {
    fn drop(&mut self) {
        self.release_all();
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// An "X server" that records key events and maps keysyms to fixed keycodes.
    struct Fake(Arc<Mutex<Vec<(u8, bool)>>>);

    impl Xlib for Fake {
        fn fake_key(&mut self, keycode: u8, press: bool) {
            self.0.lock().unwrap().push((keycode, press));
        }
        fn keysym_to_keycode(&mut self, keysym: u64) -> Option<u8> {
            Some(match keysym {
                0xFFE1 => 50, // Shift_L
                0xFFE3 => 37, // Control_L
                0xFF0D => 36, // Return
                0xFF08 => 22, // BackSpace
                0xFF09 => 23, // Tab
                0x0076 => 55, // v
                _ => return None,
            })
        }
    }

    type Log = Arc<Mutex<Vec<(u8, bool)>>>;

    fn rig() -> (X11Backend, Log) {
        let events = Arc::new(Mutex::new(Vec::new()));
        (
            X11Backend::with_xlib(Box::new(Fake(events.clone())), xkb::builtin_us()),
            events,
        )
    }

    // X keycodes are evdev codes plus 8: KEY_A is evdev 30, so X 38.
    const A: u8 = 38;

    #[test]
    fn a_character_goes_down_and_up_as_one_x_keycode() {
        let (mut b, ev) = rig();
        assert!(b.type_char("a").unwrap());
        assert_eq!(*ev.lock().unwrap(), [(A, true), (A, false)]);
    }

    #[test]
    fn shift_is_pressed_before_its_key_and_released_after_the_run() {
        let (mut b, ev) = rig();
        assert!(b.key_down("A").unwrap());
        assert!(b.key_up("A").unwrap());
        assert_eq!(
            *ev.lock().unwrap(),
            [(50, true), (A, true), (A, false), (50, false)]
        );
    }

    #[test]
    fn a_shifted_key_rolling_over_does_not_capitalise_the_next() {
        let (mut b, ev) = rig();
        b.key_down("A").unwrap();
        b.key_down("b").unwrap(); // 'A' still held
        b.key_up("A").unwrap();
        b.key_up("b").unwrap();
        let events = ev.lock().unwrap();
        let b_down = events
            .iter()
            .position(|e| *e == (56, true))
            .expect("b pressed");
        let shift_up = events
            .iter()
            .position(|e| *e == (50, false))
            .expect("shift released");
        assert!(
            shift_up < b_down,
            "Shift must be gone before 'b' goes down: {events:?}"
        );
    }

    #[test]
    fn a_run_of_capitals_holds_one_continuous_shift() {
        let (mut b, ev) = rig();
        b.key_down("A").unwrap();
        b.key_down("B").unwrap();
        b.key_up("A").unwrap();
        b.key_up("B").unwrap();
        let shift: Vec<bool> = ev
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| *c == 50)
            .map(|(_, p)| *p)
            .collect();
        assert_eq!(shift, [true, false]);
    }

    #[test]
    fn pressing_a_held_key_again_releases_it_first() {
        let (mut b, ev) = rig();
        b.key_down("a").unwrap();
        b.key_down("a").unwrap();
        let on_a: Vec<bool> = ev
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| *c == A)
            .map(|(_, p)| *p)
            .collect();
        assert_eq!(on_a, [true, false, true]);
    }

    #[test]
    fn named_keys_use_their_keysyms() {
        let (mut b, ev) = rig();
        assert!(b.type_char("enter").unwrap());
        assert!(b.type_char("backspace").unwrap());
        assert_eq!(
            *ev.lock().unwrap(),
            [(36, true), (36, false), (22, true), (22, false)]
        );
        assert!(
            !b.key_down("escape").unwrap(),
            "no keycode for it on this fake server"
        );
    }

    #[test]
    fn release_all_lets_go_of_keys_and_modifiers() {
        let (mut b, ev) = rig();
        b.key_down("A").unwrap();
        ev.lock().unwrap().clear();
        b.release_all();
        let events = ev.lock().unwrap();
        assert!(
            events.contains(&(A, false))
                && events.contains(&(50, false))
                && events.contains(&(37, false))
        );
        assert!(events.iter().all(|(_, press)| !press));
    }

    #[test]
    fn a_closed_backend_is_an_error() {
        let mut b = X11Backend::new();
        assert!(b.key_down("a").is_err());
        b.release_all();
        b.close();
    }

    #[test]
    fn it_does_not_schedule_modifiers() {
        let (mut b, _ev) = rig();
        assert!(b.modifiers_for("A").is_empty());
    }

    #[test]
    fn an_unreachable_character_is_refused() {
        let (mut b, ev) = rig();
        assert!(!b.key_down("\u{1F600}").unwrap());
        assert!(ev.lock().unwrap().is_empty());
    }

    #[test]
    fn the_probe_always_explains_itself() {
        let (ok, why) = X11Backend::available();
        assert!(!why.is_empty());
        assert_eq!(ok, why == "ready");
    }

    #[test]
    fn wayland_is_never_volunteered() {
        // Only meaningful when this process happens to be under Wayland.
        if std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s.eq_ignore_ascii_case("wayland")) {
            let (ok, why) = X11Backend::available();
            assert!(!ok);
            assert!(!why.is_empty());
        }
    }
}
