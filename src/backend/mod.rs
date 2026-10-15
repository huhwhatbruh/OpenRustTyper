//! Backend discovery and selection.
//!
//! Picks the most capable delivery method this machine actually supports, and
//! says plainly why the others were passed over -- a diagnosis the user can act
//! on beats a silent fallback that half works.
//!
//! Linux only, and deliberately. Windows and macOS mark injected events as
//! synthetic and hand the flag to any application that asks, so a backend there
//! could never type as indistinguishably as a uinput device does.

pub mod codes;
pub mod identity;
pub mod null;
pub mod uinput;
pub mod x11;
pub mod xkb;

#[cfg(test)]
mod tests;

use std::fmt;

pub use null::NullBackend;
pub use uinput::UInputBackend;
pub use x11::X11Backend;

/// A backend could not be opened, or is unusable on this system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendError(pub String);

impl fmt::Display for BackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BackendError {}

impl From<String> for BackendError {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// Delivers keystrokes to whatever window currently has focus.
///
/// Presses and releases are separate operations. That is not an ornament: a
/// page listening for keydown and keyup measures how long each key was held,
/// and on a real keyboard the next key often goes down before the last one
/// comes up. A backend that could only deliver a press bonded to its release
/// could not express either.
///
/// `close` must be safe to call twice, and must leave no key and no modifier
/// physically held -- a stuck Shift or Ctrl outlives the process and wrecks the
/// user's session, which is the single worst failure mode this program has.
pub trait Backend: Send {
    /// Short identifier shown in the UI, e.g. `"uinput"`.
    fn name(&self) -> &'static str;

    /// Acquire the device.
    fn open(&mut self) -> Result<(), BackendError>;

    /// Which modifiers must be held to produce `token`.
    ///
    /// The answer depends on the keyboard layout, so only a backend can give
    /// it. The cadence model uses it to schedule Shift and AltGr as keystrokes
    /// in their own right, with the lead and lag a hand gives them; a backend
    /// that returns nothing simply has them pressed with the key instead.
    fn modifiers_for(&mut self, _token: &str) -> Vec<String> {
        Vec::new()
    }

    /// Press and hold one key. `token` is a single character, one of
    /// [`codes::NAMED_KEYS`], or a modifier name from [`Backend::modifiers_for`].
    /// Returns `Ok(false)` if this keyboard layout cannot reach it, in which
    /// case the caller falls back to the clipboard rather than pressing
    /// something else.
    fn key_down(&mut self, token: &str) -> Result<bool, BackendError>;

    /// Release a key previously pressed. `Ok(false)` if it was not held.
    fn key_up(&mut self, token: &str) -> Result<bool, BackendError>;

    /// Release every key and modifier held. Must not panic.
    fn release_all(&mut self);

    /// Release the device. Idempotent; must not panic.
    fn close(&mut self);

    /// Deliver `text` via the clipboard, restoring the previous contents. The
    /// fallback for characters no keyboard layout can reach. Backends that
    /// cannot do this return false and the engine reports the character as
    /// skipped rather than silently typing something else.
    fn paste(&mut self, _text: &str) -> bool {
        false
    }

    /// Event node of this backend's own virtual keyboard, if it has one. The
    /// abort-key watcher must ignore it, or the typer would trigger its own
    /// abort the moment the text contains the abort key.
    fn device_path(&self) -> Option<String> {
        None
    }

    /// What a dry run recorded. Only the null backend has anything to say.
    fn recorded_text(&self) -> Option<String> {
        None
    }

    /// Press and immediately release one key.
    fn type_char(&mut self, token: &str) -> Result<bool, BackendError> {
        if !self.key_down(token)? {
            return Ok(false);
        }
        self.key_up(token)
    }
}

/// One entry per backend, best first. `null` is last and is never volunteered.
struct Candidate {
    name: &'static str,
    available: fn() -> (bool, String),
    build: fn() -> Box<dyn Backend>,
}

fn registry() -> [Candidate; 3] {
    [
        Candidate {
            name: "uinput",
            available: UInputBackend::available,
            build: || Box::new(UInputBackend::new()),
        },
        Candidate {
            name: "x11",
            available: X11Backend::available,
            build: || Box::new(X11Backend::new()),
        },
        Candidate {
            name: "null",
            available: NullBackend::available,
            build: || Box::new(NullBackend::default()),
        },
    ]
}

/// `(name, usable, reason)` for every backend, for diagnostics.
pub fn report() -> Vec<(&'static str, bool, String)> {
    registry()
        .iter()
        .map(|c| {
            // A probe must never take the app down.
            match std::panic::catch_unwind(c.available) {
                Ok((ok, reason)) => (c.name, ok, reason),
                Err(_) => (c.name, false, "probe failed".to_string()),
            }
        })
        .collect()
}

/// Build a backend by name, or the best available one.
///
/// `NullBackend` is never chosen automatically: silently typing into the void
/// would look exactly like the bug this program exists to fix.
pub fn select(name: Option<&str>) -> Result<Box<dyn Backend>, BackendError> {
    let candidates = registry();
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        if let Some(c) = candidates.iter().find(|c| c.name == name) {
            let (ok, reason) = (c.available)();
            if !ok {
                return Err(BackendError(format!(
                    "backend '{name}' is unusable here: {reason}"
                )));
            }
            return Ok((c.build)());
        }
        let known: Vec<&str> = candidates.iter().map(|c| c.name).collect();
        return Err(BackendError(format!(
            "unknown backend '{name}' (available: {})",
            known.join(", ")
        )));
    }

    let mut problems = Vec::new();
    for c in candidates.iter().filter(|c| c.name != "null") {
        let (ok, reason) = std::panic::catch_unwind(c.available)
            .unwrap_or_else(|_| (false, "probe failed".to_string()));
        if ok {
            return Ok((c.build)());
        }
        problems.push(format!("  {}: {reason}", c.name));
    }
    Err(BackendError(format!(
        "no usable keyboard backend on this system:\n{}",
        problems.join("\n")
    )))
}
