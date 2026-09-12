"""Build a character -> keystroke table from the system's real XKB layout.

Injecting at the kernel level means sending *scancodes*: the compositor decides
what character a scancode produces, using the active keyboard layout. Hardcode
a US table, as most auto-typers do, and a German or Dvorak user gets gibberish
with no clue why. libxkbcommon already knows the answer, so ask it: compile the
configured layout and walk every key at every shift level to learn which
keycode and modifiers produce which character.
"""

from __future__ import annotations

import ctypes
import ctypes.util
import os
import subprocess
from dataclasses import dataclass

# XKB numbers keycodes from the evdev scancode plus this historical offset.
_XKB_EVDEV_OFFSET = 8

# XKB modifier names mapped to the evdev key we press to engage them. Lock is
# deliberately absent: toggling Caps Lock to reach a capital letter would leave
# the user's keyboard in a different state than it started in.
_MOD_TO_KEY = {
    "shift": "KEY_LEFTSHIFT",
    "control": "KEY_LEFTCTRL",
    "ctrl": "KEY_LEFTCTRL",
    "mod1": "KEY_LEFTALT",
    "alt": "KEY_LEFTALT",
    "mod4": "KEY_LEFTMETA",
    "super": "KEY_LEFTMETA",
    "mod5": "KEY_RIGHTALT",
    "levelthree": "KEY_RIGHTALT",
    "altgr": "KEY_RIGHTALT",
    "iso_level3_shift": "KEY_RIGHTALT",
    "iso_level5_shift": "KEY_RIGHTALT",
}


# A keymap describes every key the layout can name, including ones no ordinary
# keyboard has: XKB puts KP_Left_Paren at 179, so "(" without modifiers looks
# cheaper than Shift+9 and wins a naive comparison. The resulting stroke is one
# no hand could perform, the kernel drops the event for an undeclared key, and
# the character disappears with no error raised anywhere. So candidates are
# ranked by where the key actually lives before anything else is considered.

#: Esc through slash, plus Space: the rows a touch typist uses.
_PRIMARY = frozenset(range(1, 54)) | {57}
#: The extra key on ISO keyboards, absent from ANSI ones.
_SECONDARY = frozenset({86})


def _rank(keycode: int, modifiers: tuple[str, ...]) -> tuple[int, int, int]:
    """Sort key for competing ways to produce one character, best first.

    Home-row reach beats keypad or exotic keys; among equals, fewer modifiers
    wins, then the lower keycode so the table is deterministic.
    """
    if keycode in _PRIMARY:
        tier = 0
    elif keycode in _SECONDARY:
        tier = 1
    else:
        tier = 2
    return (tier, len(modifiers), keycode)


class _RuleNames(ctypes.Structure):
    _fields_ = [
        ("rules", ctypes.c_char_p),
        ("model", ctypes.c_char_p),
        ("layout", ctypes.c_char_p),
        ("variant", ctypes.c_char_p),
        ("options", ctypes.c_char_p),
    ]


@dataclass(frozen=True, slots=True)
class Stroke:
    """The physical action that produces one character."""

    keycode: int                 # evdev scancode
    modifiers: tuple[str, ...]   # evdev key names to hold, e.g. ("KEY_LEFTSHIFT",)


@dataclass(frozen=True, slots=True)
class Layout:
    """A compiled character -> :class:`Stroke` table."""

    table: dict[str, Stroke]
    source: str  # where the layout description came from, for diagnostics

    def __len__(self) -> int:
        return len(self.table)

    def get(self, ch: str) -> Stroke | None:
        return self.table.get(ch)


def _load_library() -> ctypes.CDLL | None:
    name = ctypes.util.find_library("xkbcommon") or "libxkbcommon.so.0"
    try:
        return ctypes.CDLL(name)
    except OSError:
        return None


def _configured_layout() -> tuple[dict[str, str], str]:
    """Discover the active layout, preferring explicit configuration."""
    env = {
        "rules": os.environ.get("XKB_DEFAULT_RULES", ""),
        "model": os.environ.get("XKB_DEFAULT_MODEL", ""),
        "layout": os.environ.get("XKB_DEFAULT_LAYOUT", ""),
        "variant": os.environ.get("XKB_DEFAULT_VARIANT", ""),
        "options": os.environ.get("XKB_DEFAULT_OPTIONS", ""),
    }
    if env["layout"]:
        return env, "XKB_DEFAULT_* environment"

    # localectl reports what the session was actually configured with, and is
    # the only source that works identically under X11 and Wayland.
    try:
        out = subprocess.run(
            ["localectl", "status"],
            capture_output=True, text=True, timeout=3, check=False,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        out = ""

    found: dict[str, str] = {}
    for line in out.splitlines():
        key, _, value = line.strip().partition(":")
        key, value = key.strip().lower(), value.strip()
        if not value:
            continue
        if key == "x11 layout":
            found["layout"] = value
        elif key == "x11 model":
            found["model"] = value
        elif key == "x11 variant":
            found["variant"] = value
        elif key == "x11 options":
            found["options"] = value

    if found.get("layout"):
        return {**env, **found}, f"localectl (layout={found['layout']})"
    return env, "xkbcommon defaults"


def _declare(lib: ctypes.CDLL) -> None:
    u32, ptr = ctypes.c_uint32, ctypes.c_void_p
    lib.xkb_context_new.argtypes = [ctypes.c_int]
    lib.xkb_context_new.restype = ptr
    lib.xkb_context_unref.argtypes = [ptr]
    lib.xkb_keymap_new_from_names.argtypes = [ptr, ctypes.POINTER(_RuleNames), ctypes.c_int]
    lib.xkb_keymap_new_from_names.restype = ptr
    lib.xkb_keymap_unref.argtypes = [ptr]
    lib.xkb_keymap_min_keycode.argtypes = [ptr]
    lib.xkb_keymap_min_keycode.restype = u32
    lib.xkb_keymap_max_keycode.argtypes = [ptr]
    lib.xkb_keymap_max_keycode.restype = u32
    lib.xkb_keymap_num_layouts_for_key.argtypes = [ptr, u32]
    lib.xkb_keymap_num_layouts_for_key.restype = u32
    lib.xkb_keymap_num_levels_for_key.argtypes = [ptr, u32, u32]
    lib.xkb_keymap_num_levels_for_key.restype = u32
    lib.xkb_keymap_key_get_syms_by_level.argtypes = [
        ptr, u32, u32, u32, ctypes.POINTER(ctypes.POINTER(u32))
    ]
    lib.xkb_keymap_key_get_syms_by_level.restype = ctypes.c_int
    lib.xkb_keymap_key_get_mods_for_level.argtypes = [
        ptr, u32, u32, u32, ctypes.POINTER(u32), ctypes.c_size_t
    ]
    lib.xkb_keymap_key_get_mods_for_level.restype = ctypes.c_size_t
    lib.xkb_keysym_to_utf32.argtypes = [u32]
    lib.xkb_keysym_to_utf32.restype = u32
    lib.xkb_keymap_num_mods.argtypes = [ptr]
    lib.xkb_keymap_num_mods.restype = u32
    lib.xkb_keymap_mod_get_name.argtypes = [ptr, u32]
    lib.xkb_keymap_mod_get_name.restype = ctypes.c_char_p


def _mask_to_keys(mask: int, mod_names: list[str]) -> tuple[str, ...] | None:
    """Translate an XKB modifier mask into evdev keys, or None if unusable."""
    keys: list[str] = []
    for bit, mod in enumerate(mod_names):
        if not mask & (1 << bit):
            continue
        if mod == "lock":
            return None  # never toggle Caps Lock on the user's behalf
        key = _MOD_TO_KEY.get(mod)
        if key is None:
            return None  # an exotic modifier we cannot reliably engage
        if key not in keys:
            keys.append(key)
    return tuple(keys)


def load_layout() -> Layout:
    """Compile the active keyboard layout into a character table.

    Falls back to a built-in US QWERTY table when libxkbcommon is unavailable,
    so the backend still works on a minimal system.
    """
    lib = _load_library()
    if lib is None:
        return _fallback("no libxkbcommon")

    try:
        _declare(lib)
    except AttributeError:
        return _fallback("libxkbcommon too old")

    names_dict, source = _configured_layout()
    ctx = lib.xkb_context_new(0)
    if not ctx:
        return _fallback("no xkb context")

    keymap = None
    try:
        names = _RuleNames(
            rules=(names_dict["rules"] or None) and names_dict["rules"].encode(),
            model=(names_dict["model"] or None) and names_dict["model"].encode(),
            layout=(names_dict["layout"] or None) and names_dict["layout"].encode(),
            variant=(names_dict["variant"] or None) and names_dict["variant"].encode(),
            options=(names_dict["options"] or None) and names_dict["options"].encode(),
        )
        keymap = lib.xkb_keymap_new_from_names(ctx, ctypes.byref(names), 0)
        if not keymap:
            return _fallback("keymap compile failed")

        mod_names = []
        for i in range(lib.xkb_keymap_num_mods(keymap)):
            raw = lib.xkb_keymap_mod_get_name(keymap, i)
            mod_names.append(raw.decode().lower() if raw else "")

        table: dict[str, Stroke] = {}
        syms_ptr = ctypes.POINTER(ctypes.c_uint32)()
        masks = (ctypes.c_uint32 * 8)()

        lo = lib.xkb_keymap_min_keycode(keymap)
        hi = lib.xkb_keymap_max_keycode(keymap)
        for keycode in range(lo, hi + 1):
            evdev_code = keycode - _XKB_EVDEV_OFFSET
            if not 0 < evdev_code < 256:
                continue
            # Layout 0 is the primary group; typing should not depend on the
            # user having a secondary group selected.
            if lib.xkb_keymap_num_layouts_for_key(keymap, keycode) == 0:
                continue
            for level in range(lib.xkb_keymap_num_levels_for_key(keymap, keycode, 0)):
                n = lib.xkb_keymap_key_get_syms_by_level(
                    keymap, keycode, 0, level, ctypes.byref(syms_ptr)
                )
                if n != 1:
                    continue  # multi-symbol levels are not single keystrokes
                codepoint = lib.xkb_keysym_to_utf32(syms_ptr[0])
                if codepoint < 0x20 or codepoint == 0x7F:
                    continue  # control characters are handled as named keys
                ch = chr(codepoint)

                count = lib.xkb_keymap_key_get_mods_for_level(
                    keymap, keycode, 0, level, masks, 8
                )
                best: tuple[str, ...] | None = None
                for m in range(count):
                    keys = _mask_to_keys(masks[m], mod_names)
                    if keys is not None and (best is None or len(keys) < len(best)):
                        best = keys
                if best is None:
                    continue

                existing = table.get(ch)
                if existing is None or _rank(evdev_code, best) < _rank(
                    existing.keycode, existing.modifiers
                ):
                    table[ch] = Stroke(keycode=evdev_code, modifiers=best)

        if not table:
            return _fallback("empty keymap")
        return Layout(table=table, source=source)
    finally:
        if keymap:
            lib.xkb_keymap_unref(keymap)
        lib.xkb_context_unref(ctx)


def _build_us_fallback() -> dict[str, Stroke]:
    """A US QWERTY table for systems without libxkbcommon."""
    from .evdev_codes import KEY_CODES

    rows = [
        ("`1234567890-=", "~!@#$%^&*()_+",
         "KEY_GRAVE KEY_1 KEY_2 KEY_3 KEY_4 KEY_5 KEY_6 KEY_7 KEY_8 KEY_9 "
         "KEY_0 KEY_MINUS KEY_EQUAL"),
        ("qwertyuiop[]\\", "QWERTYUIOP{}|",
         "KEY_Q KEY_W KEY_E KEY_R KEY_T KEY_Y KEY_U KEY_I KEY_O KEY_P "
         "KEY_LEFTBRACE KEY_RIGHTBRACE KEY_BACKSLASH"),
        ("asdfghjkl;'", 'ASDFGHJKL:"',
         "KEY_A KEY_S KEY_D KEY_F KEY_G KEY_H KEY_J KEY_K KEY_L "
         "KEY_SEMICOLON KEY_APOSTROPHE"),
        ("zxcvbnm,./", "ZXCVBNM<>?",
         "KEY_Z KEY_X KEY_C KEY_V KEY_B KEY_N KEY_M KEY_COMMA KEY_DOT KEY_SLASH"),
    ]
    table: dict[str, Stroke] = {}
    for plain, shifted, keys in rows:
        for ch, sh, key in zip(plain, shifted, keys.split(), strict=True):
            code = KEY_CODES[key]
            table[ch] = Stroke(keycode=code, modifiers=())
            table[sh] = Stroke(keycode=code, modifiers=("KEY_LEFTSHIFT",))
    table[" "] = Stroke(keycode=KEY_CODES["KEY_SPACE"], modifiers=())
    return table


def _fallback(reason: str) -> Layout:
    return Layout(table=_build_us_fallback(), source=f"built-in US QWERTY ({reason})")
