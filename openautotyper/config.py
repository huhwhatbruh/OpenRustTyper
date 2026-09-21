"""Settings, remembered between runs.

Stored as TOML under the XDG config directory so it can be read, edited and
version-controlled by hand. Unknown keys are preserved on write -- the GUI
saves on every window close, so a setting from a newer release, or one added by
hand, must survive being read by a build that does not understand it. Bad values
are ignored rather than fatal: a corrupt config should cost you your
preferences, not the ability to start the program.
"""

from __future__ import annotations

import json
import os
import tomllib
from dataclasses import asdict, dataclass, field, fields
from pathlib import Path

from .timing import Cadence


def config_dir() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return Path(base) / "openautotyper"


def config_path() -> Path:
    return config_dir() / "config.toml"


def _toml_line(key: str, value: object) -> str:
    """One ``key = value`` line. Only the scalar types settings use."""
    if isinstance(value, bool):
        return f"{key} = {str(value).lower()}"
    if isinstance(value, str):
        escaped = value.replace("\\", "\\\\").replace('"', '\\"')
        return f'{key} = "{escaped}"'
    if isinstance(value, (int, float)):
        return f"{key} = {value}"
    # A table or array from a future release: TOML's own repr is not worth
    # reimplementing here, and dropping it would be the bug this exists to fix.
    return f"{key} = {json.dumps(value)}"


@dataclass(slots=True)
class Settings:
    """Everything the UI remembers."""

    # cadence
    wpm: float = 70.0
    variance: float = 0.30
    rhythm: float = 1.0
    hesitation: float = 0.03
    sentence_pause: float = 0.55
    max_pause: float = 1.8
    typo_rate: float = 0.0
    drift: float = 1.0
    # run
    countdown: float = 5.0
    backend: str = ""            # empty means auto-select
    panic_key: str = "KEY_ESC"
    any_key_stops: bool = False
    # text handling
    fold_typography: bool = True
    expand_tabs: int = 0
    strip_trailing_ws: bool = True
    #: Keys found in the file that this build does not recognise, carried
    #: through to the next write. Without this the docstring's promise was
    #: false in the most destructive direction: the GUI saves on every window
    #: close, so opening and closing it once silently deleted a setting added
    #: by hand or by a newer release. Not a field of the settings themselves,
    #: so it is excluded from the TOML body and from ``cadence()``.
    unknown: dict[str, object] = field(default_factory=dict, repr=False)

    def cadence(self) -> Cadence:
        return Cadence(
            wpm=self.wpm, variance=self.variance, rhythm=self.rhythm,
            hesitation=self.hesitation, sentence_pause=self.sentence_pause,
            max_pause=self.max_pause, typo_rate=self.typo_rate, drift=self.drift,
        )

    # -- persistence -------------------------------------------------------
    @classmethod
    def load(cls, path: Path | None = None) -> Settings:
        path = path or config_path()
        try:
            raw = tomllib.loads(path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError, ValueError):
            return cls()

        known = {f.name for f in fields(cls)} - {"unknown"}
        merged: dict[str, object] = {}
        for spec in fields(cls):
            if spec.name == "unknown":
                continue
            value = raw.get(spec.name)
            if value is None:
                continue
            try:
                if spec.type in ("float", float):
                    merged[spec.name] = float(value)
                elif spec.type in ("int", int):
                    merged[spec.name] = int(value)
                elif spec.type in ("bool", bool):
                    merged[spec.name] = bool(value)
                else:
                    merged[spec.name] = str(value)
            except (TypeError, ValueError):
                continue  # one bad key must not discard the rest

        # Anything this build does not know about is somebody else's setting:
        # a newer release's, or one the user added by hand. Carry it.
        merged["unknown"] = {k: v for k, v in raw.items() if k not in known}

        settings = cls(**merged)
        try:
            settings.cadence().validate()
        except ValueError:
            return cls()  # out-of-range values: fall back to defaults wholesale
        return settings

    def save(self, path: Path | None = None) -> bool:
        path = path or config_path()
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            lines = [
                "# OpenAutoTyper settings. Delete this file to reset.",
                "",
            ]
            body = asdict(self)
            unknown = body.pop("unknown", {})
            for key, value in body.items():
                lines.append(_toml_line(key, value))
            if unknown:
                lines += [
                    "",
                    "# Not settings this version understands — from a newer",
                    "# release, or added by hand. Kept so saving cannot eat them.",
                ]
                lines += [_toml_line(key, value) for key, value in unknown.items()]
            tmp = path.with_suffix(".toml.tmp")
            tmp.write_text("\n".join(lines) + "\n", encoding="utf-8")
            # Atomic replace: a crash mid-write must not leave a truncated file.
            tmp.replace(path)
            return True
        except OSError:
            return False
