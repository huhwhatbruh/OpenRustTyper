"""Clipboard access, used only as a last resort.

Some characters no keyboard layout can reach -- an em dash on US QWERTY, a
euro sign, an emoji. For those the clipboard is the only route. It is also
destructive: whatever the user had copied is gone. So this module always saves
the previous contents and puts them back, and the engine only calls it for
characters that genuinely have no keystroke.
"""

from __future__ import annotations

import shutil
import subprocess
import time
from collections.abc import Callable

#: How long to leave our text on the clipboard before restoring. The paste is
#: asynchronous -- the target application asks the clipboard owner for the data
#: after receiving Ctrl+V -- so restoring immediately loses the race and pastes
#: the old contents instead.
_PASTE_GRACE = 0.35


def _tools() -> tuple[list[str], list[str]] | None:
    """Return ``(copy_cmd, paste_cmd)`` for this session, or None."""
    if shutil.which("wl-copy") and shutil.which("wl-paste"):
        return ["wl-copy"], ["wl-paste", "--no-newline"]
    if shutil.which("xclip"):
        return (
            ["xclip", "-selection", "clipboard"],
            ["xclip", "-selection", "clipboard", "-o"],
        )
    if shutil.which("xsel"):
        return ["xsel", "--clipboard", "--input"], ["xsel", "--clipboard", "--output"]
    return None


def available() -> tuple[bool, str]:
    """Whether clipboard fallback can work here."""
    if _tools() is not None:
        return True, "ready"
    try:
        import pyperclip  # noqa: F401
    except ImportError:
        return False, "install wl-clipboard (Wayland) or xclip/xsel (X11)"
    return True, "pyperclip"


def read() -> str | None:
    tools = _tools()
    if tools is not None:
        try:
            done = subprocess.run(
                tools[1], capture_output=True, text=True, timeout=4, check=False
            )
            return done.stdout if done.returncode == 0 else None
        except (OSError, subprocess.SubprocessError):
            return None
    try:
        import pyperclip

        return pyperclip.paste()
    except Exception:
        return None


def write(text: str) -> bool:
    tools = _tools()
    if tools is not None:
        try:
            done = subprocess.run(
                tools[0], input=text, text=True, timeout=4, check=False,
                capture_output=True,
            )
            return done.returncode == 0
        except (OSError, subprocess.SubprocessError):
            return False
    try:
        import pyperclip

        pyperclip.copy(text)
        return True
    except Exception:
        return False


def paste_via_clipboard(text: str, send_ctrl_v: Callable[[], None]) -> bool:
    """Put ``text`` on the clipboard, paste it, then restore what was there."""
    saved = read()
    if not write(text):
        return False
    try:
        send_ctrl_v()
        time.sleep(_PASTE_GRACE)
    finally:
        if saved is not None:
            write(saved)
    return True
