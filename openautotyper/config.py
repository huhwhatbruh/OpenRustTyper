"""Settings, remembered between runs.

Stored as TOML under the XDG config directory so it can be read, edited and
version-controlled by hand. Unknown keys are preserved on write and bad values
are ignored rather than fatal -- a corrupt config should cost you your
preferences, not the ability to start the program.
"""

from __future__ import annotations

import os
import tomllib
from dataclasses import asdict, dataclass, fields
from pathlib import Path

from .timing import Cadence


def config_dir() -> Path:
    base = os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config")
    return Path(base) / "openautotyper"


def config_path() -> Path:
    return config_dir() / "config.toml"


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

        merged = {}
        for field in fields(cls):
            value = raw.get(field.name)
            if value is None:
                continue
            try:
                if field.type in ("float", float):
                    merged[field.name] = float(value)
                elif field.type in ("int", int):
                    merged[field.name] = int(value)
                elif field.type in ("bool", bool):
                    merged[field.name] = bool(value)
                else:
                    merged[field.name] = str(value)
            except (TypeError, ValueError):
                continue  # one bad key must not discard the rest

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
            for key, value in asdict(self).items():
                if isinstance(value, bool):
                    lines.append(f"{key} = {str(value).lower()}")
                elif isinstance(value, str):
                    lines.append(f'{key} = "{value}"')
                else:
                    lines.append(f"{key} = {value}")
            tmp = path.with_suffix(".toml.tmp")
            tmp.write_text("\n".join(lines) + "\n", encoding="utf-8")
            # Atomic replace: a crash mid-write must not leave a truncated file.
            tmp.replace(path)
            return True
        except OSError:
            return False
