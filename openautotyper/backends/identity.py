"""What the virtual keyboard looks like to anything that inspects it.

A uinput device is a real keyboard as far as the *input stack* is concerned --
that is the whole reason v2 uses one. It is not automatically a real keyboard
as far as *userspace* is concerned, because ``/proc/bus/input/devices`` is
world-readable and says what the device called itself. The default identity
py-evdev hands out is unmistakable::

    N: Name="OpenAutoTyper Virtual Keyboard"
    P: Phys=py-evdev-uinput
    B: EV=200003

against a keyboard that is actually plugged in::

    N: Name="HID 0d62:3740 Keyboard"
    P: Phys=usb-0000:00:14.0-10/input1
    B: EV=12001f
    B: MSC=10
    B: LED=7

Four separate giveaways there, and the name is only the loudest. ``Phys`` still
says py-evdev even after a rename. The capability bitmap is wrong in a way no
keyboard's ever is: no ``MSC_SCAN``, no LEDs, no autorepeat -- and an ``EV_FF``
bit, because py-evdev asks for force-feedback effect slots by default, giving a
keyboard that can rumble but cannot light its own Caps Lock.

So this module builds an identity from a keyboard the machine genuinely has.
The device then reads as a second keyboard of a model already present, which is
an ordinary thing for a computer to have.

What this does not do, and cannot: the kernel puts every uinput device under
``/sys/devices/virtual/input/``, and nothing a userspace process passes to
``UI_DEV_SETUP`` changes that. Anything willing to follow the sysfs path knows.
The aim here is to stop announcing it, not to defeat an inspection.
"""

from __future__ import annotations

import os
import re
from dataclasses import dataclass

#: Used when no real keyboard can be read -- a generic USB HID descriptor, the
#: single most common shape of name on any machine with an external keyboard.
_GENERIC = ("USB Keyboard", 0x1A2C, 0x2124, 0x0110)

#: Bus 0x03 is BUS_USB. A keyboard on any other bus (0x11, the i8042 port, for
#: a laptop's built-in one) would need a matching ``Phys``, and inventing a PS/2
#: port that is not there is worse than presenting as USB, which every machine
#: plausibly has.
BUS_USB = 0x03

#: A plausible port for a keyboard on a machine whose real ones cannot be read.
_FALLBACK_PHYS = "usb-0000:00:14.0-2/input0"


@dataclass(frozen=True, slots=True)
class Identity:
    """The name and ids a virtual keyboard presents itself under."""

    name: str
    vendor: int
    product: int
    version: int
    phys: str
    #: Where this came from, for ``--doctor``.
    source: str


def _read(path: str) -> str:
    try:
        with open(path, encoding="utf-8", errors="replace") as handle:
            return handle.read().strip()
    except OSError:
        return ""


def _candidates() -> list[Identity]:
    """Real USB keyboards this machine has, best to clone first.

    Read from ``/sys`` rather than by opening ``/dev/input/event*``: naming a
    device needs no more privilege than listing it, and a machine where the
    abort key cannot be watched should still get a decent identity.
    """
    root = "/sys/class/input"
    try:
        entries = sorted(os.listdir(root))
    except OSError:
        return []

    ranked: list[tuple[tuple[int, int], Identity]] = []
    for entry in entries:
        if not re.fullmatch(r"input\d+", entry):
            continue
        base = f"{root}/{entry}"
        if "/virtual/" in os.path.realpath(base):
            continue  # another uinput device, possibly one of ours
        name = _read(f"{base}/name")
        if not name:
            continue
        try:
            vendor = int(_read(f"{base}/id/vendor") or "0", 16)
            product = int(_read(f"{base}/id/product") or "0", 16)
            version = int(_read(f"{base}/id/version") or "0", 16)
            bustype = int(_read(f"{base}/id/bustype") or "0", 16)
        except ValueError:
            continue
        # Only USB keyboards are worth cloning. An identity has to come with a
        # Phys matching the bus it claims, and a believable USB port path is
        # something that can be constructed; a second keyboard on the laptop's
        # single i8042 port is not a machine that exists.
        if bustype != BUS_USB or not vendor:
            continue
        if not _is_a_keyboard(base):
            continue

        phys = _read(f"{base}/phys")
        # A gaming mouse's HID keyboard endpoint passes every capability test a
        # keyboard does, so the last word goes to the name: presenting as a
        # mouse would be its own kind of odd.
        says_keyboard = 0 if "keyboard" in name.lower() else 1
        ranked.append((
            (says_keyboard, len(ranked)),
            Identity(
                name=name, vendor=vendor, product=product, version=version,
                phys=_sibling_port(phys) if phys else _FALLBACK_PHYS,
                source=f"modelled on this machine's {name!r}",
            ),
        ))

    ranked.sort(key=lambda pair: pair[0])
    return [identity for _rank, identity in ranked]


def _is_a_keyboard(base: str) -> bool:
    """Whether this input node is a keyboard, rather than some other button.

    A mouse, a lid switch and a power button are all ``EV_KEY`` devices. Two
    things separate a keyboard: it can produce letters, and it has the lock
    lights. Every real keyboard reports ``LED`` capabilities even when the
    lights are not physically fitted.
    """
    if not _bit(_read(f"{base}/capabilities/led"), 0):  # LED_NUML
        return False
    key_a, key_z = 30, 44
    keys = _read(f"{base}/capabilities/key")
    return _bit(keys, key_a) and _bit(keys, key_z)


def _bit(bitmap: str, index: int) -> bool:
    """Read one bit out of a sysfs capability bitmap.

    The kernel prints these as space-separated hex words, most significant
    word first, each covering 64 bits.
    """
    words = bitmap.split()
    if not words:
        return False
    try:
        value = int("".join(f"{int(word, 16):064b}" for word in words), 2)
    except ValueError:
        return False
    return bool(value >> index & 1)


def _sibling_port(phys: str) -> str:
    """A USB port path next to ``phys``, so the clone is not the same device.

    Two input nodes reporting the same name *and* the same physical path are a
    contradiction: one keyboard cannot be plugged into one port twice. Moving
    the port number makes the pair read as two keyboards of one model.
    """
    match = re.fullmatch(r"(usb-[0-9a-f:.]+-)(\d+)((?:\.\d+)*/input\d+)", phys)
    if match:
        port = int(match.group(2))
        return f"{match.group(1)}{port % 8 + 1}{match.group(3)}"
    return _FALLBACK_PHYS


def for_this_machine(override: str | None = None) -> Identity:
    """Pick the identity the virtual keyboard should present.

    ``override`` names the device outright, for anyone who would rather choose.
    """
    if override:
        return Identity(override, *_GENERIC[1:], _FALLBACK_PHYS,
                        "chosen by the user")

    candidates = _candidates()
    if candidates:
        return candidates[0]

    name, vendor, product, version = _GENERIC
    return Identity(
        name=name, vendor=vendor, product=product, version=version,
        phys=_FALLBACK_PHYS,
        source="generic USB keyboard (no readable keyboard to model)",
    )
