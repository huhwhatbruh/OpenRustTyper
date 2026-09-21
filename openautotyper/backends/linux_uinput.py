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
from .evdev_codes import (
    KEY_CODES,
    MODIFIER_CODES,
    MODIFIER_NAMES,
    NAMED_KEY_CODES,
    SCANCODES,
)
from .identity import BUS_USB, Identity, for_this_machine
from .xkb import Layout, Stroke, load_layout

#: The kernel creates the device instantly, but udev has to notice it and the
#: compositor has to add it to its seat. Typing into that window is the classic
#: uinput bug: the first characters vanish. Waiting is the whole fix.
DEFAULT_SETTLE = 0.65

#: Autorepeat, in milliseconds: how long a key must be held before the kernel
#: starts duplicating it, and how fast it then repeats. These are the kernel's
#: own defaults, which is what a keyboard nobody has reconfigured reports.
#: Declaring EV_REP is not cosmetic -- a device with no autorepeat at all is
#: not a keyboard -- but it does mean an over-long hold would type a character
#: twice, so the schedule caps hold time below the delay. See timing.MAX_DWELL.
REPEAT_DELAY_MS = 250
REPEAT_PERIOD_MS = 33


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
        identity: Identity | None = None,
    ) -> None:
        self._settle = settle
        self._hold_lo, self._hold_hi = hold
        self._layout = layout
        #: How the device presents itself to anything that inspects it. The
        #: default is modelled on a keyboard this machine actually has.
        self.identity = identity or for_this_machine()
        self._ui: object | None = None
        self._advertised: frozenset[int] = frozenset()
        #: Keys currently held, by keycode. Several at once is normal.
        self._down: dict[int, Stroke] = {}
        #: Modifier keycodes currently held on behalf of ``_down``.
        self._modifiers: list[int] = []
        #: Modifiers the *schedule* pressed rather than this backend. They are
        #: left alone when a key is released, because their own release is a
        #: scheduled event with its own time; see ``_sync_modifiers``.
        self._scheduled: set[int] = set()
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

        # The capability bitmap is as much a part of looking like a keyboard as
        # the name is, and it lands in world-readable /proc/bus/input/devices:
        #
        #   EV_MSC  every key event is preceded by MSC_SCAN on real hardware
        #   EV_LED  the lock lights; every keyboard reports them, fitted or not
        #   EV_REP  autorepeat, which a device without a repeat rate cannot do
        #
        # max_effects=0 suppresses the EV_FF bit py-evdev would otherwise ask
        # for, which would advertise a keyboard with force feedback and no
        # Caps Lock light -- a device that has never existed.
        events = {
            ecodes.EV_KEY: codes,
            ecodes.EV_MSC: [ecodes.MSC_SCAN],
            ecodes.EV_LED: [ecodes.LED_NUML, ecodes.LED_CAPSL, ecodes.LED_SCROLLL],
            ecodes.EV_REP: [],
        }
        ident = self.identity
        try:
            self._ui = UInput(
                events,
                name=ident.name,
                vendor=ident.vendor, product=ident.product, version=ident.version,
                phys=ident.phys, bustype=BUS_USB, max_effects=0,
            )
        except Exception as exc:  # evdev raises several unrelated types here
            raise BackendError(f"could not create virtual keyboard: {exc}") from exc

        # A keyboard that declares autorepeat has to have a repeat rate; the
        # kernel leaves these at zero unless the device sets them, and zero is
        # a value no real keyboard reports.
        with contextlib.suppress(Exception):
            self._ui.write(ecodes.EV_REP, ecodes.REP_DELAY, REPEAT_DELAY_MS)
            self._ui.write(ecodes.EV_REP, ecodes.REP_PERIOD, REPEAT_PERIOD_MS)
            self._ui.syn()

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
        """Send one key transition the way the hid driver sends one.

        Real hardware reports the raw scancode first and the translated key
        second, in a single report: ``MSC_SCAN``, then ``EV_KEY``, then the
        ``SYN_REPORT`` that closes both. Emitting only the key event is the
        single clearest way to tell a synthetic keyboard from a driven one,
        and it costs one extra write to stop doing it.
        """
        from evdev import ecodes

        scancode = SCANCODES.get(code)
        if scancode is not None:
            self._ui.write(ecodes.EV_MSC, ecodes.MSC_SCAN, scancode)  # type: ignore[union-attr]
        self._ui.write(ecodes.EV_KEY, code, value)  # type: ignore[union-attr]
        self._ui.syn()  # type: ignore[union-attr]

    def modifiers_for(self, token: str) -> tuple[str, ...]:
        """Which modifiers this layout needs held to produce ``token``.

        A question about the keyboard layout, not about the device, and it is
        asked before the device exists: the session compiles its schedule in
        its constructor and opens the backend only when the run starts. So
        this must not go through ``_resolve``, which checks the keycode
        against the set of keys the *open* device advertised -- empty until
        then, which silently answered "no modifiers" for every character and
        left Shift back where it started.
        """
        if token in NAMED_KEY_CODES or token in MODIFIER_NAMES:
            return ()
        if self._layout is None:
            self._layout = load_layout()
        stroke = self._layout.get(token)
        return stroke.modifiers if stroke is not None else ()

    def _resolve(self, token: str) -> Stroke | None:
        """The keycode and modifiers that produce ``token``, or None."""
        evdev_name = NAMED_KEY_CODES.get(token)
        if evdev_name is not None:
            return Stroke(keycode=KEY_CODES[evdev_name], modifiers=())
        # A modifier asked for by name: the schedule presses these itself, so
        # that Shift can lead the letter it modifies the way a hand does.
        if token in MODIFIER_NAMES:
            return Stroke(keycode=KEY_CODES[token], modifiers=())
        stroke = self._layout.get(token) if self._layout else None
        if stroke is None or stroke.keycode not in self._advertised:
            # Unreachable here; the caller falls back to the clipboard rather
            # than emitting an event the kernel would throw away.
            return None
        return stroke

    def _sync_modifiers(self, names: tuple[str, ...] = (), *, sweep: bool = False) -> None:
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
            if sweep and code in self._scheduled:
                # Its release is a scheduled event with a time of its own: a
                # modifier lingers past the key that needed it, exactly as a
                # hand does. Only a press that would be corrupted by it --
                # sweep=False, below -- overrules that.
                continue
            self._emit(code, 0)
            self._modifiers.remove(code)
            self._scheduled.discard(code)
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

        if token in MODIFIER_NAMES:
            # The schedule is pressing this one itself, ahead of the key it
            # modifies. Modifiers are tracked in ``_modifiers``, never in
            # ``_down``: one is a thing held on another key's behalf, the other
            # is a key producing a character, and conflating them is what let
            # a stale Shift capitalise the next letter.
            self._scheduled.add(stroke.keycode)
            if stroke.keycode not in self._modifiers:
                self._modifiers.append(stroke.keycode)
                self._emit(stroke.keycode, 1)
            return True

        # Pressing a key that is already down would strand it there forever;
        # the scheduler forbids it, and this makes the guarantee local.
        if stroke.keycode in self._down:
            self._emit(stroke.keycode, 0)
            del self._down[stroke.keycode]

        self._down[stroke.keycode] = stroke
        # sweep=False: a modifier still held from an earlier run would change
        # this character into one the text never contained, so correctness wins
        # over the schedule's intent here and it goes regardless.
        self._sync_modifiers(stroke.modifiers)
        self._emit(stroke.keycode, 1)
        return True

    def key_up(self, token: str) -> bool:
        if self._ui is None:
            raise BackendError("backend is not open")
        stroke = self._resolve(token)
        if stroke is None:
            return False
        if token in MODIFIER_NAMES:
            self._scheduled.discard(stroke.keycode)
            if stroke.keycode not in self._modifiers:
                return False  # already swept, by a press that could not keep it
            self._modifiers.remove(stroke.keycode)
            self._emit(stroke.keycode, 0)
            return True
        if self._down.pop(stroke.keycode, None) is None:
            return False
        self._emit(stroke.keycode, 0)
        # Modifiers are only ever re-synced by the next press, which knows what
        # it needs. Dropping them here would strand a run of capitals, and
        # re-deriving them from whatever is still held would press Shift with
        # no key under it. Once nothing is down there is nothing to hold for.
        if not self._down:
            self._sync_modifiers(sweep=True)
        return True

    def release_all(self) -> None:
        """Let go of everything. Called on every exit path, including crashes."""
        if self._ui is None:
            return
        self._scheduled.clear()
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
