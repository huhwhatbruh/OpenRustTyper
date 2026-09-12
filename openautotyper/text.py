"""Text normalization.

Pasted text carries characters that no keyboard can produce: smart quotes from
word processors, non-breaking spaces from HTML, zero-width joiners from
absolutely everywhere. Typing them requires a clipboard detour that is slow and
visible; folding them to their ASCII equivalents is both faster and closer to
what a person at a keyboard would have produced.
"""

from __future__ import annotations

import unicodedata
from dataclasses import dataclass, field

# Characters that carry no visible width. They survive copy/paste and then
# confuse everything downstream, so they are dropped rather than folded.
_INVISIBLE = frozenset(
    "​‌‍⁠﻿­᠎͏"
)

# Typographic characters -> what a person typing on a keyboard would produce.
_FOLD = {
    "‘": "'", "’": "'", "‚": "'", "‛": "'",
    "“": '"', "”": '"', "„": '"', "‟": '"',
    "′": "'", "″": '"',
    "–": "-", "—": "--", "―": "--", "−": "-",
    "‐": "-", "‑": "-",
    "…": "...",
    " ": " ", " ": " ", " ": " ", " ": " ",
    " ": " ", " ": " ", " ": " ", " ": " ",
    " ": " ", " ": " ", " ": " ", "　": " ",
    "«": '"', "»": '"', "‹": "'", "›": "'",
    "•": "-", "·": "-", "⁃": "-",
    "×": "x", "÷": "/", "⁄": "/",
    "`": "`", "´": "'",
    "™": "(tm)", "®": "(r)", "©": "(c)",
    "←": "<-", "→": "->", "⇒": "=>", "≤": "<=",
    "≥": ">=", "≠": "!=",
}


@dataclass(slots=True)
class Normalized:
    """Normalized text plus a record of what had to change to get there."""

    text: str
    folded: dict[str, str] = field(default_factory=dict)
    dropped: dict[str, int] = field(default_factory=dict)

    @property
    def changed(self) -> bool:
        return bool(self.folded or self.dropped)

    def summary(self) -> str:
        """Human-readable account of the substitutions, for the UI/CLI."""
        bits: list[str] = []
        if self.folded:
            shown = ", ".join(
                f"{_name(src)} -> {dst!r}" for src, dst in list(self.folded.items())[:6]
            )
            more = len(self.folded) - 6
            bits.append(f"folded {shown}" + (f" (+{more} more)" if more > 0 else ""))
        if self.dropped:
            total = sum(self.dropped.values())
            bits.append(f"dropped {total} zero-width character(s)")
        return "; ".join(bits)


def _name(ch: str) -> str:
    try:
        return unicodedata.name(ch)
    except ValueError:
        return f"U+{ord(ch):04X}"


def normalize(
    raw: str,
    *,
    fold_typography: bool = True,
    expand_tabs: int = 0,
    strip_trailing_ws: bool = True,
) -> Normalized:
    """Prepare text for typing.

    Line endings are always normalized to ``\\n``: a literal carriage return is
    a keystroke no one wants and CRLF would type Enter twice.

    ``expand_tabs`` of 0 keeps tab characters (typed as the Tab key). Set it to
    a width when the target treats Tab as "move to next field" rather than
    "insert whitespace", which is the usual reason typing into a web form goes
    sideways.
    """
    folded: dict[str, str] = {}
    dropped: dict[str, int] = {}

    text = raw.replace("\r\n", "\n").replace("\r", "\n")

    out: list[str] = []
    for ch in text:
        if ch in _INVISIBLE:
            dropped[ch] = dropped.get(ch, 0) + 1
            continue
        if fold_typography:
            sub = _FOLD.get(ch)
            if sub is not None:
                folded[ch] = sub
                out.append(sub)
                continue
        out.append(ch)
    text = "".join(out)

    if expand_tabs > 0:
        text = text.expandtabs(expand_tabs)

    if strip_trailing_ws:
        text = "\n".join(line.rstrip() for line in text.split("\n"))

    return Normalized(text=text, folded=folded, dropped=dropped)


def stats(text: str) -> tuple[int, int, int]:
    """Return ``(characters, words, lines)`` for display."""
    if not text:
        return 0, 0, 0
    return len(text), len(text.split()), text.count("\n") + 1
