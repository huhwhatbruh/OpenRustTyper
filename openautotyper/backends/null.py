"""A backend that types into a string instead of the screen.

Makes the whole engine testable without a display server, and gives the CLI a
``--dry-run`` that proves what would be typed before anything is.
"""

from __future__ import annotations

from .base import NAMED_KEYS, Backend

_NAMED = {"enter": "\n", "tab": "\t", "space": " "}


class NullBackend(Backend):
    """Accumulates keystrokes in :attr:`output`."""

    name = "null"
    description = "Dry run — records keystrokes without touching the keyboard"

    def __init__(self, *, reachable: str | None = None) -> None:
        #: Characters this pretend layout can produce; None means everything.
        self._reachable = reachable
        self.output: list[str] = []
        self.pasted: list[str] = []
        #: Tokens currently held, in press order — overlap is expected.
        self.down: list[str] = []
        #: The greatest number held at once, so tests can assert on rollover.
        self.max_concurrent = 0
        self.opened = False
        self.closed = False
        self.released = 0

    @staticmethod
    def available() -> tuple[bool, str]:
        return True, "always available"

    def open(self) -> None:
        self.opened = True

    def close(self) -> None:
        self.closed = True

    def _reaches(self, token: str) -> bool:
        # A token is either one character or one of the named keys. Anything
        # else is a caller bug, and answering True would swallow it: the engine
        # reads False as "no key reaches this" and falls back to the clipboard,
        # so a typo'd key name must not look like a key that simply worked.
        if len(token) != 1:
            return token in NAMED_KEYS
        return self._reachable is None or token in self._reachable

    def key_down(self, token: str) -> bool:
        if not self._reaches(token):
            return False
        # A character lands when the key goes down, which is also when a page
        # receives keydown; the release carries no text.
        if token == "backspace":
            if self.output:
                self.output.pop()
        else:
            self.output.append(_NAMED.get(token, token))
        self.down.append(token)
        self.max_concurrent = max(self.max_concurrent, len(self.down))
        return True

    def key_up(self, token: str) -> bool:
        if token not in self.down:
            return False
        self.down.remove(token)
        return True

    def paste(self, text: str) -> bool:
        self.pasted.append(text)
        self.output.append(text)
        return True

    def release_all(self) -> None:
        self.down.clear()
        self.released += 1

    @property
    def text(self) -> str:
        """Everything delivered, in order."""
        return "".join(self.output)
