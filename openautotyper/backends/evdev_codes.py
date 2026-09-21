"""Linux evdev key codes, from ``include/uapi/linux/input-event-codes.h``.

Hardcoded rather than imported from the ``evdev`` package so the keymap can be
built, inspected and tested on a machine that will never type a character --
and because these numbers are kernel ABI and therefore do not move.
"""

from __future__ import annotations

KEY_CODES: dict[str, int] = {
    "KEY_ESC": 1, "KEY_1": 2, "KEY_2": 3, "KEY_3": 4, "KEY_4": 5, "KEY_5": 6,
    "KEY_6": 7, "KEY_7": 8, "KEY_8": 9, "KEY_9": 10, "KEY_0": 11,
    "KEY_MINUS": 12, "KEY_EQUAL": 13, "KEY_BACKSPACE": 14, "KEY_TAB": 15,
    "KEY_Q": 16, "KEY_W": 17, "KEY_E": 18, "KEY_R": 19, "KEY_T": 20,
    "KEY_Y": 21, "KEY_U": 22, "KEY_I": 23, "KEY_O": 24, "KEY_P": 25,
    "KEY_LEFTBRACE": 26, "KEY_RIGHTBRACE": 27, "KEY_ENTER": 28,
    "KEY_LEFTCTRL": 29, "KEY_A": 30, "KEY_S": 31, "KEY_D": 32, "KEY_F": 33,
    "KEY_G": 34, "KEY_H": 35, "KEY_J": 36, "KEY_K": 37, "KEY_L": 38,
    "KEY_SEMICOLON": 39, "KEY_APOSTROPHE": 40, "KEY_GRAVE": 41,
    "KEY_LEFTSHIFT": 42, "KEY_BACKSLASH": 43, "KEY_Z": 44, "KEY_X": 45,
    "KEY_C": 46, "KEY_V": 47, "KEY_B": 48, "KEY_N": 49, "KEY_M": 50,
    "KEY_COMMA": 51, "KEY_DOT": 52, "KEY_SLASH": 53, "KEY_RIGHTSHIFT": 54,
    "KEY_KPASTERISK": 55, "KEY_LEFTALT": 56, "KEY_SPACE": 57,
    "KEY_CAPSLOCK": 58, "KEY_F1": 59, "KEY_F2": 60, "KEY_F3": 61, "KEY_F4": 62,
    "KEY_F5": 63, "KEY_F6": 64, "KEY_F7": 65, "KEY_F8": 66, "KEY_F9": 67,
    "KEY_F10": 68, "KEY_NUMLOCK": 69, "KEY_SCROLLLOCK": 70,
    "KEY_KP7": 71, "KEY_KP8": 72, "KEY_KP9": 73, "KEY_KPMINUS": 74,
    "KEY_KP4": 75, "KEY_KP5": 76, "KEY_KP6": 77, "KEY_KPPLUS": 78,
    "KEY_KP1": 79, "KEY_KP2": 80, "KEY_KP3": 81, "KEY_KP0": 82,
    "KEY_KPDOT": 83, "KEY_102ND": 86, "KEY_F11": 87, "KEY_F12": 88,
    "KEY_KPENTER": 96, "KEY_RIGHTCTRL": 97, "KEY_KPSLASH": 98,
    "KEY_SYSRQ": 99, "KEY_RIGHTALT": 100, "KEY_LINEFEED": 101,
    "KEY_HOME": 102, "KEY_UP": 103, "KEY_PAGEUP": 104, "KEY_LEFT": 105,
    "KEY_RIGHT": 106, "KEY_END": 107, "KEY_DOWN": 108, "KEY_PAGEDOWN": 109,
    "KEY_INSERT": 110, "KEY_DELETE": 111, "KEY_PAUSE": 119,
    "KEY_LEFTMETA": 125, "KEY_RIGHTMETA": 126, "KEY_COMPOSE": 127,
}

#: The modifiers a layout may ask to be held. These double as key tokens: the
#: schedule presses them by name so Shift can lead the letter it modifies.
MODIFIER_NAMES: tuple[str, ...] = (
    "KEY_LEFTSHIFT", "KEY_RIGHTSHIFT", "KEY_LEFTCTRL", "KEY_RIGHTCTRL",
    "KEY_LEFTALT", "KEY_RIGHTALT", "KEY_LEFTMETA", "KEY_RIGHTMETA",
)

#: Every modifier the typing path may hold, for the panic release.
MODIFIER_CODES: tuple[int, ...] = (
    KEY_CODES["KEY_LEFTSHIFT"], KEY_CODES["KEY_RIGHTSHIFT"],
    KEY_CODES["KEY_LEFTCTRL"], KEY_CODES["KEY_RIGHTCTRL"],
    KEY_CODES["KEY_LEFTALT"], KEY_CODES["KEY_RIGHTALT"],
    KEY_CODES["KEY_LEFTMETA"], KEY_CODES["KEY_RIGHTMETA"],
)

#: Engine-level key names mapped to evdev names.
NAMED_KEY_CODES: dict[str, str] = {
    "enter": "KEY_ENTER",
    "tab": "KEY_TAB",
    "backspace": "KEY_BACKSPACE",
    "escape": "KEY_ESC",
    "space": "KEY_SPACE",
}


# ---------------------------------------------------------------------------
# HID usage codes, for MSC_SCAN
# ---------------------------------------------------------------------------
# Every real keyboard emits an ``EV_MSC / MSC_SCAN`` event carrying the raw
# scancode immediately before each ``EV_KEY``, press and release alike. A
# device that never does is not a keyboard the kernel has ever driven, and the
# difference is visible to anything reading evdev directly -- one run of
# ``libinput debug-events`` shows it.
#
# This is ``hid_keyboard[]`` from ``drivers/hid/hid-input.c``, indexed by HID
# usage and giving the evdev keycode; a USB keyboard reports
# ``0x70000 | usage``. Inverting it gives the scancode to announce for a key.
_HID_KEYBOARD: tuple[int, ...] = (
      0,  0,  0,  0, 30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38,
     50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44,  2,  3,
      4,  5,  6,  7,  8,  9, 10, 11, 28,  1, 14, 15, 57, 12, 13, 26,
     27, 43, 43, 39, 40, 41, 51, 52, 53, 58, 59, 60, 61, 62, 63, 64,
     65, 66, 67, 68, 87, 88, 99, 70, 119, 110, 102, 104, 111, 107, 109, 106,
    105, 108, 103, 69, 98, 55, 74, 78, 96, 79, 80, 81, 75, 76, 77, 71,
     72, 73, 82, 83, 86, 127, 116, 117,
)

#: HID usage page 7 is the keyboard page; a USB keyboard's MSC_SCAN is the
#: usage with the page in the high bits.
_HID_KEYBOARD_PAGE = 0x70000

#: Modifier usages live at 0xE0-0xE7, past the block tabulated above.
_MODIFIER_USAGES: dict[int, int] = {
    KEY_CODES["KEY_LEFTCTRL"]: 0xE0, KEY_CODES["KEY_LEFTSHIFT"]: 0xE1,
    KEY_CODES["KEY_LEFTALT"]: 0xE2, KEY_CODES["KEY_LEFTMETA"]: 0xE3,
    KEY_CODES["KEY_RIGHTCTRL"]: 0xE4, KEY_CODES["KEY_RIGHTSHIFT"]: 0xE5,
    KEY_CODES["KEY_RIGHTALT"]: 0xE6, KEY_CODES["KEY_RIGHTMETA"]: 0xE7,
}


def _build_scancodes() -> dict[int, int]:
    out = dict(_MODIFIER_USAGES)
    for usage, keycode in enumerate(_HID_KEYBOARD):
        # The table repeats a keycode for two usages in one place (Backslash
        # and the ISO hash key); the first wins, as it does on real hardware.
        if keycode and keycode not in out:
            out[keycode] = usage
    return {code: _HID_KEYBOARD_PAGE | usage for code, usage in out.items()}


#: evdev keycode -> the MSC_SCAN value a USB keyboard would report for it.
SCANCODES: dict[int, int] = _build_scancodes()
