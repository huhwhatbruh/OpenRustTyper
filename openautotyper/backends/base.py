"""What every keyboard backend must provide."""

from __future__ import annotations

from abc import ABC, abstractmethod

#: Named keys the engine can request by name, in addition to characters.
NAMED_KEYS = ("enter", "tab", "backspace", "escape", "space")


class BackendError(RuntimeError):
    """A backend could not be opened, or is unusable on this system."""


class Backend(ABC):
    """Delivers keystrokes to whatever window currently has focus.

    Presses and releases are separate operations. That is not an ornament: a
    page listening for keydown and keyup measures how long each key was held,
    and on a real keyboard the next key often goes down before the last one
    comes up. A backend that could only deliver a press bonded to its release
    could not express either.

    Implementations are used as context managers. ``close`` must be safe to
    call twice, and must leave no key and no modifier physically held -- a
    stuck Shift or Ctrl outlives the process and wrecks the user's session,
    which is the single worst failure mode this program has.
    """

    #: Short identifier shown in the UI, e.g. ``"uinput"``.
    name: str = "abstract"
    #: One line on what this backend does and where it works.
    description: str = ""

    @staticmethod
    @abstractmethod
    def available() -> tuple[bool, str]:
        """Return ``(usable, reason)`` for this machine, without side effects."""

    @abstractmethod
    def open(self) -> None:
        """Acquire the device. Raises :class:`BackendError` on failure."""

    def modifiers_for(self, token: str) -> tuple[str, ...]:
        """Which modifiers must be held to produce ``token``.

        The answer depends on the keyboard layout, so only a backend can give
        it. The cadence model uses it to schedule Shift and AltGr as keystrokes
        in their own right, with the lead and lag a hand gives them; a backend
        that returns nothing simply has them pressed with the key instead.

        The names returned are valid ``token`` arguments to :meth:`key_down`.
        """
        return ()

    @abstractmethod
    def key_down(self, token: str) -> bool:
        """Press and hold one key.

        ``token`` is a single character, one of :data:`NAMED_KEYS`, or a
        modifier name from :meth:`modifiers_for`. Returns False if this
        keyboard layout cannot reach it, in which case the caller falls back to
        the clipboard rather than pressing something else.
        """

    @abstractmethod
    def key_up(self, token: str) -> bool:
        """Release a key previously pressed. Returns False if it was not held."""

    @abstractmethod
    def release_all(self) -> None:
        """Release every key and modifier held. Must not raise."""

    @abstractmethod
    def close(self) -> None:
        """Release the device. Must be idempotent and must not raise."""

    def type_char(self, ch: str) -> bool:
        """Press and immediately release one key.

        A convenience for callers that do not care about hold time. The engine
        does care, and schedules the two halves itself.
        """
        if not self.key_down(ch):
            return False
        return self.key_up(ch)

    def press_key(self, name: str) -> bool:
        """Press and release one of :data:`NAMED_KEYS`."""
        return self.type_char(name)

    def paste(self, text: str) -> bool:
        """Deliver ``text`` via the clipboard, restoring the previous contents.

        The fallback for characters no keyboard layout can reach. Backends that
        cannot do this return False and the engine reports the character as
        skipped rather than silently typing something else.
        """
        return False

    def __enter__(self) -> Backend:
        self.open()
        return self

    def __exit__(self, *exc: object) -> None:
        try:
            self.release_all()
        finally:
            self.close()
