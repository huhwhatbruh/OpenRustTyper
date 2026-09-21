"""What the virtual keyboard looks like to anything that inspects it.

The point of typing through /dev/uinput is that the kernel presents the device
as real hardware. That is wasted if the device then announces itself: the name,
the physical path and the capability bitmap all land in world-readable
``/proc/bus/input/devices``, and the defaults py-evdev hands out say
``Phys=py-evdev-uinput`` next to a force-feedback bit no keyboard has.
"""

from __future__ import annotations

import sys

import pytest

from openautotyper.backends.evdev_codes import KEY_CODES, MODIFIER_NAMES, SCANCODES
from openautotyper.backends.identity import (
    _FALLBACK_PHYS,
    _sibling_port,
    for_this_machine,
)

linux_only = pytest.mark.skipif(
    not sys.platform.startswith("linux"), reason="Linux input devices"
)


# -- the identity itself -----------------------------------------------------
def test_the_device_never_names_itself_after_this_program() -> None:
    """The failure this module exists for: the name was a signed confession."""
    identity = for_this_machine()
    lowered = f"{identity.name} {identity.phys}".lower()
    for word in ("openautotyper", "autotyper", "evdev", "uinput", "virtual", "python"):
        assert word not in lowered, f"{word!r} in {identity}"


def test_the_identity_is_a_usb_keyboard_in_every_field() -> None:
    identity = for_this_machine()
    assert identity.phys.startswith("usb-")
    assert 0 < identity.vendor <= 0xFFFF
    assert 0 <= identity.product <= 0xFFFF
    assert identity.source


def test_an_explicit_name_is_honoured() -> None:
    assert for_this_machine("Model M").name == "Model M"


def test_a_cloned_keyboard_is_not_plugged_into_its_own_port() -> None:
    """One keyboard cannot occupy one USB port twice.

    The clone keeps the model but moves the port, so the pair reads as two
    keyboards of one model -- which is an ordinary thing to own.
    """
    real = "usb-0000:00:14.0-10/input1"
    assert _sibling_port(real) != real
    assert _sibling_port(real).startswith("usb-0000:00:14.0-")
    assert _sibling_port(real).endswith("/input1")


def test_an_unparseable_physical_path_still_yields_a_usb_one() -> None:
    assert _sibling_port("isa0060/serio0/input0") == _FALLBACK_PHYS
    assert _sibling_port("") == _FALLBACK_PHYS


@linux_only
def test_the_chosen_model_is_a_keyboard_this_machine_has() -> None:
    from openautotyper.backends.identity import _candidates

    for identity in _candidates():
        assert identity.vendor and identity.phys.startswith("usb-")


# -- the scancodes that go with it -------------------------------------------
def test_every_key_worth_typing_has_a_scancode() -> None:
    """MSC_SCAN is emitted for each key; a gap would be a silent omission."""
    typed = [name for name in KEY_CODES if name != "KEY_LINEFEED"]
    missing = [name for name in typed if KEY_CODES[name] not in SCANCODES]
    assert not missing, missing


def test_scancodes_are_hid_keyboard_usages() -> None:
    """Spot-checked against HID usage page 7, which is what USB reports."""
    for name, usage in (
        ("KEY_A", 0x04), ("KEY_Z", 0x1D), ("KEY_1", 0x1E), ("KEY_ENTER", 0x28),
        ("KEY_ESC", 0x29), ("KEY_BACKSPACE", 0x2A), ("KEY_TAB", 0x2B),
        ("KEY_SPACE", 0x2C), ("KEY_LEFTSHIFT", 0xE1), ("KEY_LEFTCTRL", 0xE0),
    ):
        assert SCANCODES[KEY_CODES[name]] == 0x70000 | usage, name


def test_no_two_keys_claim_the_same_scancode() -> None:
    values = list(SCANCODES.values())
    assert len(values) == len(set(values))


# -- the event stream --------------------------------------------------------
def test_each_key_event_is_preceded_by_its_scancode() -> None:
    """Real hardware reports the raw scancode, then the translated key.

    Driven with a stub device so the rule holds on every platform rather than
    only where /dev/uinput happens to be writable.
    """
    from evdev import ecodes

    from openautotyper.backends.linux_uinput import UInputBackend

    written: list[tuple[int, int, int]] = []

    class FakeUI:
        def write(self, etype: int, code: int, value: int) -> None:
            written.append((etype, code, value))

        def syn(self) -> None:
            written.append((-1, -1, -1))

    backend = UInputBackend(settle=0.0)
    backend._ui = FakeUI()
    backend._advertised = frozenset(KEY_CODES.values())
    backend._emit(KEY_CODES["KEY_A"], 1)
    backend._emit(KEY_CODES["KEY_A"], 0)

    assert written == [
        (ecodes.EV_MSC, ecodes.MSC_SCAN, 0x70004),
        (ecodes.EV_KEY, KEY_CODES["KEY_A"], 1),
        (-1, -1, -1),
        (ecodes.EV_MSC, ecodes.MSC_SCAN, 0x70004),
        (ecodes.EV_KEY, KEY_CODES["KEY_A"], 0),
        (-1, -1, -1),
    ]


# -- and the hold time that keeps autorepeat out of the text ----------------
def test_no_hold_ever_reaches_the_autorepeat_delay() -> None:
    """A device that declares autorepeat will use it.

    EV_REP has to be declared -- a keyboard that cannot repeat is not one --
    so a hold past the kernel's delay would insert a character the source text
    never contained.
    """
    from openautotyper.backends.linux_uinput import REPEAT_DELAY_MS
    from openautotyper.timing import MAX_DWELL, Cadence, plan, stroke_stats

    assert MAX_DWELL < REPEAT_DELAY_MS / 1000.0

    text = "The quick brown fox jumps over the lazy dog. " * 40
    for wpm in (15, 70, 200):
        holds = stroke_stats(plan(text, Cadence(wpm=wpm), seed=3)).dwell
        assert max(holds) <= MAX_DWELL + 1e-9, (wpm, max(holds))


# -- the schedule is built before the device exists --------------------------
@linux_only
def test_modifiers_are_known_before_the_device_is_opened() -> None:
    """The session plans in its constructor and opens the backend at run time.

    Asking the layout through ``_resolve`` checked the keycode against the set
    of keys the *open* device had advertised -- empty at planning time -- so
    every character came back needing no modifiers and Shift silently went back
    to being pressed in the same instant as its letter. Nothing failed; the
    schedule simply had no modifier events in it.
    """
    from openautotyper.backends.linux_uinput import UInputBackend

    backend = UInputBackend(settle=0.0)
    assert backend._advertised == frozenset(), "test assumes an unopened device"
    assert any("SHIFT" in m for m in backend.modifiers_for("A"))
    assert backend.modifiers_for("a") == ()
    assert backend.modifiers_for("enter") == ()


@linux_only
def test_a_session_schedules_shift_for_the_capitals_in_its_text() -> None:
    """End to end through the constructor, which is where the bug lived."""
    from openautotyper.backends.linux_uinput import UInputBackend
    from openautotyper.session import TypeSession
    from openautotyper.timing import Cadence

    ok, _why = UInputBackend.available()
    if not ok:
        pytest.skip("uinput unusable here")

    session = TypeSession("Hello World!", Cadence(wpm=70), UInputBackend(settle=0.0),
                          countdown=0, panic_key=None)
    modifiers = [e for e in session.events if e.value in MODIFIER_NAMES]
    # H, W and ! each need Shift: three presses and three releases.
    assert len(modifiers) == 6, [e.value for e in session.events]
