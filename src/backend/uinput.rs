//! Linux backend: a virtual keyboard on /dev/uinput.
//!
//! XTEST -- what most auto-typers use -- is an X11 protocol extension, so under
//! Wayland it reaches only XWayland clients. Keystrokes aimed at a native
//! Wayland window are delivered to nothing at all, which looks exactly like
//! "laggy, drops characters, sometimes does nothing".
//!
//! A uinput device sits below all of that. The kernel presents it as a real
//! keyboard, so the compositor routes its events like any other keyboard, on
//! Wayland and X11 alike, and in a plain TTY.

use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::Duration;

use rand::Rng;

use super::codes::{self, KEY_CODES};
use super::identity::{self, Identity, BUS_USB};
use super::xkb::{self, Layout, Stroke};
use super::{Backend, BackendError};

/// The kernel creates the device instantly, but udev has to notice it and the
/// compositor has to add it to its seat. Typing into that window is the classic
/// uinput bug: the first characters vanish. Waiting is the whole fix.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(650);

/// Autorepeat, in milliseconds: how long a key must be held before the kernel
/// starts duplicating it, and how fast it then repeats. These are the kernel's
/// own defaults, which is what a keyboard nobody has reconfigured reports.
/// Declaring EV_REP is not cosmetic -- a device with no autorepeat at all is
/// not a keyboard -- but it does mean an over-long hold would type a character
/// twice, so the schedule caps hold time below the delay. See
/// [`crate::timing::MAX_DWELL`].
const REPEAT_DELAY_MS: i32 = 250;
const REPEAT_PERIOD_MS: i32 = 33;

// linux/input-event-codes.h
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_MSC: u16 = 0x04;
const EV_LED: u16 = 0x11;
const EV_REP: u16 = 0x14;
const MSC_SCAN: u16 = 0x04;
const LED_NUML: u16 = 0x00;
const LED_CAPSL: u16 = 0x01;
const LED_SCROLLL: u16 = 0x02;
const REP_DELAY: u16 = 0x00;
const REP_PERIOD: u16 = 0x01;
const SYN_REPORT: u16 = 0;

// linux/uinput.h, with the _IOC arithmetic spelled out.
const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;

const fn ioc(dir: u64, ty: u8, nr: u64, size: usize) -> libc::c_ulong {
    ((dir << 30) | ((size as u64) << 16) | ((ty as u64) << 8) | nr) as libc::c_ulong
}

const UI_DEV_CREATE: libc::c_ulong = ioc(IOC_NONE, b'U', 1, 0);
const UI_DEV_DESTROY: libc::c_ulong = ioc(IOC_NONE, b'U', 2, 0);
const UI_DEV_SETUP: libc::c_ulong = ioc(IOC_WRITE, b'U', 3, std::mem::size_of::<UinputSetup>());
const UI_SET_EVBIT: libc::c_ulong = ioc(IOC_WRITE, b'U', 100, std::mem::size_of::<libc::c_int>());
const UI_SET_KEYBIT: libc::c_ulong = ioc(IOC_WRITE, b'U', 101, std::mem::size_of::<libc::c_int>());
const UI_SET_MSCBIT: libc::c_ulong = ioc(IOC_WRITE, b'U', 104, std::mem::size_of::<libc::c_int>());
const UI_SET_PHYS: libc::c_ulong = ioc(
    IOC_WRITE,
    b'U',
    108,
    std::mem::size_of::<*const libc::c_char>(),
);
const UI_SET_LEDBIT: libc::c_ulong = ioc(IOC_WRITE, b'U', 105, std::mem::size_of::<libc::c_int>());
const fn ui_get_sysname(len: usize) -> libc::c_ulong {
    ioc(IOC_READ, b'U', 44, len)
}

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// Where the backend's events end up.
enum Output {
    Closed,
    Device(File),
    /// Test-only: keep the `(type, code, value)` triples instead of writing
    /// them, so the rules about the event stream are checked on every machine
    /// and not only where `/dev/uinput` happens to be writable.
    #[cfg(test)]
    Recorder(Vec<(u16, u16, i32)>),
}

/// Types by driving a virtual keyboard the kernel exposes for us.
pub struct UInputBackend {
    settle: Duration,
    hold: (f64, f64),
    layout: Option<Layout>,
    /// How the device presents itself to anything that inspects it. The default
    /// is modelled on a keyboard this machine actually has.
    pub identity: Identity,
    out: Output,
    advertised: HashSet<u16>,
    /// Keys currently held, by keycode. Several at once is normal.
    down: HashMap<u16, Stroke>,
    /// Modifier keycodes currently held on behalf of `down`.
    modifiers: Vec<u16>,
    /// Modifiers the *schedule* pressed rather than this backend. They are left
    /// alone when a key is released, because their own release is a scheduled
    /// event with its own time; see `sync_modifiers`.
    scheduled: HashSet<u16>,
    device_path: Option<String>,
}

impl Default for UInputBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl UInputBackend {
    pub fn new() -> Self {
        Self::with(DEFAULT_SETTLE, None, None)
    }

    pub fn with(settle: Duration, layout: Option<Layout>, identity: Option<Identity>) -> Self {
        Self {
            settle,
            hold: (0.010, 0.024),
            layout,
            identity: identity.unwrap_or_else(|| identity::for_this_machine(None)),
            out: Output::Closed,
            advertised: HashSet::new(),
            down: HashMap::new(),
            modifiers: Vec::new(),
            scheduled: HashSet::new(),
            device_path: None,
        }
    }

    /// Whether `/dev/uinput` can be opened for writing, and if not, why.
    pub fn available() -> (bool, String) {
        if !std::path::Path::new("/dev/uinput").exists() {
            return (
                false,
                "/dev/uinput does not exist (load the uinput module)".into(),
            );
        }
        match OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")
        {
            Ok(_) => (true, "ready".into()),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => (
                false,
                "no write permission on /dev/uinput (see README: udev rule)".into(),
            ),
            Err(e) => (false, format!("/dev/uinput could not be opened: {e}")),
        }
    }

    fn is_open(&self) -> bool {
        !matches!(self.out, Output::Closed)
    }

    fn require_open(&self) -> Result<(), BackendError> {
        if self.is_open() {
            Ok(())
        } else {
            Err(BackendError("backend is not open".into()))
        }
    }

    fn ioctl_int(fd: RawFd, request: libc::c_ulong, value: u16) -> std::io::Result<()> {
        // SAFETY: `fd` is an open uinput descriptor and these requests take an
        // int by value.
        let rc = unsafe { libc::ioctl(fd, request, libc::c_int::from(value)) };
        if rc < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Append one `input_event` to a buffer.
    fn push_event(buf: &mut Vec<u8>, ty: u16, code: u16, value: i32) {
        let ev = libc::input_event {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_: ty,
            code,
            value,
        };
        // SAFETY: `input_event` is a plain-old-data `repr(C)` struct.
        buf.extend_from_slice(unsafe {
            std::slice::from_raw_parts(
                (&ev as *const libc::input_event).cast::<u8>(),
                std::mem::size_of::<libc::input_event>(),
            )
        });
    }

    /// Send one key transition the way the hid driver sends one.
    ///
    /// Real hardware reports the raw scancode first and the translated key
    /// second, in a single report: `MSC_SCAN`, then `EV_KEY`, then the
    /// `SYN_REPORT` that closes both. Emitting only the key event is the single
    /// clearest way to tell a synthetic keyboard from a driven one, and it
    /// costs nothing to stop doing it.
    fn emit(&mut self, code: u16, value: i32) -> Result<(), BackendError> {
        let mut report: Vec<(u16, u16, i32)> = Vec::with_capacity(3);
        if let Some(scan) = codes::scancode(code) {
            report.push((EV_MSC, MSC_SCAN, scan as i32));
        }
        report.push((EV_KEY, code, value));
        report.push((EV_SYN, SYN_REPORT, 0));

        match &mut self.out {
            Output::Closed => Err(BackendError("backend is not open".into())),
            Output::Device(file) => {
                let mut buf =
                    Vec::with_capacity(report.len() * std::mem::size_of::<libc::input_event>());
                for (ty, code, value) in report {
                    Self::push_event(&mut buf, ty, code, value);
                }
                file.write_all(&buf)
                    .map_err(|e| BackendError(format!("write to /dev/uinput failed: {e}")))
            }
            #[cfg(test)]
            Output::Recorder(events) => {
                events.extend(report);
                Ok(())
            }
        }
    }

    /// The layout is a question about the keyboard, not the device, and it is
    /// asked before the device exists: the session compiles its schedule in its
    /// constructor and opens the backend only when the run starts.
    fn layout(&mut self) -> &Layout {
        self.layout.get_or_insert_with(xkb::load_layout)
    }

    /// The keycode and modifiers that produce `token`, or `None`.
    fn resolve(&self, token: &str) -> Option<Stroke> {
        if let Some(code) = codes::named_key_code(token) {
            return Some(Stroke {
                keycode: code,
                modifiers: vec![],
            });
        }
        // A modifier asked for by name: the schedule presses these itself, so
        // that Shift can lead the letter it modifies the way a hand does.
        if codes::is_modifier(token) {
            return Some(Stroke {
                keycode: codes::key_code(token)?,
                modifiers: vec![],
            });
        }
        let mut chars = token.chars();
        let (ch, None) = (chars.next()?, chars.next()) else {
            return None;
        };
        let stroke = self.layout.as_ref()?.get(ch)?;
        // Unreachable here; the caller falls back to the clipboard rather than
        // emitting an event the kernel would throw away.
        self.advertised
            .contains(&stroke.keycode)
            .then(|| stroke.clone())
    }

    /// Hold exactly the modifiers `names` asks for, and nothing else.
    ///
    /// The modifiers belong to the key *going down*, not to the set of keys
    /// held. That distinction is the whole correctness of rollover. A character
    /// is decided by the modifier state at the instant its key is pressed, so
    /// once a shifted key has registered, Shift has done its job and may lift
    /// even though the key itself is still down -- which is exactly what a hand
    /// does when it runs on into the next letter.
    ///
    /// Taking the union over every held key instead would keep Shift down for
    /// as long as any shifted key overlapped the next press, and quietly
    /// capitalise it: "Second" arrives as "SEcond".
    ///
    /// A run of capitals still holds one continuous Shift, because each press
    /// in the run asks for it and an already-held modifier is left alone.
    fn sync_modifiers(&mut self, names: &[&str], sweep: bool) -> Result<(), BackendError> {
        let mut wanted: Vec<u16> = Vec::new();
        for name in names {
            if let Some(code) = codes::key_code(name) {
                if !wanted.contains(&code) {
                    wanted.push(code);
                }
            }
        }

        let stale: Vec<u16> = self
            .modifiers
            .iter()
            .copied()
            .filter(|c| !wanted.contains(c))
            .collect();
        for code in stale {
            if sweep && self.scheduled.contains(&code) {
                // Its release is a scheduled event with a time of its own: a
                // modifier lingers past the key that needed it, exactly as a
                // hand does. Only a press that would be corrupted by it --
                // sweep=false -- overrules that.
                continue;
            }
            self.emit(code, 0)?;
            self.modifiers.retain(|c| *c != code);
            self.scheduled.remove(&code);
        }
        for code in wanted {
            if !self.modifiers.contains(&code) {
                self.emit(code, 1)?;
                self.modifiers.push(code);
            }
        }
        Ok(())
    }

    fn send_ctrl_v(&mut self) -> Result<(), BackendError> {
        let mut rng = rand::thread_rng();
        let hold = rng.gen_range(self.hold.0..=self.hold.1);
        self.emit(codes::KEY_LEFTCTRL, 1)?;
        self.emit(codes::KEY_V, 1)?;
        std::thread::sleep(Duration::from_secs_f64(hold));
        self.emit(codes::KEY_V, 0)?;
        self.emit(codes::KEY_LEFTCTRL, 0)
    }

    /// Resolve `/dev/input/eventN` for the device we just created.
    fn find_device_path(fd: RawFd) -> Option<String> {
        let mut buf = [0u8; 64];
        // SAFETY: UI_GET_SYSNAME writes a NUL-terminated name of at most `len`
        // bytes into the buffer we pass.
        let rc = unsafe { libc::ioctl(fd, ui_get_sysname(buf.len()), buf.as_mut_ptr()) };
        if rc < 0 {
            return None;
        }
        let sysname = CStr::from_bytes_until_nul(&buf)
            .ok()?
            .to_str()
            .ok()?
            .to_string();
        let dir = format!("/sys/devices/virtual/input/{sysname}");
        std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|n| n.starts_with("event"))
            .map(|n| format!("/dev/input/{n}"))
    }
}

impl Backend for UInputBackend {
    fn name(&self) -> &'static str {
        "uinput"
    }

    fn open(&mut self) -> Result<(), BackendError> {
        if self.is_open() {
            return Ok(());
        }
        let (ok, reason) = Self::available();
        if !ok {
            return Err(BackendError(reason));
        }
        self.layout();

        // Advertise every key we could conceivably send. A device that claims
        // only the keys in this particular text would be a different device
        // each run, which confuses compositors that cache seat capabilities.
        //
        // The layout's own keycodes are included as well: the kernel silently
        // discards an event for a key the device never declared, which is
        // indistinguishable from a dropped character. Declaring a superset
        // costs nothing and removes the failure mode.
        let mut advertised: HashSet<u16> = KEY_CODES.iter().map(|(_, c)| *c).collect();
        if let Some(layout) = &self.layout {
            advertised.extend(
                layout
                    .table
                    .values()
                    .map(|s| s.keycode)
                    .filter(|c| *c > 0 && *c < 256),
            );
        }
        let mut sorted: Vec<u16> = advertised.iter().copied().collect();
        sorted.sort_unstable();

        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")
            .map_err(|e| BackendError(format!("could not create virtual keyboard: {e}")))?;
        let fd = file.as_raw_fd();
        let fail = |what: &str, e: std::io::Error| {
            BackendError(format!("could not create virtual keyboard: {what}: {e}"))
        };

        // The capability bitmap is as much a part of looking like a keyboard as
        // the name is, and it lands in world-readable /proc/bus/input/devices:
        //
        //   EV_MSC  every key event is preceded by MSC_SCAN on real hardware
        //   EV_LED  the lock lights; every keyboard reports them, fitted or not
        //   EV_REP  autorepeat, which a device without a repeat rate cannot do
        //
        // ff_effects_max = 0 below keeps the EV_FF bit off, which would
        // advertise a keyboard with force feedback and no Caps Lock light -- a
        // device that has never existed.
        for ev in [EV_KEY, EV_MSC, EV_LED, EV_REP] {
            Self::ioctl_int(fd, UI_SET_EVBIT, ev).map_err(|e| fail("UI_SET_EVBIT", e))?;
        }
        for code in &sorted {
            Self::ioctl_int(fd, UI_SET_KEYBIT, *code).map_err(|e| fail("UI_SET_KEYBIT", e))?;
        }
        Self::ioctl_int(fd, UI_SET_MSCBIT, MSC_SCAN).map_err(|e| fail("UI_SET_MSCBIT", e))?;
        for led in [LED_NUML, LED_CAPSL, LED_SCROLLL] {
            Self::ioctl_int(fd, UI_SET_LEDBIT, led).map_err(|e| fail("UI_SET_LEDBIT", e))?;
        }

        let ident = &self.identity;
        let mut name = [0u8; 80];
        let bytes = ident.name.as_bytes();
        let n = bytes.len().min(79);
        name[..n].copy_from_slice(&bytes[..n]);
        let setup = UinputSetup {
            id: InputId {
                bustype: BUS_USB,
                vendor: ident.vendor,
                product: ident.product,
                version: ident.version,
            },
            name,
            ff_effects_max: 0,
        };
        // SAFETY: `setup` is a fully initialised `repr(C)` struct matching
        // `struct uinput_setup`, and outlives the call.
        if unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup) } < 0 {
            return Err(fail("UI_DEV_SETUP", std::io::Error::last_os_error()));
        }
        // The physical path is as much a part of the identity as the name: left
        // alone, the kernel reports it as empty, which no real keyboard does.
        let phys = CString::new(ident.phys.as_str()).unwrap_or_default();
        // SAFETY: UI_SET_PHYS reads a NUL-terminated string that outlives the call.
        if unsafe { libc::ioctl(fd, UI_SET_PHYS, phys.as_ptr()) } < 0 {
            return Err(fail("UI_SET_PHYS", std::io::Error::last_os_error()));
        }
        // SAFETY: UI_DEV_CREATE takes no argument.
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE) } < 0 {
            return Err(fail("UI_DEV_CREATE", std::io::Error::last_os_error()));
        }

        self.device_path = Self::find_device_path(fd);
        self.advertised = advertised;
        self.out = Output::Device(file);

        // A keyboard that declares autorepeat has to have a repeat rate; the
        // kernel leaves these at zero unless the device sets them, and zero is
        // a value no real keyboard reports.
        let mut buf = Vec::new();
        Self::push_event(&mut buf, EV_REP, REP_DELAY, REPEAT_DELAY_MS);
        Self::push_event(&mut buf, EV_REP, REP_PERIOD, REPEAT_PERIOD_MS);
        Self::push_event(&mut buf, EV_SYN, SYN_REPORT, 0);
        if let Output::Device(f) = &mut self.out {
            let _ = f.write_all(&buf);
        }

        std::thread::sleep(self.settle);
        Ok(())
    }

    fn close(&mut self) {
        if let Output::Device(file) = std::mem::replace(&mut self.out, Output::Closed) {
            // SAFETY: the descriptor is open until `file` drops at the end of scope.
            unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_DESTROY) };
        }
        self.out = Output::Closed;
        self.device_path = None;
    }

    /// Which modifiers this layout needs held to produce `token`.
    fn modifiers_for(&mut self, token: &str) -> Vec<String> {
        if codes::named_key_code(token).is_some() || codes::is_modifier(token) {
            return Vec::new();
        }
        let mut chars = token.chars();
        let (Some(ch), None) = (chars.next(), chars.next()) else {
            return Vec::new();
        };
        self.layout()
            .get(ch)
            .map(|s| s.modifiers.iter().map(|m| (*m).to_string()).collect())
            .unwrap_or_default()
    }

    fn key_down(&mut self, token: &str) -> Result<bool, BackendError> {
        self.require_open()?;
        let Some(stroke) = self.resolve(token) else {
            return Ok(false);
        };

        if codes::is_modifier(token) {
            // The schedule is pressing this one itself, ahead of the key it
            // modifies. Modifiers are tracked in `modifiers`, never in `down`:
            // one is a thing held on another key's behalf, the other is a key
            // producing a character, and conflating them is what lets a stale
            // Shift capitalise the next letter.
            self.scheduled.insert(stroke.keycode);
            if !self.modifiers.contains(&stroke.keycode) {
                self.modifiers.push(stroke.keycode);
                self.emit(stroke.keycode, 1)?;
            }
            return Ok(true);
        }

        // Pressing a key that is already down would strand it there forever;
        // the scheduler forbids it, and this makes the guarantee local.
        if self.down.remove(&stroke.keycode).is_some() {
            self.emit(stroke.keycode, 0)?;
        }

        // sweep=false: a modifier still held from an earlier run would change
        // this character into one the text never contained, so correctness wins
        // over the schedule's intent here and it goes regardless.
        self.sync_modifiers(&stroke.modifiers, false)?;
        self.down.insert(stroke.keycode, stroke.clone());
        self.emit(stroke.keycode, 1)?;
        Ok(true)
    }

    fn key_up(&mut self, token: &str) -> Result<bool, BackendError> {
        self.require_open()?;
        let Some(stroke) = self.resolve(token) else {
            return Ok(false);
        };
        if codes::is_modifier(token) {
            self.scheduled.remove(&stroke.keycode);
            if !self.modifiers.contains(&stroke.keycode) {
                return Ok(false); // already swept, by a press that could not keep it
            }
            self.modifiers.retain(|c| *c != stroke.keycode);
            self.emit(stroke.keycode, 0)?;
            return Ok(true);
        }
        if self.down.remove(&stroke.keycode).is_none() {
            return Ok(false);
        }
        self.emit(stroke.keycode, 0)?;
        // Modifiers are only ever re-synced by the next press, which knows what
        // it needs. Dropping them here would strand a run of capitals, and
        // re-deriving them from whatever is still held would press Shift with
        // no key under it. Once nothing is down there is nothing to hold for.
        if self.down.is_empty() {
            self.sync_modifiers(&[], true)?;
        }
        Ok(true)
    }

    /// Let go of everything. Called on every exit path, including failures.
    fn release_all(&mut self) {
        if !self.is_open() {
            return;
        }
        self.scheduled.clear();
        let held: Vec<u16> = self.down.keys().copied().collect();
        for code in held {
            let _ = self.emit(code, 0);
        }
        self.down.clear();
        // Then sweep every modifier unconditionally: a half-finished
        // sync_modifiers leaves the bookkeeping and the hardware disagreeing,
        // and a stuck Ctrl is far worse than a spare key-up.
        for code in codes::modifier_codes() {
            let _ = self.emit(code, 0);
        }
        self.modifiers.clear();
    }

    fn paste(&mut self, text: &str) -> bool {
        crate::clipboard::paste_via_clipboard(text, || self.send_ctrl_v().is_ok())
    }

    fn device_path(&self) -> Option<String> {
        self.device_path.clone()
    }

    fn owns_device_node(&self) -> bool {
        true
    }
}

impl Drop for UInputBackend {
    fn drop(&mut self) {
        // A backend dropped without close() must still not leave a key held.
        self.release_all();
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_kernel_headers() {
        assert_eq!(UI_DEV_CREATE, 0x5501);
        assert_eq!(UI_DEV_DESTROY, 0x5502);
        assert_eq!(UI_SET_EVBIT, 0x4004_5564);
        assert_eq!(UI_SET_KEYBIT, 0x4004_5565);
        assert_eq!(UI_DEV_SETUP, 0x405c_5503);
        assert_eq!(UI_SET_PHYS, 0x4008_556c);
        assert_eq!(ui_get_sysname(64), 0x8040_552c);
    }

    #[test]
    fn the_setup_struct_matches_the_kernel_abi() {
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
    }

    #[test]
    fn events_are_the_kernel_size() {
        let mut buf = Vec::new();
        UInputBackend::push_event(&mut buf, EV_KEY, 30, 1);
        assert_eq!(buf.len(), std::mem::size_of::<libc::input_event>());
        assert_eq!(buf.len(), 24);
    }

    /// A backend wired to a recorder instead of a device, with a stub layout.
    fn wired(table: HashMap<char, Stroke>) -> UInputBackend {
        let mut b = UInputBackend::with(
            Duration::ZERO,
            Some(Layout {
                table,
                source: "test".into(),
            }),
            Some(identity::for_this_machine(Some("Test Board"))),
        );
        b.advertised = KEY_CODES.iter().map(|(_, c)| *c).collect();
        b.out = Output::Recorder(Vec::new());
        b
    }

    /// The `(code, value)` of every key event recorded so far.
    fn key_events(b: &UInputBackend) -> Vec<(u16, i32)> {
        match &b.out {
            Output::Recorder(events) => events
                .iter()
                .filter(|(ty, _, _)| *ty == EV_KEY)
                .map(|(_, code, value)| (*code, *value))
                .collect(),
            _ => panic!("not a recorder"),
        }
    }

    fn stroke(code: u16, shifted: bool) -> Stroke {
        Stroke {
            keycode: code,
            modifiers: if shifted {
                vec!["KEY_LEFTSHIFT"]
            } else {
                vec![]
            },
        }
    }

    fn ab_layout() -> HashMap<char, Stroke> {
        let (a, b) = (
            codes::key_code("KEY_A").unwrap(),
            codes::key_code("KEY_B").unwrap(),
        );
        HashMap::from([
            ('a', stroke(a, false)),
            ('A', stroke(a, true)),
            ('b', stroke(b, false)),
            ('B', stroke(b, true)),
        ])
    }

    /// Decode recorded events the way the kernel and compositor would. A
    /// character is whatever the modifier state says it is at the instant its
    /// key goes down -- which is the whole point being tested.
    fn typed(events: &[(u16, i32)]) -> String {
        let (shift, a, b) = (
            codes::KEY_LEFTSHIFT,
            codes::key_code("KEY_A").unwrap(),
            codes::key_code("KEY_B").unwrap(),
        );
        let (mut held, mut out) = (false, String::new());
        for &(code, value) in events {
            if code == shift {
                held = value == 1;
            } else if value == 1 {
                let base = if code == a {
                    'a'
                } else if code == b {
                    'b'
                } else {
                    '?'
                };
                out.push(if held {
                    base.to_ascii_uppercase()
                } else {
                    base
                });
            }
        }
        out
    }

    // -- modifiers belong to the press, not to the hold ---------------------
    #[test]
    fn a_shifted_key_rolling_over_does_not_capitalise_the_next() {
        // The regression that turned "Second" into "SEcond". The engine overlaps
        // keystrokes on purpose, so 'A' is still physically held when 'b' goes
        // down. Shift belongs to the press that needed it: deriving it from
        // everything still held keeps it down over 'b' and silently capitalises
        // it, and no error is raised anywhere.
        let mut b = wired(ab_layout());
        b.key_down("A").unwrap();
        b.key_down("b").unwrap(); // rolls over: 'A' has not been released yet
        b.key_up("A").unwrap();
        b.key_up("b").unwrap();
        assert_eq!(typed(&key_events(&b)), "Ab");
    }

    #[test]
    fn an_unshifted_key_rolling_over_does_not_lowercase_the_next() {
        let mut b = wired(ab_layout());
        b.key_down("a").unwrap();
        b.key_down("B").unwrap();
        b.key_up("a").unwrap();
        b.key_up("B").unwrap();
        assert_eq!(typed(&key_events(&b)), "aB");
    }

    #[test]
    fn a_run_of_capitals_holds_one_continuous_shift() {
        let mut b = wired(ab_layout());
        b.key_down("A").unwrap();
        b.key_down("B").unwrap();
        b.key_up("A").unwrap();
        b.key_up("B").unwrap();
        let events = key_events(&b);
        assert_eq!(typed(&events), "AB");
        let shift: Vec<i32> = events
            .iter()
            .filter(|(c, _)| *c == codes::KEY_LEFTSHIFT)
            .map(|(_, v)| *v)
            .collect();
        assert_eq!(shift, [1, 0]);
    }

    #[test]
    fn nothing_is_left_held_once_every_key_is_released() {
        let mut b = wired(ab_layout());
        for token in ["A", "b", "B", "a"] {
            assert!(b.type_char(token).unwrap());
        }
        assert!(b.down.is_empty() && b.modifiers.is_empty());
        let mut last: HashMap<u16, i32> = HashMap::new();
        for (code, value) in key_events(&b) {
            last.insert(code, value);
        }
        assert!(
            last.values().all(|v| *v == 0),
            "a key was left down: {last:?}"
        );
    }

    #[test]
    fn a_scheduled_shift_outlives_the_key_that_needed_it() {
        // The schedule presses Shift itself so it can lead the letter and linger
        // after it; the backend must not sweep it out from under the schedule.
        let mut b = wired(ab_layout());
        assert!(b.key_down("KEY_LEFTSHIFT").unwrap());
        b.key_down("A").unwrap();
        b.key_up("A").unwrap();
        let shift_events = |b: &UInputBackend| {
            key_events(b)
                .iter()
                .filter(|(c, _)| *c == codes::KEY_LEFTSHIFT)
                .count()
        };
        assert_eq!(shift_events(&b), 1, "only the scheduled press so far");
        assert!(b.key_up("KEY_LEFTSHIFT").unwrap());
        assert_eq!(shift_events(&b), 2);
        assert!(!b.key_up("KEY_LEFTSHIFT").unwrap(), "already released");
    }

    #[test]
    fn a_stale_shift_is_dropped_by_a_press_that_does_not_want_it() {
        let mut b = wired(ab_layout());
        b.key_down("KEY_LEFTSHIFT").unwrap();
        b.key_down("b").unwrap(); // 'b' wants no Shift, so correctness wins
        assert_eq!(typed(&key_events(&b)), "b");
    }

    #[test]
    fn pressing_a_held_key_again_releases_it_first() {
        let mut b = wired(ab_layout());
        b.key_down("a").unwrap();
        b.key_down("a").unwrap();
        let a = codes::key_code("KEY_A").unwrap();
        let on_a: Vec<i32> = key_events(&b)
            .into_iter()
            .filter(|(c, _)| *c == a)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(on_a, [1, 0, 1]);
    }

    #[test]
    fn release_all_lets_go_of_everything_even_what_it_lost_track_of() {
        let mut b = wired(ab_layout());
        b.key_down("A").unwrap();
        b.key_down("b").unwrap();
        b.release_all();
        let events = key_events(&b);
        let mut last: HashMap<u16, i32> = HashMap::new();
        for (code, value) in &events {
            last.insert(*code, *value);
        }
        assert!(last.values().all(|v| *v == 0), "{last:?}");
        // Every modifier is swept unconditionally, held or not.
        for code in codes::modifier_codes() {
            assert_eq!(last.get(&code), Some(&0), "modifier {code} not swept");
        }
    }

    #[test]
    fn a_character_the_layout_cannot_reach_is_refused_not_faked() {
        let mut b = wired(ab_layout());
        assert!(!b.key_down("z").unwrap());
        assert!(!b.key_up("z").unwrap());
        assert!(key_events(&b).is_empty());
    }

    #[test]
    fn a_key_the_device_never_declared_is_refused() {
        // The kernel throws away an event for a key the device did not declare,
        // which is indistinguishable from a dropped character.
        let mut b = wired(ab_layout());
        b.advertised.remove(&codes::key_code("KEY_B").unwrap());
        assert!(!b.key_down("b").unwrap());
        assert!(b.key_down("a").unwrap());
    }

    #[test]
    fn named_keys_press_their_own_codes() {
        let mut b = wired(ab_layout());
        for (name, code) in [
            ("enter", codes::KEY_ENTER),
            ("tab", codes::KEY_TAB),
            ("backspace", codes::KEY_BACKSPACE),
        ] {
            assert!(b.type_char(name).unwrap(), "{name}");
            assert!(key_events(&b).contains(&(code, 1)), "{name}");
        }
    }

    // -- the event stream ---------------------------------------------------
    #[test]
    fn each_key_event_is_preceded_by_its_scancode() {
        // Real hardware reports the raw scancode, then the translated key.
        let mut b = wired(ab_layout());
        let a = codes::key_code("KEY_A").unwrap();
        b.emit(a, 1).unwrap();
        b.emit(a, 0).unwrap();
        let Output::Recorder(events) = &b.out else {
            panic!()
        };
        assert_eq!(
            events,
            &[
                (EV_MSC, MSC_SCAN, 0x70004),
                (EV_KEY, a, 1),
                (EV_SYN, SYN_REPORT, 0),
                (EV_MSC, MSC_SCAN, 0x70004),
                (EV_KEY, a, 0),
                (EV_SYN, SYN_REPORT, 0),
            ]
        );
    }

    #[test]
    fn no_hold_ever_reaches_the_autorepeat_delay() {
        // A device that declares autorepeat will use it. EV_REP has to be
        // declared -- a keyboard that cannot repeat is not one -- so a hold past
        // the kernel's delay would insert a character the source text never
        // contained.
        assert!(crate::timing::MAX_DWELL < f64::from(REPEAT_DELAY_MS) / 1000.0);
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(40);
        for wpm in [15.0, 70.0, 200.0] {
            let cadence = crate::timing::Cadence {
                wpm,
                ..Default::default()
            };
            let events = crate::timing::plan(&text, &cadence, Some(3), None).unwrap();
            let holds = crate::timing::stroke_stats(&events).dwell;
            let longest = holds.iter().cloned().fold(0.0, f64::max);
            assert!(
                longest <= crate::timing::MAX_DWELL + 1e-9,
                "{wpm}: {longest}"
            );
        }
    }

    #[test]
    fn a_session_schedules_shift_for_the_capitals_in_its_text() {
        // End to end through the session constructor, which is where the
        // "Shift is silently pressed with its letter" bug lived: the schedule
        // is planned before the device is opened.
        use crate::session::{SessionOptions, TypeSession};
        let session = TypeSession::new(
            "Hello World!",
            &crate::timing::Cadence::default(),
            Box::new(UInputBackend::with(
                Duration::ZERO,
                Some(xkb::load_layout()),
                Some(identity::for_this_machine(Some("Test Board"))),
            )),
            SessionOptions {
                countdown: 0.0,
                panic_key: None,
                ..Default::default()
            },
        )
        .unwrap();
        let modifiers = session
            .events
            .iter()
            .filter(|e| codes::is_modifier(&e.value))
            .count();
        // H, W and ! each need Shift: three presses and three releases.
        assert_eq!(
            modifiers,
            6,
            "{:?}",
            session.events.iter().map(|e| &e.value).collect::<Vec<_>>()
        );
    }

    fn closed_backend() -> UInputBackend {
        UInputBackend::with(
            Duration::ZERO,
            Some(xkb::load_layout()),
            Some(identity::for_this_machine(Some("Test Board"))),
        )
    }

    #[test]
    fn using_a_closed_backend_is_an_error_not_a_panic() {
        let mut b = closed_backend();
        assert!(b.key_down("a").is_err());
        assert!(b.key_up("a").is_err());
        b.release_all();
        b.close();
        b.close();
    }

    #[test]
    fn modifier_questions_work_before_the_device_exists() {
        let mut b = closed_backend();
        let shifted = b.modifiers_for("A");
        let plain = b.modifiers_for("a");
        assert!(plain.is_empty());
        assert_eq!(shifted, vec!["KEY_LEFTSHIFT".to_string()]);
        assert!(b.modifiers_for("enter").is_empty());
        assert!(b.modifiers_for("KEY_LEFTSHIFT").is_empty());
    }
}
