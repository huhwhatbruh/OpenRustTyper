"""Windows backend: SendInput with Unicode events.

Windows offers something neither X11 nor Wayland does: an input event that
carries a character rather than a scancode. ``KEYEVENTF_UNICODE`` hands the
target application the codepoint directly, so there is no keyboard layout to
reason about and no modifier to hold -- a German layout, a Dvorak layout and an
on-screen keyboard all produce identical results.
"""

from __future__ import annotations

import ctypes
import random
import sys

from .base import Backend, BackendError

_INPUT_KEYBOARD = 1
_KEYEVENTF_KEYUP = 0x0002
_KEYEVENTF_UNICODE = 0x0004

# Virtual key codes for the keys that are actions rather than characters.
_VK = {"enter": 0x0D, "tab": 0x09, "backspace": 0x08, "escape": 0x1B, "space": 0x20}

if sys.platform == "win32":
    _ULONG_PTR = ctypes.c_ulonglong if ctypes.sizeof(ctypes.c_void_p) == 8 else ctypes.c_ulong

    class _KEYBDINPUT(ctypes.Structure):
        _fields_ = [
            ("wVk", ctypes.c_ushort), ("wScan", ctypes.c_ushort),
            ("dwFlags", ctypes.c_ulong), ("time", ctypes.c_ulong),
            ("dwExtraInfo", ctypes.POINTER(_ULONG_PTR)),
        ]

    class _INPUT(ctypes.Structure):
        class _U(ctypes.Union):
            _fields_ = [("ki", _KEYBDINPUT), ("_pad", ctypes.c_byte * 24)]

        _anonymous_ = ("u",)
        _fields_ = [("type", ctypes.c_ulong), ("u", _U)]


class WindowsBackend(Backend):
    """Types through ``user32.SendInput``."""

    name = "windows"
    description = "SendInput with Unicode events — layout-independent"

    def __init__(self, *, hold: tuple[float, float] = (0.010, 0.024)) -> None:
        self._hold_lo, self._hold_hi = hold
        self._user32: ctypes.WinDLL | None = None  # type: ignore[name-defined]
        self._down: list[str] = []
        self._rng = random.Random()

    @staticmethod
    def available() -> tuple[bool, str]:
        if sys.platform != "win32":
            return False, "not running on Windows"
        try:
            ctypes.WinDLL("user32", use_last_error=True)  # type: ignore[attr-defined]
        except OSError as exc:
            return False, f"user32 unavailable: {exc}"
        return True, "ready"

    def open(self) -> None:
        ok, reason = self.available()
        if not ok:
            raise BackendError(reason)
        self._user32 = ctypes.WinDLL("user32", use_last_error=True)  # type: ignore[attr-defined]
        self._user32.SendInput.argtypes = [
            ctypes.c_uint, ctypes.POINTER(_INPUT), ctypes.c_int
        ]
        self._user32.SendInput.restype = ctypes.c_uint

    def close(self) -> None:
        self._user32 = None

    def _send(self, events: list[tuple[int, int, int]]) -> bool:
        """Send ``(vk, scan, flags)`` triples as one atomic batch."""
        if self._user32 is None:
            raise BackendError("backend is not open")
        array = (_INPUT * len(events))()
        for i, (vk, scan, flags) in enumerate(events):
            array[i].type = _INPUT_KEYBOARD
            array[i].ki = _KEYBDINPUT(
                wVk=vk, wScan=scan, dwFlags=flags, time=0, dwExtraInfo=None
            )
        sent = self._user32.SendInput(len(events), array, ctypes.sizeof(_INPUT))
        return sent == len(events)

    @staticmethod
    def _units(ch: str) -> list[int]:
        """UTF-16 code units. Astral characters arrive as a surrogate pair and
        both halves must be sent together, or the target sees a lone surrogate.
        """
        raw = ch.encode("utf-16-le")
        return [int.from_bytes(raw[i : i + 2], "little") for i in range(0, len(raw), 2)]

    def key_down(self, token: str) -> bool:
        if self._user32 is None:
            raise BackendError("backend is not open")
        vk = _VK.get(token)
        if vk is not None:
            ok = self._send([(vk, 0, 0)])
        else:
            ok = self._send([(0, unit, _KEYEVENTF_UNICODE) for unit in self._units(token)])
        if ok:
            self._down.append(token)
        return ok

    def key_up(self, token: str) -> bool:
        if self._user32 is None:
            raise BackendError("backend is not open")
        if token not in self._down:
            return False
        self._down.remove(token)
        vk = _VK.get(token)
        if vk is not None:
            return self._send([(vk, 0, _KEYEVENTF_KEYUP)])
        return self._send(
            [(0, unit, _KEYEVENTF_UNICODE | _KEYEVENTF_KEYUP) for unit in self._units(token)]
        )

    def release_all(self) -> None:
        if self._user32 is None:
            return
        try:
            for token in list(self._down):
                self.key_up(token)
            # Unicode events hold nothing down, but a modifier the *user* is
            # leaning on would corrupt every character; lift them defensively.
            for vk in (0x10, 0x11, 0x12, 0x5B, 0x5C):  # Shift Ctrl Alt LWin RWin
                self._send([(vk, 0, _KEYEVENTF_KEYUP)])
        except Exception:
            pass

    def paste(self, text: str) -> bool:
        # SendInput reaches every character directly, so the clipboard detour
        # is never needed here.
        return self.type_char(text) if len(text) == 1 else False
