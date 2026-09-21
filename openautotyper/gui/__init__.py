"""Graphical interface (PySide6). Imported lazily so the CLI needs no Qt."""

from __future__ import annotations

import shutil
import subprocess
import sys

_NO_QT = (
    "OpenAutoTyper's window needs PySide6, which is not installed.\n\n"
    "    pip install 'openautotyper[gui]'\n\n"
    "The command line works without it:  openautotyper --help"
)


def _report_missing_qt(detail: str) -> None:
    """Say why the window did not open, somewhere the user will actually see it.

    A desktop launcher runs with Terminal=false, so stderr goes nowhere: an
    ImportError here is indistinguishable from the program being broken. Try
    the session's own dialog tools first and fall back to stderr for anyone
    who did start us from a shell.
    """
    print(f"{_NO_QT}\n\n({detail})", file=sys.stderr)
    for argv in (
        ["kdialog", "--title", "OpenAutoTyper", "--sorry", _NO_QT],
        ["zenity", "--error", "--title=OpenAutoTyper", f"--text={_NO_QT}"],
        ["xmessage", "-center", _NO_QT],
        ["notify-send", "OpenAutoTyper", _NO_QT],
    ):
        if shutil.which(argv[0]) is None:
            continue
        try:
            subprocess.run(argv, check=False, timeout=120)
        except (OSError, subprocess.SubprocessError):
            continue
        return


def run_gui() -> int:
    try:
        from .app import run
    except ImportError as exc:  # PySide6 absent, or a broken Qt install
        _report_missing_qt(str(exc))
        return 1
    return run()


__all__ = ["run_gui"]
