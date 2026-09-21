"""The desktop entry and the package that ships it.

A launcher is the one part of the program that gets no error channel: it runs
with ``Terminal=false``, so anything it prints is discarded and a failure looks
identical to the program being broken. These tests check the two ways that
happened -- a menu entry pointing at a program whose dependencies the package
did not install, and an ``ImportError`` with nowhere to go.
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

PACKAGING = Path(__file__).resolve().parent.parent / "packaging"
DESKTOP = PACKAGING / "openautotyper.desktop"
PKGBUILD = PACKAGING / "PKGBUILD"


def _entries() -> dict[str, str]:
    out: dict[str, str] = {}
    for line in DESKTOP.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith(("#", "[")):
            continue
        key, _, value = line.partition("=")
        out[key] = value
    return out


def test_the_desktop_file_starts_with_its_group_header() -> None:
    """A stray shebang before [Desktop Entry] is not a comment, it is line 1."""
    assert DESKTOP.read_text().splitlines()[0] == "[Desktop Entry]"


@pytest.mark.skipif(
    shutil.which("desktop-file-validate") is None,
    reason="desktop-file-utils not installed",
)
def test_the_desktop_file_validates() -> None:
    done = subprocess.run(
        ["desktop-file-validate", str(DESKTOP)],
        capture_output=True, text=True, check=False,
    )
    assert done.returncode == 0, done.stdout + done.stderr


def test_the_launcher_hides_itself_when_the_program_is_absent() -> None:
    """TryExec is the only thing between a missing GUI and a dead menu entry."""
    entries = _entries()
    assert entries["TryExec"] == entries["Exec"].split()[0]


def test_the_icon_the_entry_names_is_one_the_package_ships() -> None:
    icon = _entries()["Icon"]
    assert (PACKAGING / f"{icon}.svg").is_file(), icon


def test_the_arch_package_installs_everything_the_entry_references() -> None:
    """A desktop entry is a promise that clicking it starts something."""
    pkgbuild = PKGBUILD.read_text()
    depends = pkgbuild.split("depends=(", 1)[1].split(")", 1)[0]
    # The GUI is what the launcher launches, so Qt cannot be an optdepend in a
    # package that ships the launcher.
    assert "pyside6" in depends, "the desktop entry starts the GUI; Qt must be a depend"
    for shipped in ("openautotyper.desktop", "openautotyper.svg",
                    "99-openautotyper-uinput.rules"):
        assert shipped in pkgbuild, shipped


def test_a_missing_qt_is_reported_rather_than_traced_back(monkeypatch, capsys) -> None:
    """The reported bug: clicking the menu entry did nothing whatsoever."""
    import sys

    import openautotyper.gui as gui

    # None in sys.modules is how the import machinery spells "this module is
    # unavailable"; it makes the lazy import raise exactly as a missing Qt does.
    monkeypatch.setitem(sys.modules, "openautotyper.gui.app", None)
    # No dialog is to open on whoever is running the tests.
    monkeypatch.setattr(gui.shutil, "which", lambda _name: None)

    assert gui.run_gui() == 1
    assert "PySide6" in capsys.readouterr().err
