"""GUI smoke tests. Skipped entirely when Qt is not installed."""

from __future__ import annotations

import os

import pytest

pytest.importorskip("PySide6", reason="GUI extra not installed")
os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

from PySide6.QtWidgets import QApplication

from openautotyper.config import Settings
from openautotyper.gui import theme
from openautotyper.gui.app import PRESETS, MainWindow


@pytest.fixture(scope="module")
def app():
    application = QApplication.instance() or QApplication([])
    yield application


@pytest.fixture
def window(app, tmp_path, monkeypatch):
    # Redirect the config location rather than stubbing Settings.load, which
    # would also disable the loading the tests themselves rely on.
    monkeypatch.setattr("openautotyper.config.config_path",
                        lambda: tmp_path / "config.toml")
    win = MainWindow()
    yield win
    win.close()


def test_window_builds(window) -> None:
    assert window.windowTitle() == "OpenAutoTyper"
    assert window.start_button.isEnabled()
    assert not window.stop_button.isEnabled()


def test_estimate_matches_the_engine(window) -> None:
    window.editor.setPlainText("x" * 500)
    window.wpm_spin.setValue(100)
    window._refresh_meta()
    # 500 chars / 5 = 100 words at 100 wpm = 1 minute.
    assert "1m 00s" in window.meta_label.text()
    assert "500 chars" in window.meta_label.text()


def test_typography_is_folded_before_counting(window) -> None:
    window.editor.setPlainText("a — b")          # em dash folds to two chars
    assert window._normalized_text() == "a -- b"


def test_presets_round_trip_through_the_widgets(window) -> None:
    for name, values in PRESETS.items():
        window._apply_preset(name)
        collected = window._collect_settings()
        assert collected.wpm == values["wpm"], name
        assert collected.variance == pytest.approx(values["variance"]), name
        assert window.preset_buttons[name].isChecked(), name


def test_every_slider_shows_a_value_label(window) -> None:
    """A slider left at its default emits nothing, so labels need seeding."""
    window.rhythm_toggle.setChecked(True)
    grid = window.advanced.findChildren(type(window.meta_label))
    blanks = [w.objectName() for w in grid
              if w.objectName() == "faint" and w.width() == 52 and not w.text()]
    assert not blanks


def test_settings_survive_the_widget_round_trip(window, tmp_path) -> None:
    window.wpm_spin.setValue(133)
    window.typo_slider.setValue(45)
    window.tabs_spin.setValue(4)
    settings = window._collect_settings()
    assert settings.wpm == 133
    assert settings.typo_rate == pytest.approx(0.045)
    assert settings.expand_tabs == 4
    path = tmp_path / "c.toml"
    assert settings.save(path)
    assert Settings.load(path).wpm == 133


def test_starting_with_no_text_warns_instead_of_running(window, monkeypatch) -> None:
    warnings: list[str] = []
    monkeypatch.setattr(window, "_warn", lambda t, m: warnings.append(t))
    window.editor.clear()
    window._start()
    assert warnings == ["Nothing to type"]
    assert window.session is None


def test_both_palettes_produce_a_complete_stylesheet() -> None:
    for dark in (True, False):
        sheet = theme.stylesheet(theme.for_system(dark))
        assert "QPlainTextEdit" in sheet and "QPushButton" in sheet
        assert "{{" not in sheet and "}}" not in sheet  # no unescaped braces


def test_the_window_does_not_promise_an_abort_key_it_cannot_deliver(
    window, monkeypatch
) -> None:
    """Typing needs /dev/uinput; stopping needs /dev/input/event*.

    Different permissions, so the window can be able to type and unable to
    stop. It used to say "Press Esc at any time to stop" either way.
    """
    import openautotyper.panic as panic

    monkeypatch.setattr(panic, "available", lambda: (False, "no readable devices"))
    window._refresh_footer()
    text = window.status_label.text()
    assert "No abort key" in text and "no readable devices" in text
    assert "Press Esc" not in text

    monkeypatch.setattr(panic, "available", lambda: (True, "watching 3 device(s)"))
    window._refresh_footer()
    assert "stop" in window.status_label.text().lower()
