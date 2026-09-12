"""Backend selection and the keymap that feeds the Linux backends."""

from __future__ import annotations

import contextlib
import sys

import pytest

from openautotyper.backends import BackendError, NullBackend, registry, report, select
from openautotyper.backends.evdev_codes import KEY_CODES, MODIFIER_CODES

linux_only = pytest.mark.skipif(
    not sys.platform.startswith("linux"), reason="Linux keymap"
)


def test_every_backend_probe_returns_a_reason() -> None:
    for name, ok, reason in report():
        assert isinstance(ok, bool)
        assert reason, f"{name} gave no reason"


def test_null_is_never_chosen_automatically() -> None:
    """Typing into the void is indistinguishable from the bug we are fixing."""
    # A machine with no usable backend at all is a legitimate outcome.
    with contextlib.suppress(BackendError):
        assert select().name != "null"


def test_an_unknown_backend_name_is_rejected_clearly() -> None:
    with pytest.raises(BackendError, match="unknown backend"):
        select("telepathy")


def test_null_can_still_be_requested_by_name() -> None:
    assert select("null").name == "null"


def test_every_registered_backend_implements_the_interface() -> None:
    for cls in registry():
        for method in ("available", "open", "type_char", "press_key",
                       "release_all", "close", "paste"):
            assert callable(getattr(cls, method)), f"{cls.name}.{method}"


def test_modifier_codes_are_all_real_keys() -> None:
    assert set(MODIFIER_CODES) <= set(KEY_CODES.values())


@linux_only
def test_keymap_reaches_printable_ascii() -> None:
    from openautotyper.backends.xkb import load_layout

    layout = load_layout()
    missing = [chr(c) for c in range(0x20, 0x7F) if layout.get(chr(c)) is None]
    assert not missing, f"unreachable ASCII: {missing}"


@linux_only
def test_no_character_is_typed_with_a_key_no_keyboard_has() -> None:
    """The regression that silently ate '(' and ')'.

    XKB names keys like KP_Left_Paren at keycode 179. Choosing one produces an
    event the uinput device never declared, the kernel drops it, and the
    character vanishes with nothing logged anywhere.
    """
    from openautotyper.backends.xkb import _PRIMARY, _SECONDARY, load_layout

    reachable = _PRIMARY | _SECONDARY
    offenders = {
        ch: stroke.keycode
        for ch, stroke in load_layout().table.items()
        if ch.isprintable() and ch.isascii() and stroke.keycode not in reachable
    }
    assert not offenders, f"ASCII routed through exotic keys: {offenders}"


@linux_only
def test_capitals_use_shift_rather_than_caps_lock() -> None:
    """Toggling Caps Lock would leave the keyboard in a different state."""
    from openautotyper.backends.xkb import load_layout

    layout = load_layout()
    for ch in "ABZ":
        stroke = layout.get(ch)
        assert stroke is not None
        assert any("SHIFT" in m for m in stroke.modifiers), ch


@linux_only
def test_shifted_and_unshifted_pairs_share_one_physical_key() -> None:
    from openautotyper.backends.xkb import load_layout

    layout = load_layout()
    for lower, upper in [("a", "A"), ("1", "!"), ("9", "("), (",", "<")]:
        low, up = layout.get(lower), layout.get(upper)
        assert low and up and low.keycode == up.keycode, f"{lower}/{upper}"


@linux_only
def test_ranking_prefers_the_main_rows_over_the_keypad() -> None:
    from openautotyper.backends.xkb import _rank

    main_with_shift = _rank(9, ("KEY_LEFTSHIFT",))   # Shift+8 -> '*'
    keypad_no_mods = _rank(55, ())                   # KP_Asterisk -> '*'
    assert main_with_shift < keypad_no_mods


# -- the null backend is test infrastructure, so verify it too ---------------
def test_null_backend_records_and_backspaces() -> None:
    backend = NullBackend()
    backend.open()
    assert backend.type_char("a")
    assert backend.press_key("enter")
    assert backend.type_char("b")
    assert backend.press_key("backspace")
    assert backend.text == "a\n"
    assert backend.press_key("nonsense") is False
    backend.release_all()
    backend.close()
    assert backend.released == 1 and backend.closed


def test_null_backend_honours_a_restricted_layout() -> None:
    backend = NullBackend(reachable="ab")
    backend.open()
    assert backend.type_char("a") is True
    assert backend.type_char("z") is False


def test_backend_context_manager_releases_then_closes() -> None:
    order: list[str] = []

    class Tracked(NullBackend):
        def release_all(self) -> None:
            order.append("release")

        def close(self) -> None:
            order.append("close")

    with Tracked():
        pass
    assert order == ["release", "close"]


# -- modifiers belong to the press, not to the hold --------------------------
# These drive the uinput backend's event stream directly, with a stub layout
# and no device, so the rule they protect is checked on every platform rather
# than only on a machine with a writable /dev/uinput. The end-to-end version
# lives in test_integration_linux.py and skips everywhere else -- which is
# exactly how the regression below reached a release unnoticed.
def _wired(layout_table):
    """A UInputBackend that records events instead of emitting them."""
    from openautotyper.backends.linux_uinput import UInputBackend
    from openautotyper.backends.xkb import Layout

    backend = UInputBackend(settle=0.0, layout=Layout(table=layout_table, source="test"))
    backend._ui = object()  # stand in for the device; _emit is stubbed below
    backend._advertised = frozenset(KEY_CODES.values())
    events: list[tuple[int, int]] = []
    backend._emit = lambda code, value: events.append((code, value))  # type: ignore[method-assign]
    return backend, events


def _typed(events: list[tuple[int, int]]) -> str:
    """Decode recorded events the way the kernel and compositor would.

    A character is whatever the modifier state says it is at the instant its
    key goes down -- which is the whole point being tested.
    """
    shift = KEY_CODES["KEY_LEFTSHIFT"]
    unshifted = {KEY_CODES["KEY_A"]: "a", KEY_CODES["KEY_B"]: "b"}
    shifted = {KEY_CODES["KEY_A"]: "A", KEY_CODES["KEY_B"]: "B"}

    held = False
    out: list[str] = []
    for code, value in events:
        if code == shift:
            held = value == 1
        elif value == 1:
            out.append((shifted if held else unshifted)[code])
    return "".join(out)


def _ab_layout():
    from openautotyper.backends.xkb import Stroke

    shift = ("KEY_LEFTSHIFT",)
    return {
        "a": Stroke(KEY_CODES["KEY_A"], ()),
        "A": Stroke(KEY_CODES["KEY_A"], shift),
        "b": Stroke(KEY_CODES["KEY_B"], ()),
        "B": Stroke(KEY_CODES["KEY_B"], shift),
    }


def test_a_shifted_key_rolling_over_does_not_capitalise_the_next() -> None:
    """The regression that turned "Second" into "SEcond".

    The engine overlaps keystrokes on purpose, so 'A' is still physically held
    when 'b' goes down. Shift belongs to the press that needed it: deriving it
    from everything still held keeps it down over 'b' and silently capitalises
    it, and no exception is raised anywhere.
    """
    backend, events = _wired(_ab_layout())
    backend.key_down("A")
    backend.key_down("b")   # rolls over: 'A' has not been released yet
    backend.key_up("A")
    backend.key_up("b")
    assert _typed(events) == "Ab"


def test_an_unshifted_key_rolling_over_does_not_lowercase_the_next() -> None:
    backend, events = _wired(_ab_layout())
    backend.key_down("a")
    backend.key_down("B")
    backend.key_up("a")
    backend.key_up("B")
    assert _typed(events) == "aB"


def test_a_run_of_capitals_holds_one_continuous_shift() -> None:
    """Shift must not flicker between overlapping capitals."""
    backend, events = _wired(_ab_layout())
    backend.key_down("A")
    backend.key_down("B")
    backend.key_up("A")
    backend.key_up("B")
    shift = KEY_CODES["KEY_LEFTSHIFT"]
    assert _typed(events) == "AB"
    assert [v for c, v in events if c == shift] == [1, 0]


def test_nothing_is_left_held_once_every_key_is_released() -> None:
    backend, events = _wired(_ab_layout())
    for token in ("A", "b", "B", "a"):
        backend.type_char(token)
    assert backend._down == {}
    assert backend._modifiers == []
    held = {}
    for code, value in events:
        held[code] = value
    assert not [c for c, v in held.items() if v == 1], "a key was left down"
