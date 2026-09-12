"""Colour tokens, resolved once against the system theme."""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True, slots=True)
class Palette:
    bg: str
    surface: str
    surface_alt: str
    border: str
    text: str
    muted: str
    faint: str
    accent: str
    accent_hover: str
    accent_text: str
    ok: str
    warn: str
    danger: str
    dark: bool


DARK = Palette(
    bg="#16161e", surface="#1d1d26", surface_alt="#24242f", border="#33333f",
    text="#e4e4ef", muted="#9a9ab0", faint="#6a6a80",
    accent="#7aa2f7", accent_hover="#8fb3ff", accent_text="#0d1117",
    ok="#7fd88f", warn="#e0af68", danger="#f7768e", dark=True,
)

LIGHT = Palette(
    bg="#f6f6f8", surface="#ffffff", surface_alt="#eeeef2", border="#d8d8e0",
    text="#1c1c24", muted="#5c5c70", faint="#8a8a9c",
    accent="#3b5fd4", accent_hover="#2f4fbb", accent_text="#ffffff",
    ok="#2f8f4e", warn="#a06a10", danger="#c4384f", dark=False,
)


def for_system(is_dark: bool) -> Palette:
    return DARK if is_dark else LIGHT


def stylesheet(p: Palette) -> str:
    """One stylesheet for the whole window; no per-widget styling elsewhere."""
    return f"""
    QWidget {{
        background: {p.bg};
        color: {p.text};
        font-size: 13px;
    }}
    QLabel#title {{ font-size: 19px; font-weight: 600; }}
    QLabel#muted  {{ color: {p.muted}; }}
    QLabel#faint  {{ color: {p.faint}; font-size: 12px; }}
    QLabel#meta   {{ color: {p.muted}; font-size: 12px; }}
    QLabel#sectionLabel {{
        color: {p.faint}; font-size: 11px; font-weight: 600;
        letter-spacing: 1px; text-transform: uppercase;
    }}

    /* Containers that only group other widgets must not paint over the card
       or the window they sit in. */
    QWidget#advanced, QWidget#editorBox, QWidget#footerBox {{
        background: transparent;
    }}

    QFrame#card {{
        background: {p.surface};
        border: 1px solid {p.border};
        border-radius: 10px;
    }}
    QFrame#chip {{
        background: {p.surface_alt};
        border: 1px solid {p.border};
        border-radius: 11px;
    }}

    QPlainTextEdit {{
        background: {p.surface};
        border: 1px solid {p.border};
        border-radius: 10px;
        padding: 10px;
        selection-background-color: {p.accent};
        selection-color: {p.accent_text};
        font-family: ui-monospace, "JetBrains Mono", "Fira Code", monospace;
        font-size: 13px;
        line-height: 150%;
    }}
    QPlainTextEdit:focus {{ border-color: {p.accent}; }}

    QPushButton {{
        background: {p.surface_alt};
        border: 1px solid {p.border};
        border-radius: 8px;
        padding: 8px 16px;
        font-weight: 500;
    }}
    QPushButton:hover:enabled {{ border-color: {p.accent}; }}
    QPushButton:disabled {{ color: {p.faint}; background: {p.surface}; }}
    QPushButton#primary {{
        background: {p.accent}; color: {p.accent_text};
        border: 1px solid {p.accent}; font-weight: 600;
    }}
    QPushButton#primary:hover:enabled {{ background: {p.accent_hover}; }}
    QPushButton#primary:disabled {{ background: {p.surface}; color: {p.faint};
                                    border-color: {p.border}; }}
    QPushButton#danger {{ color: {p.danger}; }}
    QPushButton#danger:hover:enabled {{ border-color: {p.danger}; }}
    QPushButton#link {{
        background: transparent; border: none; color: {p.muted};
        padding: 4px 6px; text-align: left;
    }}
    QPushButton#link:hover {{ color: {p.accent}; }}
    QPushButton#preset {{ padding: 5px 12px; font-size: 12px; }}
    QPushButton#preset:checked {{
        background: {p.accent}; color: {p.accent_text}; border-color: {p.accent};
    }}

    QSlider::groove:horizontal {{
        height: 4px; background: {p.surface_alt};
        border: none; border-radius: 2px;
    }}
    QSlider::sub-page:horizontal {{ background: {p.accent}; border-radius: 2px; }}
    QSlider::handle:horizontal {{
        background: {p.text}; width: 14px; height: 14px;
        margin: -6px 0; border-radius: 7px;
    }}
    QSlider::handle:horizontal:hover {{ background: {p.accent}; }}

    QSpinBox, QDoubleSpinBox, QComboBox {{
        background: {p.surface_alt};
        border: 1px solid {p.border};
        border-radius: 7px; padding: 5px 8px; min-height: 18px;
    }}
    QSpinBox:focus, QDoubleSpinBox:focus, QComboBox:focus {{ border-color: {p.accent}; }}
    /* Subcontrols are deliberately left unstyled: styling ::drop-down stops
       Qt drawing the arrow, and a stylesheet triangle renders as a square. */
    QComboBox QAbstractItemView {{
        background: {p.surface}; border: 1px solid {p.border};
        selection-background-color: {p.accent}; selection-color: {p.accent_text};
        outline: none;
    }}

    QProgressBar {{
        background: {p.surface_alt}; border: none;
        border-radius: 4px; height: 8px; text-align: center;
    }}
    QProgressBar::chunk {{ background: {p.accent}; border-radius: 4px; }}

    QCheckBox::indicator {{
        width: 15px; height: 15px; border-radius: 4px;
        border: 1px solid {p.border}; background: {p.surface_alt};
    }}
    QCheckBox::indicator:checked {{ background: {p.accent}; border-color: {p.accent}; }}

    QScrollBar:vertical {{ background: transparent; width: 10px; margin: 2px; }}
    QScrollBar::handle:vertical {{
        background: {p.border}; border-radius: 4px; min-height: 30px;
    }}
    QScrollBar::handle:vertical:hover {{ background: {p.faint}; }}
    QScrollBar::add-line, QScrollBar::sub-line {{ height: 0; }}
    QScrollBar::add-page, QScrollBar::sub-page {{ background: none; }}

    QToolTip {{
        background: {p.surface_alt}; color: {p.text};
        border: 1px solid {p.border}; padding: 5px 7px; border-radius: 6px;
    }}
    """
