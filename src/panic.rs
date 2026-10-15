//! The abort key.
//!
//! A program that injects keystrokes below the display server needs a stop that
//! cannot be blocked by whatever window happens to have focus. Wayland
//! deliberately refuses global hotkeys to ordinary applications, so the answer
//! is to read the keyboards directly through evdev. The watcher observes; it
//! never grabs a device, so keys still reach the focused window exactly as
//! before.

use std::fs::{File, OpenOptions};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::backend::codes::{self, KEY_A, KEY_ESC};

const EV_KEY: u16 = 0x01;
const EV_MAX: usize = 0x1f;

/// `EVIOCGBIT(ev, len)`: which event codes of type `ev` the device can emit.
const fn eviocgbit(ev: usize, len: usize) -> libc::c_ulong {
    ((2u64 << 30) | ((len as u64) << 16) | ((b'E' as u64) << 8) | (0x20 + ev as u64))
        as libc::c_ulong
}

/// Every `/dev/input/event*` node, sorted, that this user can read.
fn readable_devices() -> Vec<PathBuf> {
    let Ok(dir) = std::fs::read_dir("/dev/input") else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = dir
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
                n.strip_prefix("event")
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
            })
        })
        .collect();
    paths.sort();
    paths.retain(|p| {
        std::ffi::CString::new(p.as_os_str().as_encoded_bytes())
            // SAFETY: `access` only reads the NUL-terminated path.
            .is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::R_OK) } == 0)
    });
    paths
}

/// Whether the panic watcher can run for this user.
pub fn available() -> (bool, String) {
    let n = readable_devices().len();
    if n == 0 {
        return (
            false,
            "no readable devices in /dev/input (add yourself to group 'input')".into(),
        );
    }
    (true, format!("watching {n} device(s)"))
}

/// Whether this device can produce both letters and Escape -- a keyboard, as
/// opposed to a mouse or a lid switch.
fn is_keyboard(fd: RawFd) -> bool {
    let mut bits = [0u8; 96]; // KEY_MAX is 0x2ff = 767 bits
                              // SAFETY: EVIOCGBIT(EV_KEY) fills at most `bits.len()` bytes.
    let rc = unsafe {
        libc::ioctl(
            fd,
            eviocgbit(EV_KEY as usize, bits.len()),
            bits.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return false;
    }
    let has = |code: u16| bits[usize::from(code) / 8] >> (code % 8) & 1 == 1;
    has(KEY_ESC) && has(KEY_A)
}

struct Device {
    file: File,
}

/// Calls `on_panic` when the abort key is pressed on a real keyboard.
///
/// Runs a thread. Safe to start and stop repeatedly; `stop` blocks only
/// briefly.
pub struct PanicWatcher {
    on_panic: Arc<dyn Fn() + Send + Sync>,
    code: u16,
    exclude: Vec<String>,
    any_key: bool,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub key_name: String,
}

impl PanicWatcher {
    pub fn new(
        on_panic: impl Fn() + Send + Sync + 'static,
        key: &str,
        exclude_paths: Vec<String>,
        any_key: bool,
    ) -> Self {
        Self {
            on_panic: Arc::new(on_panic),
            code: codes::key_code(key).unwrap_or(KEY_ESC),
            exclude: exclude_paths,
            any_key,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            key_name: key.to_string(),
        }
    }

    /// Begin watching. Returns false if no keyboard could be opened.
    pub fn start(&mut self) -> bool {
        if self.thread.is_some() {
            return true;
        }
        let devices = self.open_devices();
        if devices.is_empty() {
            return false;
        }
        self.stop.store(false, Ordering::SeqCst);
        let (stop, on_panic, code, any_key) = (
            self.stop.clone(),
            self.on_panic.clone(),
            self.code,
            self.any_key,
        );
        self.thread = Some(
            std::thread::Builder::new()
                .name("panic-watcher".into())
                .spawn(move || run(devices, &stop, &*on_panic, code, any_key))
                .expect("spawn panic watcher"),
        );
        true
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn open_devices(&self) -> Vec<Device> {
        readable_devices()
            .into_iter()
            .filter(|p| {
                !self
                    .exclude
                    .iter()
                    .any(|x| p.as_path() == std::path::Path::new(x))
            })
            .filter_map(|p| {
                let file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&p)
                    .ok()?;
                is_keyboard(file.as_raw_fd()).then_some(Device { file })
            })
            .collect()
    }
}

impl Drop for PanicWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    mut devices: Vec<Device>,
    stop: &AtomicBool,
    on_panic: &(dyn Fn() + Send + Sync),
    code: u16,
    any_key: bool,
) {
    const EVENT: usize = std::mem::size_of::<libc::input_event>();
    let mut buf = [0u8; EVENT * 64];

    while !stop.load(Ordering::SeqCst) && !devices.is_empty() {
        let mut fds: Vec<libc::pollfd> = devices
            .iter()
            .map(|d| libc::pollfd {
                fd: d.file.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `fds` is a valid slice of `pollfd` for the duration of the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 200) };
        if ready <= 0 {
            continue;
        }

        let mut gone = Vec::new();
        for (i, pfd) in fds.iter().enumerate() {
            if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                // Device unplugged mid-run; drop it and keep watching.
                gone.push(i);
                continue;
            }
            if pfd.revents & libc::POLLIN == 0 {
                continue;
            }
            // SAFETY: reading into a buffer we own, at most its length.
            let n = unsafe { libc::read(pfd.fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                    gone.push(i);
                }
                continue;
            }
            for chunk in buf[..n as usize].chunks_exact(EVENT) {
                // SAFETY: `chunk` is exactly one `input_event`; read unaligned.
                let ev: libc::input_event =
                    unsafe { std::ptr::read_unaligned(chunk.as_ptr().cast()) };
                if ev.type_ == EV_KEY && ev.value == 1 && (any_key || ev.code == code) {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(on_panic));
                    return;
                }
            }
        }
        for i in gone.into_iter().rev() {
            devices.remove(i);
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let _ = EV_MAX;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eviocgbit_matches_the_kernel_header() {
        // EVIOCGBIT(EV_KEY, 96) from <linux/input.h>
        assert_eq!(eviocgbit(1, 96), 0x8060_4521);
    }

    #[test]
    fn availability_always_explains_itself() {
        let (ok, why) = available();
        assert!(!why.is_empty());
        assert_eq!(ok, why.starts_with("watching"));
    }

    #[test]
    fn a_watcher_with_nothing_readable_reports_failure_and_stops_cleanly() {
        let mut w = PanicWatcher::new(
            || {},
            "KEY_ESC",
            readable_devices()
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            false,
        );
        assert!(!w.start());
        w.stop();
        w.stop();
    }
}
