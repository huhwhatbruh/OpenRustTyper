"""Normalization: what arrives on the clipboard is not what a keyboard makes."""

from __future__ import annotations

from openautotyper.text import normalize, stats


def test_line_endings_are_always_unix() -> None:
    assert normalize("a\r\nb\rc").text == "a\nb\nc"


def test_smart_typography_folds_to_ascii() -> None:
    result = normalize("“quoted” — it’s fine…")
    assert result.text == '"quoted" -- it\'s fine...'
    assert result.changed


def test_zero_width_characters_are_dropped() -> None:
    result = normalize("a​b﻿c")
    assert result.text == "abc"
    assert sum(result.dropped.values()) == 2


def test_non_breaking_space_becomes_a_real_space() -> None:
    assert normalize("a b").text == "a b"


def test_folding_can_be_switched_off() -> None:
    assert normalize("“x”", fold_typography=False).text == "“x”"


def test_tabs_survive_by_default_and_expand_on_request() -> None:
    assert normalize("a\tb").text == "a\tb"
    assert normalize("a\tb", expand_tabs=4).text == "a   b"


def test_trailing_whitespace_is_stripped_per_line() -> None:
    assert normalize("a   \nb\t\n").text == "a\nb\n"


def test_clean_text_reports_no_changes() -> None:
    result = normalize("Plain ASCII text.")
    assert not result.changed
    assert result.summary() == ""


def test_stats() -> None:
    assert stats("") == (0, 0, 0)
    assert stats("one two\nthree") == (13, 3, 2)
