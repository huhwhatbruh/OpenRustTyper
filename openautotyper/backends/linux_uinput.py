"""Linux backend: a virtual keyboard on /dev/uinput.

This is the backend that makes v2 work where v1 did not. XTEST -- what
pyautogui uses -- is an X11 protocol extension, so under Wayland it reaches
only XWayland clients. Keystrokes aimed at a native Wayland window (KDE's own
apps, Firefox and Chromium in Wayland mode, anything Electron since 2023) are
delivered to nothing at all, which looks exactly like "laggy, drops
characters, sometimes does nothing".

A uinput device sits below all of that. The kernel presents it as a real
keyboard, so the compositor routes its events like any other keyboard, on
Wayland and X11 alike, and in a plain TTY.
"""

from __future__ import annotations

import contextlib
import os
import random
import time

from .base import Backend, BackendError
from .evdev_codes import KEY_CODES, MODIFIER_CODES, NAMED_KEY_CODES
from .xkb import Layout, Stroke, load_layout

#: The kernel creates the device instantly, but udev has to notice it and the
#: compositor has to add it to its seat. Typing into that window is the classic
#: uinput bug: the first characters vanish. Waiting is the whole fix.
DEFAULT_SETTLE = 0.65


class UInputBackend(Backend):
    """Types by driving a virtual keyboard the kernel exposes for us."""

    name = "uinput"
    description = "Virtual keyboard via /dev/uinput — works on Wayland, X11 and TTY"

    def __init__(
        self,
        *,
        settle: float = DEFAULT_SETTLE,
        hold: tuple[float, float] = (0.010, 0.024),
        layout: Layout | None = None,
    ) -> None:
        self._settle = settle
        self._hold_lo, self._hold_hi = hold
        self._layout = layout
        self._ui: object | None = None
        self._advertised: frozenset[int] = frozenset()
        #: Keys currently held, by keycode. Several at once is normal.
        self._down: dict[int, Stroke] = {}
        #: Modifier keycodes currently held on behalf of ``_down``.
        self._modifiers: list[int] = []
        self._rng = random.Random()
        #: Event node of our own virtual keyboard. The panic-key watcher must
        #: ignore it, or the typer would trigger its own abort the moment the
        #: text contains the panic key.
        self.device_path: str | None = None

    # -- discovery ---------------------------------------------------------
    @staticmethod
    def available() -> tuple[bool, str]:
        try:
            import evdev  # noqa: F401
        except ImportError:
            return False, "python-evdev is not installed (pip install evdev)"
        if not os.path.exists("/dev/uinput"):
            return False, "/dev/uinput does not exist (load the uinput module)"
        try:
            fd = os.open("/dev/uinput", os.O_WRONLY | os.O_NONBLOCK)
        except PermissionError:
            return False, "no write permission on /dev/uinput (see README: udev rule)"
        except OSError as exc:
            return False, f"/dev/uinput could not be opened: {exc}"
        os.close(fd)
        return True, "ready"

    # -- lifecycle ---------------------------------------------------------
    def open(self) -> None:
        if self._ui is not None:
            return
        ok, reason = self.available()
        if not ok:
            raise BackendError(reason)

        from evdev import UInput, ecodes

        if self._layout is None:
            self._layout = load_layout()

        # Advertise every key we could conceivably send. A device that claims
        # only the keys in this particular text would be a different device
        # each run, which confuses compositors that cache seat capabilities.
        #
        # The layout's own keycodes are included as well: the kernel silently
        # discards an event for a key the device never declared, which is
        # indistinguishable from a dropped character. Declaring a superset
        # costs nothing and removes the failure mode.
        advertised = set(KEY_CODES.values())
        advertised.update(
            stroke.keycode
            for stroke in self._layout.table.values()
            if 0 < stroke.keycode < 256
        )
        self._advertised = frozenset(advertised)
        codes = sorted(advertised)
        try:
            self._ui = UInput(
                {ecodes.EV_KEY: codes},
                name="OpenAutoTyper Virtual Keyboard",
                vendor=0x1D6B, product=0x0001, version=0x0200,
            )
        except Exception as exc:  # evdev raises several unrelated types here
            raise BackendError(f"could not create virtual keyboard: {exc}") from exc

        try:
            self.device_path = self._ui.device.path  # type: ignore[union-attr]
        except Exception:
            self.device_path = None

        time.sleep(self._settle)

    def close(self) -> None:
        ui, self._ui = self._ui, None
        if ui is None:
            return
        with contextlib.suppress(Exception):
            ui.close()  # type: ignore[union-attr]
        self.device_path = None

    # -- raw emission ------------------------------------------------------
    def _emit(self, code: int, value: int) -> None:
        from evdev import ecodes

        self._ui.write(ecodes.EV_KEY, code, value)  # type: ignore[union-attr]
        self._ui.syn()  # type: ignore[union-attr]

    def _resolve(self, token: str) -> Stroke | None:
        """The keycode and modifiers that produce ``token``, or None."""
        evdev_name = NAMED_KEY_CODES.get(token)
        if evdev_name is not None:
            return Stroke(keycode=KEY_CODES[evdev_name], modifiers=())
        stroke = self._layout.get(token) if self._layout else None
        if stroke is None or stroke.keycode not in self._advertised:
            # Unreachable here; the caller falls back to the clipboard rather
            # than emitting an event the kernel would throw away.
            return None
        return stroke

    def _sync_modifiers(self, names: tuple[str, ...] = ()) -> None:
        """Hold exactly the modifiers ``names`` asks for, and nothing else.

        The modifiers belong to the key *going down*, not to the set of keys
        held. That distinction is the whole correctness of rollover. A
        character is decided by the modifier state at the instant its key is
        pressed, so once a shifted key has registered, Shift has done its job
        and may lift even though the key itself is still down -- which is
        exactly what a hand does when it runs on into the next letter.

        Taking the union over every held key instead would keep Shift down for
        as long as any shifted key overlapped the next press, and quietly
        capitalise it: "Second" arrives as "SEcond", '"quotes"' as '"Quotes"'.

        A run of capitals still holds one continuous Shift, because each press
        in the run asks for it and an already-held modifier is left alone.
        """
        wanted: list[int] = []
        for name in names:
            code = KEY_CODES.get(name)
            if code is not None and code not in wanted:
                wanted.append(code)

        for code in [c for c in self._modifiers if c not in wanted]:
            self._emit(code, 0)
            self._modifiers.remove(code)
        for code in wanted:
            if code not in self._modifiers:
                self._emit(code, 1)
                self._modifiers.append(code)

    # -- typing ------------------------------------------------------------
    def key_down(self, token: str) -> bool:
        if self._ui is None:
            raise BackendError("backend is not open")
        stroke = self._resolve(token)
        if stroke is None:
            return False

        # Pressing a key that is already down would strand it there forever;
        # the scheduler forbids it, and this makes the guarantee local.
        if stroke.keycode in self._down:
            self._emit(stroke.keycode, 0)
            del self._down[stroke.keycode]

        self._down[stroke.keycode] = stroke
        self._sync_modifiers(stroke.modifiers)  # modifiers first, then the key
        self._emit(stroke.keycode, 1)
        return True

    def key_up(self, token: str) -> bool:
        if self._ui is None:
            raise BackendError("backend is not open")
        stroke = self._resolve(token)
        if stroke is None or self._down.pop(stroke.keycode, None) is None:
            return False
        self._emit(stroke.keycode, 0)
        # Modifiers are only ever re-synced by the next press, which knows what
        # it needs. Dropping them here would strand a run of capitals, and
        # re-deriving them from whatever is still held would press Shift with
        # no key under it. Once nothing is down there is nothing to hold for.
        if not self._down:
            self._sync_modifiers()
        return True

    def release_all(self) -> None:
        """Let go of everything. Called on every exit path, including crashes."""
        if self._ui is None:
            return
        try:
            for code in list(self._down):
                self._emit(code, 0)
            self._down.clear()
            # Then sweep every modifier unconditionally: a half-finished
            # _sync_modifiers leaves the bookkeeping and the hardware
            # disagreeing, and a stuck Ctrl is far worse than a spare key-up.
            for code in MODIFIER_CODES:
                self._emit(code, 0)
            self._modifiers.clear()
        except Exception:
            pass

    # -- clipboard fallback ------------------------------------------------
    def paste(self, text: str) -> bool:
        from ..clipboard import paste_via_clipboard

        return paste_via_clipboard(text, send_ctrl_v=self._send_ctrl_v)

    def _send_ctrl_v(self) -> None:
        ctrl = KEY_CODES["KEY_LEFTCTRL"]
        self._emit(ctrl, 1)
        self._emit(KEY_CODES["KEY_V"], 1)
        time.sleep(self._rng.uniform(self._hold_lo, self._hold_hi))
        self._emit(KEY_CODES["KEY_V"], 0)
        self._emit(ctrl, 0)
