//! Clipboard access, used only as a last resort.
//!
//! Some characters no keyboard layout can reach -- an em dash on US QWERTY, a
//! euro sign, an emoji. For those the clipboard is the only route. It is also
//! destructive: whatever the user had copied is gone. So this module always
//! saves the previous contents and puts them back, and the engine only calls it
//! for characters that genuinely have no keystroke.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long to leave our text on the clipboard before restoring. The paste is
/// asynchronous -- the target application asks the clipboard owner for the data
/// after receiving Ctrl+V -- so restoring immediately loses the race and pastes
/// the old contents instead.
const PASTE_GRACE: Duration = Duration::from_millis(350);

const TOOL_TIMEOUT: Duration = Duration::from_secs(4);

struct Tools {
    copy: &'static [&'static str],
    paste: &'static [&'static str],
}

pub(crate) fn which(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let candidate = dir.join(program);
            std::fs::metadata(&candidate).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
    })
}

/// `(copy_cmd, paste_cmd)` for this session, or `None`.
fn tools() -> Option<Tools> {
    if which("wl-copy") && which("wl-paste") {
        return Some(Tools {
            copy: &["wl-copy"],
            paste: &["wl-paste", "--no-newline"],
        });
    }
    if which("xclip") {
        return Some(Tools {
            copy: &["xclip", "-selection", "clipboard"],
            paste: &["xclip", "-selection", "clipboard", "-o"],
        });
    }
    if which("xsel") {
        return Some(Tools {
            copy: &["xsel", "--clipboard", "--input"],
            paste: &["xsel", "--clipboard", "--output"],
        });
    }
    None
}

/// Whether clipboard fallback can work here.
pub fn available() -> (bool, String) {
    if tools().is_some() {
        (true, "ready".into())
    } else {
        (
            false,
            "install wl-clipboard (Wayland) or xclip/xsel (X11)".into(),
        )
    }
}

/// Wait for a child, killing it if it outstays [`TOOL_TIMEOUT`].
fn wait_with_timeout(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// The clipboard's current text, or `None` if it cannot be read.
pub fn read() -> Option<String> {
    let tools = tools()?;
    let mut child = Command::new(tools.paste[0])
        .args(&tools.paste[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain on a thread so a large clipboard cannot fill the pipe and stall the
    // child while we wait for it to exit.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut out).map(|_| out)
    });
    let status = wait_with_timeout(&mut child)?;
    let bytes = reader.join().ok()?.ok()?;
    status
        .success()
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Put `text` on the clipboard.
pub fn write(text: &str) -> bool {
    let Some(tools) = tools() else { return false };
    // Output is discarded: `wl-copy` forks a server that inherits its stdout, and
    // waiting for that pipe to close would wait for the server to exit.
    let Ok(mut child) = Command::new(tools.copy[0])
        .args(&tools.copy[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(text.as_bytes()).is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return false;
        }
    }
    wait_with_timeout(&mut child).is_some_and(|s| s.success())
}

/// Put `text` on the clipboard, paste it, then restore what was there.
pub fn paste_via_clipboard(text: &str, send_ctrl_v: impl FnOnce() -> bool) -> bool {
    let saved = read();
    if !write(text) {
        return false;
    }
    let sent = send_ctrl_v();
    if sent {
        std::thread::sleep(PASTE_GRACE);
    }
    if let Some(saved) = saved {
        write(&saved);
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn availability_always_explains_itself() {
        let (ok, why) = available();
        assert!(!why.is_empty());
        assert_eq!(ok, why == "ready");
    }

    #[test]
    fn a_paste_that_cannot_be_sent_reports_failure() {
        // Whatever the machine has installed, a failing Ctrl+V is a failed paste,
        // never a silent success.
        assert!(!paste_via_clipboard("x", || false));
    }
}
