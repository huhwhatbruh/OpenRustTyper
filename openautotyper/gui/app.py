"""The graphical interface.

Two rules shape this file. Progress from the typing thread arrives as a Qt
signal, never as a direct widget call, because touching widgets off the GUI
thread is how a toolkit ends up crashing minutes into a long job. And nothing
expensive runs on a keystroke: the estimate is closed-form arithmetic, and the
schedule -- a quarter of a second of work for a novel-length document -- is
built once, when you press Start.
"""

from __future__ import annotations

import sys
from pathlib import Path

from PySide6.QtCore import QObject, Qt, QTimer, Signal, Slot
from PySide6.QtGui import QGuiApplication, QKeySequence, QShortcut
from PySide6.QtWidgets import (
    QAbstractSpinBox,
    QApplication,
    QCheckBox,
    QComboBox,
    QDoubleSpinBox,
    QFileDialog,
    QFrame,
    QGridLayout,
    QHBoxLayout,
    QLabel,
    QMainWindow,
    QMessageBox,
    QPlainTextEdit,
    QProgressBar,
    QPushButton,
    QSizePolicy,
    QSlider,
    QSpinBox,
    QVBoxLayout,
    QWidget,
)

from .. import __version__, clipboard
from ..backends import BackendError, report, select
from ..config import Settings
from ..session import Progress, State, TypeSession
from ..text import normalize, stats
from ..timing import format_duration
from . import theme

#: Named speed settings. Each is a full cadence, not just a WPM.
PRESETS: dict[str, dict[str, float]] = {
    "Careful": dict(wpm=45, variance=0.34, rhythm=1.0, hesitation=0.05,
                    sentence_pause=0.75, max_pause=2.6, drift=1.0),
    "Natural": dict(wpm=70, variance=0.30, rhythm=1.0, hesitation=0.03,
                    sentence_pause=0.55, max_pause=1.8, drift=1.0),
    "Brisk":   dict(wpm=110, variance=0.26, rhythm=0.9, hesitation=0.02,
                    sentence_pause=0.35, max_pause=1.1, drift=0.8),
    "Machine": dict(wpm=200, variance=0.0, rhythm=0.0, hesitation=0.0,
                    sentence_pause=0.0, max_pause=0.0, drift=0.0),
}

PANIC_KEYS = [
    ("Esc", "KEY_ESC"), ("F12", "KEY_F12"), ("F9", "KEY_F9"),
    ("Scroll Lock", "KEY_SCROLLLOCK"), ("Pause", "KEY_PAUSE"),
]


class _Bridge(QObject):
    """Carries progress from the typing thread into the GUI thread."""

    progress = Signal(object)


class MainWindow(QMainWindow):
    def __init__(self) -> None:
        super().__init__()
        self.settings = Settings.load()
        self.session: TypeSession | None = None
        self._bridge = _Bridge()
        self._bridge.progress.connect(self._on_progress)
        self._loading = False

        self.palette_ = theme.for_system(self._system_is_dark())
        self.setStyleSheet(theme.stylesheet(self.palette_))
        self.setWindowTitle("OpenAutoTyper")
        self.resize(880, 760)
        self.setMinimumSize(640, 560)

        self._build()
        self._load_settings_into_widgets()
        self._refresh_meta()

        # Counting words means splitting the whole document, so it waits for a
        # gap in typing rather than running on every keypress.
        self._meta_timer = QTimer(self)
        self._meta_timer.setSingleShot(True)
        self._meta_timer.setInterval(180)
        self._meta_timer.timeout.connect(self._refresh_meta)

    # -- construction ------------------------------------------------------
    @staticmethod
    def _system_is_dark() -> bool:
        try:
            return QGuiApplication.styleHints().colorScheme() == Qt.ColorScheme.Dark
        except AttributeError:
            return QGuiApplication.palette().window().color().lightness() < 128

    def _build(self) -> None:
        root = QWidget()
        outer = QVBoxLayout(root)
        outer.setContentsMargins(18, 16, 18, 14)
        outer.setSpacing(12)
        outer.addLayout(self._build_header())
        outer.addWidget(self._build_editor(), 1)
        outer.addWidget(self._build_controls())
        outer.addWidget(self._build_footer())
        self.setCentralWidget(root)

        QShortcut(QKeySequence("Ctrl+Return"), self, self._start)
        QShortcut(QKeySequence("Ctrl+O"), self, self._load_file)
        QShortcut(QKeySequence("Escape"), self, self._stop_if_running)

    def _build_header(self) -> QHBoxLayout:
        row = QHBoxLayout()
        row.setSpacing(10)

        title = QLabel("OpenAutoTyper")
        title.setObjectName("title")
        row.addWidget(title)

        version = QLabel(f"v{__version__}")
        version.setObjectName("faint")
        row.addWidget(version)
        row.addStretch(1)

        self.backend_chip = QLabel()
        self.backend_chip.setObjectName("muted")
        chip = QFrame()
        chip.setObjectName("chip")
        chip_layout = QHBoxLayout(chip)
        chip_layout.setContentsMargins(10, 4, 10, 4)
        chip_layout.addWidget(self.backend_chip)
        row.addWidget(chip)

        doctor = QPushButton("Diagnostics")
        doctor.setObjectName("link")
        doctor.clicked.connect(self._show_diagnostics)
        row.addWidget(doctor)
        return row

    def _build_editor(self) -> QWidget:
        box = QWidget()
        box.setObjectName("editorBox")
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(6)

        self.editor = QPlainTextEdit()
        self.editor.setPlaceholderText(
            "Paste or type the text here, focus the target window, then press Start."
        )
        self.editor.setTabChangesFocus(False)
        self.editor.textChanged.connect(self._on_text_changed)
        layout.addWidget(self.editor, 1)

        row = QHBoxLayout()
        row.setSpacing(4)
        for label, slot in (
            ("Open file…", self._load_file),
            ("From clipboard", self._paste_clipboard),
            ("Clear", self._clear),
        ):
            button = QPushButton(label)
            button.setObjectName("link")
            button.clicked.connect(slot)
            row.addWidget(button)
        row.addStretch(1)

        self.meta_label = QLabel()
        self.meta_label.setObjectName("meta")
        row.addWidget(self.meta_label)
        layout.addLayout(row)
        return box

    def _build_controls(self) -> QWidget:
        card = QFrame()
        card.setObjectName("card")
        layout = QVBoxLayout(card)
        layout.setContentsMargins(16, 14, 16, 14)
        layout.setSpacing(12)

        # -- speed ---------------------------------------------------------
        speed = QHBoxLayout()
        speed.setSpacing(10)
        speed.addWidget(QLabel("Speed"))

        self.wpm_slider = QSlider(Qt.Orientation.Horizontal)
        self.wpm_slider.setRange(10, 300)
        self.wpm_slider.setToolTip("Words per minute, measured end to end")
        self.wpm_slider.valueChanged.connect(self._on_wpm_slider)
        speed.addWidget(self.wpm_slider, 1)

        self.wpm_spin = QSpinBox()
        self.wpm_spin.setRange(1, 2000)
        self.wpm_spin.setSuffix(" wpm")
        self.wpm_spin.setFixedWidth(92)
        self.wpm_spin.setButtonSymbols(QAbstractSpinBox.ButtonSymbols.NoButtons)
        self.wpm_spin.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.wpm_spin.valueChanged.connect(self._on_wpm_spin)
        speed.addWidget(self.wpm_spin)

        speed.addSpacing(12)
        speed.addWidget(QLabel("Start delay"))
        self.delay_spin = QDoubleSpinBox()
        self.delay_spin.setRange(0.0, 120.0)
        self.delay_spin.setDecimals(1)
        self.delay_spin.setSingleStep(0.5)
        self.delay_spin.setSuffix(" s")
        self.delay_spin.setFixedWidth(80)
        self.delay_spin.setButtonSymbols(QAbstractSpinBox.ButtonSymbols.NoButtons)
        self.delay_spin.setAlignment(Qt.AlignmentFlag.AlignCenter)
        self.delay_spin.setToolTip("Time to click into the target window before typing begins")
        speed.addWidget(self.delay_spin)
        layout.addLayout(speed)

        # -- presets -------------------------------------------------------
        presets = QHBoxLayout()
        presets.setSpacing(6)
        presets.addWidget(QLabel("Preset"))
        self.preset_buttons: dict[str, QPushButton] = {}
        for name in PRESETS:
            button = QPushButton(name)
            button.setObjectName("preset")
            button.setCheckable(True)
            button.clicked.connect(lambda _checked, n=name: self._apply_preset(n))
            presets.addWidget(button)
            self.preset_buttons[name] = button
        presets.addStretch(1)

        self.rhythm_toggle = QPushButton("Rhythm and safety  ▸")
        self.rhythm_toggle.setObjectName("link")
        self.rhythm_toggle.setCheckable(True)
        self.rhythm_toggle.toggled.connect(self._toggle_advanced)
        presets.addWidget(self.rhythm_toggle)
        layout.addLayout(presets)

        layout.addWidget(self._build_advanced())
        return card

    def _slider_row(
        self, grid: QGridLayout, row: int, col: int, label: str,
        lo: int, hi: int, tip: str, fmt,
    ) -> tuple[QSlider, QLabel]:
        grid.addWidget(QLabel(label), row, col * 3)
        slider = QSlider(Qt.Orientation.Horizontal)
        slider.setRange(lo, hi)
        slider.setToolTip(tip)
        slider.setMinimumWidth(110)
        grid.addWidget(slider, row, col * 3 + 1)
        value = QLabel()
        value.setObjectName("faint")
        value.setFixedWidth(52)
        value.setAlignment(Qt.AlignmentFlag.AlignRight | Qt.AlignmentFlag.AlignVCenter)
        grid.addWidget(value, row, col * 3 + 2)
        slider.valueChanged.connect(lambda v: value.setText(fmt(v)))
        # setValue() emits nothing when the value is already the default, so a
        # slider left at zero would otherwise show a blank label forever.
        value.setText(fmt(slider.value()))
        return slider, value

    def _build_advanced(self) -> QWidget:
        self.advanced = QWidget()
        self.advanced.setObjectName("advanced")
        layout = QVBoxLayout(self.advanced)
        layout.setContentsMargins(0, 4, 0, 0)
        layout.setSpacing(10)

        caption = QLabel(
            "These change the character of the typing, not its speed — the "
            "words-per-minute above is delivered either way."
        )
        caption.setObjectName("faint")
        caption.setWordWrap(True)
        layout.addWidget(caption)

        grid = QGridLayout()
        grid.setHorizontalSpacing(10)
        grid.setVerticalSpacing(8)
        grid.setColumnStretch(1, 1)
        grid.setColumnStretch(4, 1)
        def pct(value: int) -> str:
            return f"{value}%"

        self.variance_slider, _ = self._slider_row(
            grid, 0, 0, "Variation", 0, 100,
            "Spread of the gaps between keystrokes", pct)
        self.rhythm_slider, _ = self._slider_row(
            grid, 1, 0, "Finger travel", 0, 100,
            "How much awkward letter pairs slow the typing down", pct)
        self.hesitation_slider, _ = self._slider_row(
            grid, 2, 0, "Hesitation", 0, 20,
            "Share of keystrokes preceded by a visible hitch",
            lambda v: f"{v}%")
        self.pause_slider, _ = self._slider_row(
            grid, 0, 1, "Pauses", 0, 100,
            "Chance of pausing at the end of a sentence or paragraph", pct)
        self.maxpause_slider, _ = self._slider_row(
            grid, 1, 1, "Longest pause", 0, 60,
            "Upper bound on a single pause",
            lambda v: f"{v / 10:.1f}s")
        self.drift_slider, _ = self._slider_row(
            grid, 2, 1, "Speed drift", 0, 100,
            "How much the pace wanders over long passages", pct)
        self.typo_slider, _ = self._slider_row(
            grid, 3, 0, "Slips", 0, 100,
            "Mistyped keys, always corrected with a backspace. "
            "The finished text is identical either way.",
            lambda v: f"{v / 10:.1f}%")
        layout.addLayout(grid)

        # -- safety and delivery -------------------------------------------
        safety = QHBoxLayout()
        safety.setSpacing(8)
        safety.addWidget(QLabel("Abort key"))
        self.panic_combo = QComboBox()
        for label, key in PANIC_KEYS:
            self.panic_combo.addItem(label, key)
        self.panic_combo.setToolTip(
            "Pressing this on a real keyboard stops typing immediately, "
            "whatever window has focus"
        )
        self.panic_combo.currentIndexChanged.connect(self._refresh_footer)
        safety.addWidget(self.panic_combo)

        self.any_key_check = QCheckBox("or any key at all")
        self.any_key_check.setToolTip("Stop the moment you touch the keyboard")
        self.any_key_check.toggled.connect(self._refresh_footer)
        safety.addWidget(self.any_key_check)

        safety.addSpacing(16)
        safety.addWidget(QLabel("Backend"))
        self.backend_combo = QComboBox()
        self.backend_combo.addItem("Automatic", "")
        for name, ok, reason in report():
            if name == "null":
                continue
            self.backend_combo.addItem(f"{name}{'' if ok else '  (unavailable)'}", name)
            index = self.backend_combo.count() - 1
            self.backend_combo.setItemData(index, reason, Qt.ItemDataRole.ToolTipRole)
            if not ok:
                self.backend_combo.model().item(index).setEnabled(False)
        safety.addWidget(self.backend_combo)
        safety.addStretch(1)
        layout.addLayout(safety)

        text_row = QHBoxLayout()
        text_row.setSpacing(8)
        self.fold_check = QCheckBox("Fold smart quotes and dashes to ASCII")
        self.fold_check.setToolTip(
            "Curly quotes, em dashes and non-breaking spaces have no key on a "
            "keyboard. Folding them types what a person would have typed; "
            "unfolding sends them through the clipboard instead."
        )
        self.fold_check.toggled.connect(self._refresh_meta)
        text_row.addWidget(self.fold_check)

        text_row.addSpacing(12)
        text_row.addWidget(QLabel("Tabs as"))
        self.tabs_spin = QSpinBox()
        self.tabs_spin.setRange(0, 16)
        self.tabs_spin.setSpecialValueText("Tab key")
        self.tabs_spin.setSuffix(" spaces")
        self.tabs_spin.setButtonSymbols(QAbstractSpinBox.ButtonSymbols.NoButtons)
        self.tabs_spin.setToolTip(
            "Web forms usually treat Tab as 'next field'. Send spaces instead "
            "when that would jump out of the box you are typing into."
        )
        self.tabs_spin.valueChanged.connect(self._refresh_meta)
        text_row.addWidget(self.tabs_spin)
        text_row.addStretch(1)
        layout.addLayout(text_row)

        self.advanced.setVisible(False)
        return self.advanced

    def _build_footer(self) -> QWidget:
        box = QWidget()
        box.setObjectName("footerBox")
        layout = QVBoxLayout(box)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(8)

        self.progress_bar = QProgressBar()
        self.progress_bar.setRange(0, 1000)
        self.progress_bar.setTextVisible(False)
        self.progress_bar.setValue(0)
        layout.addWidget(self.progress_bar)

        row = QHBoxLayout()
        row.setSpacing(8)
        self.status_label = QLabel()
        self.status_label.setObjectName("muted")
        self.status_label.setSizePolicy(
            QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred
        )
        row.addWidget(self.status_label, 1)

        self.start_button = QPushButton("Start typing")
        self.start_button.setObjectName("primary")
        self.start_button.setMinimumWidth(130)
        self.start_button.setToolTip("Ctrl+Enter")
        self.start_button.clicked.connect(self._start)
        row.addWidget(self.start_button)

        self.pause_button = QPushButton("Pause")
        self.pause_button.setEnabled(False)
        self.pause_button.clicked.connect(self._toggle_pause)
        row.addWidget(self.pause_button)

        self.stop_button = QPushButton("Stop")
        self.stop_button.setObjectName("danger")
        self.stop_button.setEnabled(False)
        self.stop_button.clicked.connect(lambda: self._stop("stopped"))
        row.addWidget(self.stop_button)
        layout.addLayout(row)
        return box

    # -- settings <-> widgets ---------------------------------------------
    def _load_settings_into_widgets(self) -> None:
        self._loading = True
        s = self.settings
        self.wpm_spin.setValue(int(s.wpm))
        self.wpm_slider.setValue(max(10, min(300, int(s.wpm))))
        self.delay_spin.setValue(s.countdown)
        self.variance_slider.setValue(int(s.variance * 100))
        self.rhythm_slider.setValue(int(s.rhythm * 100))
        self.hesitation_slider.setValue(int(s.hesitation * 100))
        self.pause_slider.setValue(int(s.sentence_pause * 100))
        self.maxpause_slider.setValue(int(s.max_pause * 10))
        self.drift_slider.setValue(int(s.drift * 100))
        self.typo_slider.setValue(int(s.typo_rate * 1000))
        self.fold_check.setChecked(s.fold_typography)
        self.tabs_spin.setValue(s.expand_tabs)
        self.any_key_check.setChecked(s.any_key_stops)
        index = self.panic_combo.findData(s.panic_key)
        self.panic_combo.setCurrentIndex(index if index >= 0 else 0)
        index = self.backend_combo.findData(s.backend)
        self.backend_combo.setCurrentIndex(index if index >= 0 else 0)
        self._loading = False
        self._sync_preset_buttons()
        self._refresh_backend_chip()
        self._refresh_footer()

    def _collect_settings(self) -> Settings:
        s = self.settings
        s.wpm = float(self.wpm_spin.value())
        s.countdown = self.delay_spin.value()
        s.variance = self.variance_slider.value() / 100
        s.rhythm = self.rhythm_slider.value() / 100
        s.hesitation = self.hesitation_slider.value() / 100
        s.sentence_pause = self.pause_slider.value() / 100
        s.max_pause = self.maxpause_slider.value() / 10
        s.drift = self.drift_slider.value() / 100
        s.typo_rate = self.typo_slider.value() / 1000
        s.fold_typography = self.fold_check.isChecked()
        s.expand_tabs = self.tabs_spin.value()
        s.any_key_stops = self.any_key_check.isChecked()
        s.panic_key = self.panic_combo.currentData() or "KEY_ESC"
        s.backend = self.backend_combo.currentData() or ""
        return s

    def _apply_preset(self, name: str) -> None:
        values = PRESETS[name]
        self._loading = True
        self.wpm_spin.setValue(int(values["wpm"]))
        self.wpm_slider.setValue(max(10, min(300, int(values["wpm"]))))
        self.variance_slider.setValue(int(values["variance"] * 100))
        self.rhythm_slider.setValue(int(values["rhythm"] * 100))
        self.hesitation_slider.setValue(int(values["hesitation"] * 100))
        self.pause_slider.setValue(int(values["sentence_pause"] * 100))
        self.maxpause_slider.setValue(int(values["max_pause"] * 10))
        self.drift_slider.setValue(int(values["drift"] * 100))
        self._loading = False
        self._sync_preset_buttons()
        self._refresh_meta()

    def _sync_preset_buttons(self) -> None:
        current = self._collect_settings()
        for name, button in self.preset_buttons.items():
            values = PRESETS[name]
            button.setChecked(
                abs(current.wpm - values["wpm"]) < 0.5
                and abs(current.variance - values["variance"]) < 0.01
                and abs(current.rhythm - values["rhythm"]) < 0.01
                and abs(current.sentence_pause - values["sentence_pause"]) < 0.01
            )

    # -- editor ------------------------------------------------------------
    def _normalized_text(self) -> str:
        return normalize(
            self.editor.toPlainText(),
            fold_typography=self.fold_check.isChecked(),
            expand_tabs=self.tabs_spin.value(),
            strip_trailing_ws=self.settings.strip_trailing_ws,
        ).text

    def _on_text_changed(self) -> None:
        self._meta_timer.start()

    def _refresh_meta(self) -> None:
        text = self._normalized_text()
        chars, words, lines = stats(text)
        wpm = max(1, self.wpm_spin.value())
        # The cadence model normalizes to exactly this, so the estimate is
        # arithmetic rather than a simulation -- no planning on a keystroke.
        seconds = chars / 5.0 * 60.0 / wpm
        self.meta_label.setText(
            f"{chars:,} chars · {words:,} words · {lines:,} lines · "
            f"~{format_duration(seconds)}"
        )
        if not self._loading:
            self._sync_preset_buttons()

    def _load_file(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self, "Open text file", str(Path.home()),
            "Text files (*.txt *.md *.py *.json *.csv);;All files (*)",
        )
        if not path:
            return
        try:
            self.editor.setPlainText(Path(path).read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError) as exc:
            self._warn("Could not open file", str(exc))
        else:
            self._refresh_meta()

    def _paste_clipboard(self) -> None:
        content = clipboard.read()
        if content is None:
            self._warn("Clipboard unavailable", clipboard.available()[1])
            return
        self.editor.setPlainText(content)
        self._refresh_meta()

    def _clear(self) -> None:
        self.editor.clear()
        self._refresh_meta()

    # -- speed widgets stay in step ---------------------------------------
    def _on_wpm_slider(self, value: int) -> None:
        if self.wpm_spin.value() != value:
            self.wpm_spin.setValue(value)
        self._refresh_meta()

    def _on_wpm_spin(self, value: int) -> None:
        clamped = max(10, min(300, value))
        if self.wpm_slider.value() != clamped:
            self._loading, prior = True, self._loading
            self.wpm_slider.setValue(clamped)
            self._loading = prior
        self._refresh_meta()

    def _toggle_advanced(self, shown: bool) -> None:
        self.advanced.setVisible(shown)
        self.rhythm_toggle.setText(
            "Rhythm and safety  ▾" if shown else "Rhythm and safety  ▸"
        )

    # -- running -----------------------------------------------------------
    def _start(self) -> None:
        if self.session is not None:
            return
        text = self._normalized_text()
        if not text.strip():
            self._warn("Nothing to type", "Put some text in the box first.")
            return

        settings = self._collect_settings()
        try:
            cadence = settings.cadence()
            cadence.validate()
            backend = select(settings.backend or None)
        except (BackendError, ValueError) as exc:
            self._warn("Cannot start", str(exc))
            return

        self.session = TypeSession(
            text, cadence, backend,
            countdown=settings.countdown,
            panic_key=settings.panic_key,
            any_key_stops=settings.any_key_stops,
            on_progress=self._bridge.progress.emit,
        )
        self._set_running(True)
        self.progress_bar.setValue(0)
        self.session.start()

    def _stop(self, reason: str) -> None:
        if self.session is not None:
            self.session.stop(reason)

    def _stop_if_running(self) -> None:
        if self.session is not None:
            self._stop("stopped from the app window")

    def _toggle_pause(self) -> None:
        session = self.session
        if session is None:
            return
        if session.paused:
            session.resume()
            self.pause_button.setText("Pause")
        else:
            session.pause()
            self.pause_button.setText("Resume")

    def _set_running(self, running: bool) -> None:
        self.start_button.setEnabled(not running)
        self.pause_button.setEnabled(running)
        self.stop_button.setEnabled(running)
        self.editor.setReadOnly(running)
        if not running:
            self.pause_button.setText("Pause")

    @Slot(object)
    def _on_progress(self, progress: Progress) -> None:
        if progress.state is State.COUNTDOWN:
            self.status_label.setText(progress.message)
            self.progress_bar.setValue(0)
            return

        if progress.state in (State.TYPING, State.PAUSED):
            self.progress_bar.setValue(int(progress.fraction * 1000))
            tag = "Paused — " if progress.state is State.PAUSED else ""
            self.status_label.setText(
                f"{tag}{progress.typed:,} of {progress.total:,} · "
                f"{format_duration(progress.remaining)} left · "
                f"{progress.wpm:.0f} wpm"
            )
            return

        # Terminal states.
        self.session = None
        self._set_running(False)
        if progress.state is State.DONE:
            self.progress_bar.setValue(1000)
            self.status_label.setText(
                f"Done — {progress.typed:,} characters in "
                f"{format_duration(progress.elapsed)}"
            )
        elif progress.state is State.STOPPED:
            self.status_label.setText(
                f"{progress.message.capitalize()} — {progress.typed:,} of "
                f"{progress.total:,} characters typed"
            )
        else:
            self.progress_bar.setValue(0)
            self.status_label.setText("Failed")
            self._warn("Typing failed", progress.message)

    # -- chrome ------------------------------------------------------------
    def _refresh_backend_chip(self) -> None:
        try:
            name = select(self.settings.backend or None).name
            self.backend_chip.setText(f"via {name}")
            self.backend_chip.setToolTip("Keystrokes are delivered through this backend")
        except BackendError as exc:
            self.backend_chip.setText("no backend")
            self.backend_chip.setToolTip(str(exc))

    def _refresh_footer(self) -> None:
        if self._loading:
            return
        if self.any_key_check.isChecked():
            hint = "Typing stops the moment you touch the keyboard."
        else:
            label = self.panic_combo.currentText()
            hint = f"Press {label} on your keyboard at any time to stop."
        self.status_label.setText(hint)

    def _show_diagnostics(self) -> None:
        from ..backends.xkb import load_layout
        from ..panic import available as panic_available

        lines = ["Backends:"]
        for name, ok, reason in report():
            lines.append(f"  {'OK' if ok else '--'}  {name}: {reason}")
        layout = load_layout()
        lines.append(f"\nKeyboard layout: {layout.source}")
        lines.append(f"  {len(layout)} characters reachable without the clipboard")
        ok, reason = panic_available()
        lines.append(f"\nAbort key: {'OK' if ok else 'unavailable'} — {reason}")
        ok, reason = clipboard.available()
        lines.append(f"Clipboard: {'OK' if ok else 'unavailable'} — {reason}")

        box = QMessageBox(self)
        box.setWindowTitle("Diagnostics")
        box.setText("\n".join(lines))
        box.setIcon(QMessageBox.Icon.NoIcon)
        box.exec()

    def _warn(self, title: str, message: str) -> None:
        box = QMessageBox(self)
        box.setWindowTitle(title)
        box.setText(message)
        box.setIcon(QMessageBox.Icon.Warning)
        box.exec()

    def closeEvent(self, event) -> None:
        if self.session is not None:
            answer = QMessageBox.question(
                self, "Still typing",
                "Typing is still in progress. Stop it and quit?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if answer != QMessageBox.StandardButton.Yes:
                event.ignore()
                return
            self.session.stop("window closed")
            self.session.join(timeout=2.0)
        self._collect_settings().save()
        event.accept()


def run() -> int:
    app = QApplication.instance() or QApplication(sys.argv)
    app.setApplicationName("OpenAutoTyper")
    app.setApplicationDisplayName("OpenAutoTyper")
    app.setDesktopFileName("openautotyper")
    window = MainWindow()
    window.show()
    return app.exec()
