//! A backend that types into a string instead of the screen.
//!
//! Makes the whole engine testable without a display server, and gives the CLI
//! a `--dry-run` that proves what would be typed before anything is.

use super::codes::NAMED_KEYS;
use super::{Backend, BackendError};

#[derive(Debug, Default)]
pub struct NullBackend {
    /// Characters this pretend layout can produce; `None` means everything.
    reachable: Option<String>,
    /// Modifiers a pretend layout wants for a character, for exercising the
    /// scheduled-Shift path without a keyboard.
    shifted: Option<fn(&str) -> Vec<String>>,
    output: Vec<String>,
    pub pasted: Vec<String>,
    /// Tokens currently held, in press order -- overlap is expected.
    pub down: Vec<String>,
    /// The greatest number held at once, so tests can assert on rollover.
    pub max_concurrent: usize,
    pub opened: bool,
    pub closed: bool,
    pub released: usize,
}

impl NullBackend {
    /// A pretend layout that can only produce these characters.
    pub fn reaching(chars: &str) -> Self {
        Self {
            reachable: Some(chars.to_string()),
            ..Self::default()
        }
    }

    pub fn with_modifiers(mut self, f: fn(&str) -> Vec<String>) -> Self {
        self.shifted = Some(f);
        self
    }

    fn reaches(&self, token: &str) -> bool {
        // A token is either one character or one of the named keys. Anything
        // else is a caller bug, and answering true would swallow it: the
        // engine reads false as "no key reaches this" and falls back to the
        // clipboard, so a typo'd key name must not look like a key that simply
        // worked.
        let mut chars = token.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => self.reachable.as_ref().is_none_or(|r| r.contains(c)),
            _ => NAMED_KEYS.contains(&token) || super::codes::is_modifier(token),
        }
    }

    /// Everything delivered, in order.
    pub fn text(&self) -> String {
        self.output.concat()
    }

    pub fn available() -> (bool, String) {
        (true, "always available".into())
    }
}

impl Backend for NullBackend {
    fn name(&self) -> &'static str {
        "null"
    }

    fn open(&mut self) -> Result<(), BackendError> {
        self.opened = true;
        Ok(())
    }

    fn close(&mut self) {
        self.closed = true;
    }

    fn modifiers_for(&mut self, token: &str) -> Vec<String> {
        self.shifted.map(|f| f(token)).unwrap_or_default()
    }

    fn key_down(&mut self, token: &str) -> Result<bool, BackendError> {
        if !self.reaches(token) {
            return Ok(false);
        }
        // A character lands when the key goes down, which is also when a page
        // receives keydown; the release carries no text.
        match token {
            "backspace" => {
                self.output.pop();
            }
            "enter" => self.output.push("\n".into()),
            "tab" => self.output.push("\t".into()),
            "space" => self.output.push(" ".into()),
            t if super::codes::is_modifier(t) => {}
            "escape" => {}
            t => self.output.push(t.to_string()),
        }
        self.down.push(token.to_string());
        self.max_concurrent = self.max_concurrent.max(self.down.len());
        Ok(true)
    }

    fn key_up(&mut self, token: &str) -> Result<bool, BackendError> {
        match self.down.iter().position(|t| t == token) {
            Some(i) => {
                self.down.remove(i);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    fn paste(&mut self, text: &str) -> bool {
        self.pasted.push(text.to_string());
        self.output.push(text.to_string());
        true
    }

    fn release_all(&mut self) {
        self.down.clear();
        self.released += 1;
    }

    fn recorded_text(&self) -> Option<String> {
        Some(self.text())
    }
}
