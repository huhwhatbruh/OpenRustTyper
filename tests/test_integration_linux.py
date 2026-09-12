"""End-to-end tests against real kernel input devices.

Skipped unless /dev/uinput is writable, which is the same condition under which
the program can type at all. These are the only tests that prove the thing
actually works; everything else verifies a layer in isolation.
"""

from __future__ import annotations

import sys
import time

import pytest

from openautotyper.session import State, TypeSession
from openautotyper.timing import Cadence

pytestmark = pytest.mark.skipif(
    not sys.platform.startswith("linux"), reason="Linux input devices"
)

evdev = pytest.importorskip("evdev", reason="python-evdev not installed")

from openautotyper.backends.linux_uinput import UInputBackend  # noqa: E402

_usable, _why = UInputBackend.available()
needs_uinput = pytest.mark.skipif(not _usable, reason=f"uinput unusable: {_why}")

SAMPLE = 'Fidelity check: "quotes", (parens), 100% & a+b*c = 42!\nSecond line;\ttabbed.'


def _decode(layout, events) -> str:
    """Turn a captured evdev stream back into the text it represents."""
    from evdev import ecodes

    by_stroke: dict[tuple[int, frozenset[str]], str] = {}
    for ch, stroke in layout.table.items():
        by_stroke.setdefault((stroke.keycode, frozenset(stroke.modifiers)), ch)
    named = {28: "\n", 15: "\t"}
    mods = {42: "KEY_LEFTSHIFT", 54: "KEY_RIGHTSHIFT", 29: "KEY_LEFTCTRL",
            100: "KEY_RIGHTALT", 56: "KEY_LEFTALT", 125: "KEY_LEFTMETA"}

    out: list[str] = []
    held: set[str] = set()
    for event in events:
        if event.type != ecodes.EV_KEY:
            continue
        if event.code in mods:
            held.add(mods[event.code]) if event.value == 1 else held.discard(
                mods[event.code]
            )
            continue
        if event.value != 1:
            continue
        if event.code == 14:  # backspace, i.e. a correction
            if out:
                out.pop()
        elif event.code in named:
            out.append(named[event.code])
        else:
            out.append(by_stroke.get((event.code, frozenset(held)), "�"))
    return "".join(out)


@needs_uinput
@pytest.mark.parametrize("typo_rate", [0.0, 0.08])
def test_keystrokes_on_the_wire_reproduce_the_text(typo_rate: float) -> None:
    """The device is grabbed, so nothing reaches the focused window."""
    from evdev import InputDevice

    backend = UInputBackend()
    backend.open()
    mirror = InputDevice(backend.device_path)
    mirror.grab()

    captured = []
    stop = [False]

    def reader() -> None:
        import selectors

        selector = selectors.DefaultSelector()
        selector.register(mirror, selectors.EVENT_READ)
        while not stop[0]:
            for _key, _mask in selector.select(timeout=0.1):
                try:
                    captured.extend(mirror.read())
                except OSError:
                    return
        selector.close()

    import threading

    thread = threading.Thread(target=reader, daemon=True)
    thread.start()
    try:
        result = TypeSession(
            SAMPLE, Cadence(wpm=400, typo_rate=typo_rate), backend,
            countdown=0, panic_key=None, seed=3,
        ).run()
        time.sleep(0.3)
    finally:
        stop[0] = True
        thread.join(timeout=2)

    assert result.state is State.DONE
    assert _decode(backend._layout, captured) == SAMPLE


@needs_uinput
def test_the_abort_key_stops_a_run_in_flight() -> None:
    """Scroll Lock stands in for Esc so the test keypress is inert."""
    from evdev import UInput, ecodes

    keyboard = UInput(
        {ecodes.EV_KEY: [ecodes.KEY_A, ecodes.KEY_ESC, ecodes.KEY_SCROLLLOCK]},
        name="OpenAutoTyper Test Keyboard",
    )
    time.sleep(0.7)  # let the watcher's device scan see it
    try:
        session = TypeSession(
            "x" * 4000, Cadence(wpm=200), UInputBackend(),
            countdown=0, panic_key="KEY_SCROLLLOCK",
        )
        session.start()
        time.sleep(1.8)
        assert session.state is State.TYPING

        pressed = time.perf_counter()
        keyboard.write(ecodes.EV_KEY, ecodes.KEY_SCROLLLOCK, 1)
        keyboard.syn()
        keyboard.write(ecodes.EV_KEY, ecodes.KEY_SCROLLLOCK, 0)
        keyboard.syn()
        session.join(timeout=5)
        latency = time.perf_counter() - pressed
    finally:
        keyboard.close()

    assert session.result is not None
    assert session.result.state is State.STOPPED
    assert session.result.typed < session.result.total
    assert latency < 1.0, f"abort took {latency:.2f}s"


@needs_uinput
def test_typing_the_abort_key_does_not_abort_the_run() -> None:
    """The watcher must ignore the typer's own virtual keyboard."""
    session = TypeSession(
        "esc esc esc", Cadence(wpm=400), UInputBackend(),
        countdown=0, panic_key="KEY_ESC",
    )
    result = session.run()
    assert result.state is State.DONE
    assert result.typed == result.total
