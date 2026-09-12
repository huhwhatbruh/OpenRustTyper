"""Graphical interface (PySide6). Imported lazily so the CLI needs no Qt."""

from __future__ import annotations


def run_gui() -> int:
    from .app import run

    return run()


__all__ = ["run_gui"]
