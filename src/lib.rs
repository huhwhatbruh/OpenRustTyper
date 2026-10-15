//! OpenAutoTyper: type text into the focused window as real keystrokes, at a
//! cadence you control.
//!
//! There is no browser automation, no network access, no model in the loop and
//! no telemetry. It reads text, it presses keys -- through a virtual keyboard
//! on `/dev/uinput`, so it works on Wayland, X11 and a bare TTY alike.

pub mod backend;
pub mod clipboard;
pub mod clock;
pub mod config;
#[cfg(feature = "gui")]
pub mod gui;
pub mod panic;
pub mod session;
pub mod text;
pub mod timing;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
