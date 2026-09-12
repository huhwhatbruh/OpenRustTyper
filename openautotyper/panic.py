"""The abort key.

A program that injects keystrokes below the display server needs a stop that
cannot be blocked by whatever window happens to have focus. Wayland deliberately
refuses global hotkeys to ordinary applications, and v1's answer -- pyautogui's
"fling the mouse into a corner" failsafe -- only fires if the X11 path is alive
at all, which under Wayland it may not be.

Reading the keyboards directly through evdev sidesteps both problems. The
watcher observes; it never grabs a device, so keys still reach the focused
window exactly as before.
"""

from __future__ import annotations

import contextlib
import selectors
import threading
from collections.abc import Callable

from .backends.evdev_codes import KEY_CODES


def available() -> tuple[bool, str]:
    """Whether the panic watcher can run for this user."""
    try:
        import evdev
    except ImportError:
        return False, "python-evdev is not installed"
    try:
        paths = evdev.list_devices()
    except OSError as exc:
        return False, f"cannot enumerate input devices: {exc}"
    if not paths:
        return False, "no readable devices in /dev/input (add yourself to group 'input')"
    return True, f"watching {len(paths)} device(s)"


class PanicWatcher:
    """Calls ``on_panic`` when the abort key is pressed on a real keyboard.

    Runs a daemon thread. Safe to start and stop repeatedly; ``stop`` blocks
    only briefly and never raises.
    """

    def __init__(
        self,
        on_panic: Callable[[], None],
        *,
        key: str = "KEY_ESC",
        exclude_paths: tuple[str, ...] = (),
        any_key: bool = False,
    ) -> None:
        self._on_panic = on_panic
        self._code = KEY_CODES.get(key, KEY_CODES["KEY_ESC"])
        self._exclude = set(exclude_paths)
        self._any_key = any_key
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None
        self.key_name = key

    # -- lifecycle ---------------------------------------------------------
    def start(self) -> bool:
        """Begin watching. Returns False if no keyboard could be opened."""
        if self._thread is not None:
            return True
        devices = self._open_devices()
        if not devices:
            return False
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._run, args=(devices,), name="panic-watcher", daemon=True
        )
        self._thread.start()
        return True

    def stop(self) -> None:
        self._stop.set()
        thread, self._thread = self._thread, None
        if thread is not None:
            thread.join(timeout=1.0)

    def __enter__(self) -> PanicWatcher:
        self.start()
        return self

    def __exit__(self, *exc: object) -> None:
        self.stop()

    # -- internals ---------------------------------------------------------
    def _open_devices(self) -> list[object]:
        try:
            from evdev import InputDevice, ecodes, list_devices
        except ImportError:
            return []

        found: list[object] = []
        for path in list_devices():
            if path in self._exclude:
                continue
            try:
                dev = InputDevice(path)
            except (OSError, PermissionError):
                continue
            try:
                keys = dev.capabilities().get(ecodes.EV_KEY, [])
                # A keyboard, as opposed to a mouse or a lid switch: it can
                # produce both letters and Escape.
                if ecodes.KEY_ESC in keys and ecodes.KEY_A in keys:
                    found.append(dev)
                else:
                    dev.close()
            except Exception:
                with contextlib.suppress(Exception):
                    dev.close()
        return found

    def _run(self, devices: list[object]) -> None:
        from evdev import ecodes

        selector = selectors.DefaultSelector()
        for dev in devices:
            with contextlib.suppress(OSError, ValueError):
                selector.register(dev, selectors.EVENT_READ)
        try:
            while not self._stop.is_set():
                for key, _mask in selector.select(timeout=0.2):
                    dev = key.fileobj
                    try:
                        for event in dev.read():  # type: ignore[union-attr]
                            if event.type != ecodes.EV_KEY or event.value != 1:
                                continue
                            if self._any_key or event.code == self._code:
                                self._fire()
                                return
                    except OSError:
                        # Device unplugged mid-run; drop it and keep watching.
                        with contextlib.suppress(KeyError, ValueError):
                            selector.unregister(dev)
        finally:
            selector.close()
            for dev in devices:
                with contextlib.suppress(Exception):
                    dev.close()  # type: ignore[union-attr]

    def _fire(self) -> None:
        with contextlib.suppress(Exception):
            self._on_panic()
