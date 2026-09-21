"""Linux fallback: XTEST.

Only correct on a real X11 session. Under Wayland this still "works" in the
sense that XOpenDisplay succeeds -- XWayland is running -- but the keystrokes
reach XWayland clients only, which is precisely the trap v1 fell into. So the
probe reports the session type and refuses to volunteer under Wayland; a user
who knows their target is an X11 app can still ask for it by name.

The keycode table is shared with the uinput backend: X11 keycodes are evdev
scancodes plus 8, so one XKB layout serves both.

This backend deliberately does not implement ``modifiers_for``, so Shift is
pressed with the key it modifies rather than scheduled ahead of it as the
uinput backend does. Not an oversight: XTEST announces itself a layer below
anything keystroke timing could fix -- an X client watching through XRecord
sees the injection itself -- so giving Shift a human lead here would buy
nothing and would duplicate the trickiest state machine in the program into
the one path that cannot be exercised from a Wayland session. Use uinput.
"""

from __future__ import annotations

import contextlib
import ctypes
import ctypes.util
import os
import random
import time

from .base import Backend, BackendError
from .xkb import Layout, load_layout

_XKB_EVDEV_OFFSET = 8

# Keysyms for the modifiers we may need to hold.
_MOD_KEYSYMS = {
    "KEY_LEFTSHIFT": (0xFFE1,),
    "KEY_LEFTCTRL": (0xFFE3,),
    "KEY_LEFTALT": (0xFFE9,),
    "KEY_RIGHTALT": (0xFE03, 0xFFEA),  # ISO_Level3_Shift, then Alt_R
    "KEY_LEFTMETA": (0xFFEB,),
}

_NAMED_KEYSYMS = {
    "enter": 0xFF0D, "tab": 0xFF09, "backspace": 0xFF08,
    "escape": 0xFF1B, "space": 0x0020,
}


class X11Backend(Backend):
    """Types through the X11 XTEST extension."""

    name = "x11"
    description = "XTEST via libXtst — X11 sessions only (not native Wayland)"

    def __init__(
        self,
        *,
        hold: tuple[float, float] = (0.010, 0.024),
        layout: Layout | None = None,
    ) -> None:
        self._hold_lo, self._hold_hi = hold
        self._layout = layout
        self._x11: ctypes.CDLL | None = None
        self._xtst: ctypes.CDLL | None = None
        self._display: ctypes.c_void_p | None = None
        #: X keycodes currently held. Several at once is normal.
        self._down: set[int] = set()
        self._modifiers: list[int] = []
        self._rng = random.Random()

    @staticmethod
    def available() -> tuple[bool, str]:
        if not os.environ.get("DISPLAY"):
            return False, "DISPLAY is not set"
        if not ctypes.util.find_library("X11"):
            return False, "libX11 not found"
        if not ctypes.util.find_library("Xtst"):
            return False, "libXtst not found (install libxtst)"
        if os.environ.get("XDG_SESSION_TYPE", "").lower() == "wayland":
            return False, (
                "session is Wayland — XTEST reaches XWayland clients only; "
                "use the uinput backend"
            )
        return True, "ready"

    def open(self) -> None:
        if self._display is not None:
            return
        x11_name = ctypes.util.find_library("X11")
        xtst_name = ctypes.util.find_library("Xtst")
        if not x11_name or not xtst_name:
            raise BackendError("libX11 or libXtst is missing")
        try:
            self._x11 = ctypes.CDLL(x11_name)
            self._xtst = ctypes.CDLL(xtst_name)
        except OSError as exc:
            raise BackendError(f"could not load X libraries: {exc}") from exc

        self._x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
        self._x11.XOpenDisplay.restype = ctypes.c_void_p
        self._x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
        self._x11.XFlush.argtypes = [ctypes.c_void_p]
        self._x11.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
        self._x11.XKeysymToKeycode.restype = ctypes.c_ubyte
        self._xtst.XTestFakeKeyEvent.argtypes = [
            ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong
        ]

        display = self._x11.XOpenDisplay(None)
        if not display:
            raise BackendError("could not open the X display")
        self._display = ctypes.c_void_p(display)

        if self._layout is None:
            self._layout = load_layout()

    def close(self) -> None:
        display, self._display = self._display, None
        if display is not None and self._x11 is not None:
            with contextlib.suppress(Exception):
                self._x11.XCloseDisplay(display)

    # -- emission ----------------------------------------------------------
    def _key(self, keycode: int, press: bool) -> None:
        self._xtst.XTestFakeKeyEvent(self._display, keycode, 1 if press else 0, 0)
        self._x11.XFlush(self._display)

    def _modifier_keycode(self, evdev_name: str) -> int | None:
        for keysym in _MOD_KEYSYMS.get(evdev_name, ()):
            code = self._x11.XKeysymToKeycode(self._display, keysym)
            if code:
                return int(code)
        return None

    def _resolve(self, token: str) -> tuple[int, tuple[str, ...]] | None:
        """The X keycode and modifier names that produce ``token``."""
        keysym = _NAMED_KEYSYMS.get(token)
        if keysym is not None:
            code = self._x11.XKeysymToKeycode(self._display, keysym)
            return (int(code), ()) if code else None
        stroke = self._layout.get(token) if self._layout else None
        if stroke is None:
            return None
        return (stroke.keycode + _XKB_EVDEV_OFFSET, stroke.modifiers)

    def _sync_modifiers(self, names: tuple[str, ...] = ()) -> None:
        """Hold exactly the modifiers ``names`` asks for, and nothing else.

        As in the uinput backend, the modifiers belong to the key going down
        rather than to the set of keys held: a character is fixed by the
        modifier state at the instant of its press, so Shift may lift as soon
        as the press has landed even while the key itself rolls over into the
        next one. Deriving them from everything still held would capitalise
        whatever followed a shifted key.
        """
        wanted: list[int] = []
        for name in names:
            code = self._modifier_keycode(name)
            if code and code not in wanted:
                wanted.append(code)
        for code in [c for c in self._modifiers if c not in wanted]:
            self._key(code, False)
            self._modifiers.remove(code)
        for code in wanted:
            if code not in self._modifiers:
                self._key(code, True)
                self._modifiers.append(code)

    # -- typing ------------------------------------------------------------
    def key_down(self, token: str) -> bool:
        if self._display is None:
            raise BackendError("backend is not open")
        resolved = self._resolve(token)
        if resolved is None:
            return False
        keycode, modifiers = resolved
        # Pressing a key that is already down would strand it there forever.
        if keycode in self._down:
            self._key(keycode, False)
            self._down.discard(keycode)
        self._down.add(keycode)
        self._sync_modifiers(modifiers)  # modifiers first, then the key
        self._key(keycode, True)
        return True

    def key_up(self, token: str) -> bool:
        if self._display is None:
            raise BackendError("backend is not open")
        resolved = self._resolve(token)
        if resolved is None or resolved[0] not in self._down:
            return False
        self._down.discard(resolved[0])
        self._key(resolved[0], False)
        # The next press re-syncs to what it needs; dropping the modifiers
        # here would break up a run of capitals for no gain.
        if not self._down:
            self._sync_modifiers()
        return True

    def release_all(self) -> None:
        if self._display is None:
            return
        try:
            for keycode in list(self._down):
                self._key(keycode, False)
            self._down.clear()
            for name in _MOD_KEYSYMS:
                code = self._modifier_keycode(name)
                if code:
                    self._key(code, False)
            self._modifiers.clear()
        except Exception:
            pass

    def paste(self, text: str) -> bool:
        from ..clipboard import paste_via_clipboard

        return paste_via_clipboard(text, send_ctrl_v=self._send_ctrl_v)

    def _send_ctrl_v(self) -> None:
        ctrl = self._modifier_keycode("KEY_LEFTCTRL")
        code = self._x11.XKeysymToKeycode(self._display, 0x0076)  # 'v'
        if not ctrl or not code:
            return
        self._key(ctrl, True)
        self._key(int(code), True)
        time.sleep(self._rng.uniform(self._hold_lo, self._hold_hi))
        self._key(int(code), False)
        self._key(ctrl, False)
