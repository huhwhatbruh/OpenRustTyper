"""Backend discovery and selection.

Picks the most capable delivery method this machine actually supports, and
says plainly why the others were passed over -- a diagnosis the user can act
on beats a silent fallback that half works.
"""

from __future__ import annotations

import sys

from .base import Backend, BackendError
from .null import NullBackend

__all__ = ["Backend", "BackendError", "NullBackend", "registry", "report", "select"]


def registry() -> list[type[Backend]]:
    """Candidate backends for this platform, best first."""
    candidates: list[type[Backend]] = []
    if sys.platform.startswith("linux"):
        from .linux_uinput import UInputBackend
        from .linux_x11 import X11Backend

        candidates += [UInputBackend, X11Backend]
    elif sys.platform == "win32":
        from .windows import WindowsBackend

        candidates.append(WindowsBackend)
    elif sys.platform == "darwin":
        from .macos import QuartzBackend

        candidates.append(QuartzBackend)
    candidates.append(NullBackend)
    return candidates


def report() -> list[tuple[str, bool, str]]:
    """``(name, usable, reason)`` for every backend, for diagnostics."""
    out: list[tuple[str, bool, str]] = []
    for cls in registry():
        try:
            ok, reason = cls.available()
        except Exception as exc:  # a probe must never take the app down
            ok, reason = False, f"probe failed: {exc}"
        out.append((cls.name, ok, reason))
    return out


def select(name: str | None = None) -> Backend:
    """Build a backend by name, or the best available one.

    ``NullBackend`` is never chosen automatically: silently typing into the
    void would look exactly like the bug this program exists to fix.
    """
    candidates = registry()
    if name:
        for cls in candidates:
            if cls.name == name:
                ok, reason = cls.available()
                if not ok:
                    raise BackendError(f"backend {name!r} is unusable here: {reason}")
                return cls()
        known = ", ".join(cls.name for cls in candidates)
        raise BackendError(f"unknown backend {name!r} (available: {known})")

    problems: list[str] = []
    for cls in candidates:
        if cls is NullBackend:
            continue
        try:
            ok, reason = cls.available()
        except Exception as exc:
            ok, reason = False, str(exc)
        if ok:
            return cls()
        problems.append(f"  {cls.name}: {reason}")

    raise BackendError(
        "no usable keyboard backend on this system:\n" + "\n".join(problems)
    )
