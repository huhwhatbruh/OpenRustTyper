"""macOS backend: Quartz keyboard events.

Like Windows, macOS can attach a Unicode string to a synthetic key event, so
no layout mapping is needed.

Requires Accessibility permission: System Settings -> Privacy & Security ->
Accessibility, with the terminal or app that launches this added to the list.
Without it CGEventPost silently does nothing, which is why ``available``
cannot fully verify the backend -- the permission is only observable by trying.

Written against the documented CoreGraphics API but, unlike the Linux paths,
not exercised on real hardware here.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import random
import sys

from .base import Backend, BackendError

_kCGHIDEventTap = 0
_VK = {"enter": 36, "tab": 48, "backspace": 51, "escape": 53, "space": 49}


class QuartzBackend(Backend):
    """Types through ``CGEventPost``."""

    name = "macos"
    description = "Quartz CGEvent — needs Accessibility permission"

    def __init__(self, *, hold: tuple[float, float] = (0.010, 0.024)) -> None:
        self._hold_lo, self._hold_hi = hold
        self._cg: ctypes.CDLL | None = None
        self._down: list[str] = []
        self._rng = random.Random()

    @staticmethod
    def available() -> tuple[bool, str]:
        if sys.platform != "darwin":
            return False, "not running on macOS"
        if not ctypes.util.find_library("CoreGraphics"):
            return False, "CoreGraphics not found"
        return True, "ready (Accessibility permission required)"

    def open(self) -> None:
        ok, reason = self.available()
        if not ok:
            raise BackendError(reason)
        path = ctypes.util.find_library("CoreGraphics")
        try:
            cg = ctypes.CDLL(path)
        except OSError as exc:
            raise BackendError(f"could not load CoreGraphics: {exc}") from exc

        cg.CGEventCreateKeyboardEvent.argtypes = [
            ctypes.c_void_p, ctypes.c_uint16, ctypes.c_bool
        ]
        cg.CGEventCreateKeyboardEvent.restype = ctypes.c_void_p
        cg.CGEventKeyboardSetUnicodeString.argtypes = [
            ctypes.c_void_p, ctypes.c_ulong, ctypes.POINTER(ctypes.c_uint16)
        ]
        cg.CGEventPost.argtypes = [ctypes.c_uint32, ctypes.c_void_p]
        cg.CFRelease.argtypes = [ctypes.c_void_p]
        self._cg = cg

    def close(self) -> None:
        self._cg = None

    def _post(self, keycode: int, down: bool, text: str | None) -> bool:
        if self._cg is None:
            raise BackendError("backend is not open")
        event = self._cg.CGEventCreateKeyboardEvent(None, keycode, down)
        if not event:
            return False
        try:
            if text is not None:
                units = text.encode("utf-16-le")
                buf = (ctypes.c_uint16 * (len(units) // 2))(
                    *[
                        int.from_bytes(units[i : i + 2], "little")
                        for i in range(0, len(units), 2)
                    ]
                )
                self._cg.CGEventKeyboardSetUnicodeString(event, len(buf), buf)
            self._cg.CGEventPost(_kCGHIDEventTap, event)
            return True
        finally:
            self._cg.CFRelease(event)

    def key_down(self, token: str) -> bool:
        keycode = _VK.get(token)
        if keycode is not None:
            ok = self._post(keycode, True, None)
        else:
            ok = self._post(0, True, token)
        if ok:
            self._down.append(token)
        return ok

    def key_up(self, token: str) -> bool:
        if token not in self._down:
            return False
        self._down.remove(token)
        keycode = _VK.get(token)
        if keycode is not None:
            return self._post(keycode, False, None)
        return self._post(0, False, token)

    def release_all(self) -> None:
        for token in list(self._down):
            try:
                self.key_up(token)
            except Exception:
                self._down.clear()
                return

    def paste(self, text: str) -> bool:
        return self.type_char(text) if len(text) == 1 else False
