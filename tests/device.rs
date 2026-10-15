//! End-to-end tests against real kernel input devices.
//!
//! The first test needs only a writable `/dev/uinput` and emits no key events.
//! The rest read the virtual keyboard back from `/dev/input`, so they also need
//! read access there (the `input` group) and the ability to *grab* the device,
//! which is what keeps their keystrokes from reaching whatever has focus. Each
//! skips itself, loudly, when it cannot do that; none will ever type into your
//! desktop.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use openautotyper::backend::codes::{self, KEY_CODES};
use openautotyper::backend::identity;
use openautotyper::backend::xkb::{self, Layout};
use openautotyper::backend::{Backend, UInputBackend};
use openautotyper::session::{SessionOptions, State, TypeSession};
use openautotyper::timing::{plan, Action, Cadence};

const EV_KEY: u16 = 1;

fn uinput_or_skip(test: &str) -> bool {
    let (ok, why) = UInputBackend::available();
    if !ok {
        eprintln!("SKIPPED {test}: uinput unusable: {why}");
    }
    ok
}

// -- reading and grabbing the virtual device ----------------------------------
#[repr(C)]
#[derive(Clone, Copy)]
struct Raw {
    tv_sec: i64,
    tv_usec: i64,
    ty: u16,
    code: u16,
    value: i32,
}

struct Seen {
    ty: u16,
    code: u16,
    value: i32,
    at: f64,
}

/// `EVIOCGRAB`: _IOW('E', 0x90, int)
const EVIOCGRAB: libc::c_ulong = (1 << 30) | (4 << 16) | ((b'E' as libc::c_ulong) << 8) | 0x90;

/// A grabbed, background-read mirror of a virtual keyboard.
struct Mirror {
    seen: Arc<Mutex<Vec<Seen>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Mirror {
    fn attach(path: &str) -> Result<Self, String> {
        let file: File = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| format!("cannot read {path}: {e}"))?;
        // SAFETY: EVIOCGRAB takes an int by value.
        if unsafe { libc::ioctl(file.as_raw_fd(), EVIOCGRAB, 1) } < 0 {
            return Err(format!(
                "cannot grab {path}: {}",
                std::io::Error::last_os_error()
            ));
        }
        let seen = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (sink, flag) = (seen.clone(), stop.clone());
        let origin = Instant::now();
        let thread = std::thread::spawn(move || {
            const SIZE: usize = std::mem::size_of::<Raw>();
            let mut buf = vec![0u8; SIZE * 64];
            while !flag.load(Ordering::SeqCst) {
                // SAFETY: reading into a buffer we own, at most its length.
                let n = unsafe { libc::read(file.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                }
                let at = origin.elapsed().as_secs_f64();
                for chunk in buf[..n as usize].as_chunks::<SIZE>().0 {
                    // SAFETY: each chunk is exactly one input_event.
                    let ev: Raw = unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
                    sink.lock().unwrap().push(Seen {
                        ty: ev.ty,
                        code: ev.code,
                        value: ev.value,
                        at,
                    });
                }
            }
            // Dropping `file` releases the grab.
        });
        Ok(Self {
            seen,
            stop,
            thread: Some(thread),
        })
    }

    fn finish(mut self) -> Vec<Seen> {
        std::thread::sleep(Duration::from_millis(300));
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        std::mem::take(&mut *self.seen.lock().unwrap())
    }
}

/// Open a backend and grab its node, or say why not. The grab happens before any
/// key event exists, so nothing can have escaped to the desktop.
fn grabbed(test: &str, settle: Duration) -> Option<(UInputBackend, Mirror)> {
    if !uinput_or_skip(test) {
        return None;
    }
    let mut backend = UInputBackend::with(settle, None, None);
    backend.open().expect("open uinput backend");
    let Some(path) = backend.device_path() else {
        eprintln!("SKIPPED {test}: could not locate the virtual device node");
        return None;
    };
    match Mirror::attach(&path) {
        Ok(mirror) => Some((backend, mirror)),
        Err(why) => {
            eprintln!("SKIPPED {test}: {why}");
            None
        }
    }
}

/// Turn a captured evdev stream back into the text it represents.
fn decode(layout: &Layout, events: &[Seen]) -> String {
    let mut by_stroke = std::collections::HashMap::new();
    for (ch, stroke) in &layout.table {
        let mut mods = stroke.modifiers.clone();
        mods.sort_unstable();
        by_stroke.entry((stroke.keycode, mods)).or_insert(*ch);
    }
    let mod_names = [
        (42u16, "KEY_LEFTSHIFT"),
        (54, "KEY_RIGHTSHIFT"),
        (29, "KEY_LEFTCTRL"),
        (100, "KEY_RIGHTALT"),
        (56, "KEY_LEFTALT"),
        (125, "KEY_LEFTMETA"),
    ];

    let (mut out, mut held): (Vec<char>, Vec<&str>) = (Vec::new(), Vec::new());
    for e in events.iter().filter(|e| e.ty == EV_KEY) {
        if let Some((_, name)) = mod_names.iter().find(|(c, _)| *c == e.code) {
            if e.value == 1 {
                held.push(name);
            } else {
                held.retain(|h| h != name);
            }
            continue;
        }
        if e.value != 1 {
            continue;
        }
        match e.code {
            14 => {
                out.pop(); // backspace, i.e. a correction
            }
            28 => out.push('\n'),
            15 => out.push('\t'),
            code => {
                let mut mods: Vec<&'static str> = held
                    .iter()
                    .filter_map(|h| mod_names.iter().find(|(_, n)| n == h).map(|(_, n)| *n))
                    .collect();
                mods.sort_unstable();
                out.push(*by_stroke.get(&(code, mods)).unwrap_or(&'\u{fffd}'));
            }
        }
    }
    out.into_iter().collect()
}

const SAMPLE: &str =
    "Fidelity check: \"quotes\", (parens), 100% & a+b*c = 42!\nSecond line;\ttabbed.";

// -- tests ------------------------------------------------------------------
#[test]
fn the_virtual_keyboard_looks_like_a_keyboard_the_machine_already_has() {
    // Creates the device and emits nothing. `/proc/bus/input/devices` is
    // world-readable, so this is exactly what anything on the machine can see.
    if !uinput_or_skip("identity") {
        return;
    }
    let ident = identity::for_this_machine(None);
    let mut backend = UInputBackend::with(Duration::from_millis(500), None, Some(ident.clone()));
    backend.open().expect("open uinput backend");
    let listing = std::fs::read_to_string("/proc/bus/input/devices").unwrap();
    backend.close();

    // Several keyboards can share one model name; ours is the one under /virtual/.
    let block = listing
        .split("\n\n")
        .find(|b| {
            b.contains(&format!("Name=\"{}\"", ident.name)) && b.contains("/devices/virtual/")
        })
        .unwrap_or_else(|| {
            panic!(
                "virtual keyboard not listed for {:?}:\n{listing}",
                ident.name
            )
        });

    let field = |prefix: &str| {
        block
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .unwrap_or_else(|| panic!("no {prefix} line in:\n{block}"))
            .trim()
            .to_string()
    };

    assert_eq!(field("P: Phys="), ident.phys, "phys path");
    assert!(
        field("I: ").contains(&format!(
            "Vendor={:04x} Product={:04x}",
            ident.vendor, ident.product
        )),
        "{block}"
    );

    // EV_SYN | EV_KEY | EV_MSC | EV_LED | EV_REP. No EV_FF: a keyboard that can
    // rumble but cannot light its own Caps Lock has never existed.
    let ev = u64::from_str_radix(&field("B: EV="), 16).unwrap();
    let bit = |n: u32| ev >> n & 1 == 1;
    assert!(
        bit(0) && bit(1) && bit(4) && bit(0x11) && bit(0x14),
        "EV={ev:x}"
    );
    assert!(!bit(0x15), "force-feedback bit set: EV={ev:x}");
    assert_eq!(field("B: MSC="), "10", "MSC_SCAN only");
    assert_eq!(field("B: LED="), "7", "num, caps, scroll");
    assert!(!block.to_lowercase().contains("openautotyper"));
    assert!(!block.to_lowercase().contains("uinput"));
}

#[test]
fn the_backend_reports_the_node_it_created() {
    if !uinput_or_skip("device node") {
        return;
    }
    let mut backend = UInputBackend::with(Duration::from_millis(100), None, None);
    assert!(backend.device_path().is_none(), "no node before open");
    backend.open().unwrap();
    let path = backend.device_path().expect("a node after open");
    assert!(path.starts_with("/dev/input/event"), "{path}");
    backend.close();
    assert!(backend.device_path().is_none(), "no node after close");
    backend.close(); // idempotent
}

#[test]
fn keystrokes_on_the_wire_reproduce_the_text() {
    for typo_rate in [0.0, 0.08] {
        let Some((backend, mirror)) = grabbed("wire", Duration::from_millis(650)) else {
            return;
        };
        let layout = xkb::load_layout();
        let mut session = TypeSession::new(
            SAMPLE,
            &Cadence {
                wpm: 400.0,
                typo_rate,
                ..Default::default()
            },
            Box::new(backend),
            SessionOptions {
                countdown: 0.0,
                panic_key: None,
                seed: Some(3),
                ..Default::default()
            },
        )
        .unwrap();
        let result = session.run();
        let captured = mirror.finish();
        assert_eq!(result.state, State::Done, "{result:?}");
        assert_eq!(decode(&layout, &captured), SAMPLE, "typo_rate {typo_rate}");
    }
}

#[test]
fn nothing_is_left_held_on_the_wire_after_a_run() {
    let Some((backend, mirror)) = grabbed("held", Duration::from_millis(650)) else {
        return;
    };
    let mut session = TypeSession::new(
        "Held Keys Check",
        &Cadence {
            wpm: 300.0,
            ..Default::default()
        },
        Box::new(backend),
        SessionOptions {
            countdown: 0.0,
            panic_key: None,
            seed: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    session.run();
    let mut last = std::collections::HashMap::new();
    for e in mirror.finish().iter().filter(|e| e.ty == EV_KEY) {
        last.insert(e.code, e.value);
    }
    assert!(last.values().all(|v| *v == 0), "keys still down: {last:?}");
}

#[test]
fn shift_leads_its_letter_the_way_a_hand_does() {
    // Measured on the wire, not in the model. Before modifiers were scheduled,
    // Shift went down 0.013-0.04 ms ahead of the letter it modified -- the two
    // events shared a millisecond, which no keyboard has ever produced.
    let Some((mut backend, mirror)) = grabbed("shift lead", Duration::from_millis(800)) else {
        return;
    };
    let text = "Hello World. A Quick Brown Fox! Stop.";
    let events = {
        let cell = std::cell::RefCell::new(&mut backend);
        let ask = |t: &str| cell.borrow_mut().modifiers_for(t);
        plan(text, &Cadence::default(), Some(7), Some(&ask)).unwrap()
    };
    for event in &events {
        if event.delay > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(event.delay));
        }
        match event.action {
            Action::Down => backend.key_down(&event.value).unwrap(),
            Action::Up => backend.key_up(&event.value).unwrap(),
        };
    }
    backend.release_all();
    backend.close();
    let captured = mirror.finish();

    let (mut leads, mut pending) = (Vec::new(), None);
    for e in captured.iter().filter(|e| e.ty == EV_KEY && e.value == 1) {
        if e.code == codes::KEY_LEFTSHIFT {
            pending = Some(e.at);
        } else if let Some(at) = pending.take() {
            leads.push(e.at - at);
        }
    }
    assert!(!leads.is_empty(), "no shifted key was typed");
    // The human range is roughly 50-150 ms. The point is the floor: anything
    // under a millisecond is the machine showing through. (Timestamps here are
    // when the mirror read them, which only coarsens the measurement.)
    let min = leads.iter().cloned().fold(f64::MAX, f64::min);
    let mean = leads.iter().sum::<f64>() / leads.len() as f64;
    assert!(min > 0.015, "{leads:?}");
    assert!((0.03..0.20).contains(&mean), "{mean}");
}

#[test]
fn typing_the_abort_key_does_not_abort_the_run() {
    // The watcher must ignore the typer's own virtual keyboard.
    let (readable, why) = openautotyper::panic::available();
    let Some((backend, mirror)) = grabbed("self abort", Duration::from_millis(650)) else {
        return;
    };
    if !readable {
        eprintln!("SKIPPED self abort: {why}");
        return;
    }
    let mut session = TypeSession::new(
        "esc esc esc",
        &Cadence {
            wpm: 400.0,
            ..Default::default()
        },
        Box::new(backend),
        SessionOptions {
            countdown: 0.0,
            panic_key: Some("KEY_ESC".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let result = session.run();
    drop(mirror.finish());
    assert_eq!(result.state, State::Done);
    assert_eq!(result.typed, result.total);
}

#[test]
fn every_key_the_engine_can_name_is_declared_to_the_kernel() {
    // The device advertises a superset of what it will ever send; an
    // undeclared key is dropped silently by the kernel.
    for name in [
        "KEY_ENTER",
        "KEY_TAB",
        "KEY_BACKSPACE",
        "KEY_SPACE",
        "KEY_LEFTSHIFT",
        "KEY_RIGHTALT",
    ] {
        assert!(KEY_CODES.iter().any(|(n, _)| *n == name), "{name}");
    }
}
