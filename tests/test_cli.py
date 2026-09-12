"""The command line: sources, overrides, and exit codes."""

from __future__ import annotations

import io
from pathlib import Path

import pytest

from openautotyper import cli
from openautotyper.config import Settings


def run(argv: list[str], stdin: str = "") -> tuple[int, str, str]:
    import contextlib

    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = cli.main(argv)
    return code, out.getvalue(), err.getvalue()


def test_dry_run_reproduces_the_input_exactly() -> None:
    code, out, _ = run(["--dry-run", "-t", "Hello, world!", "-q"])
    assert code == 0
    assert out == "Hello, world!"


def test_dry_run_folds_typography_like_a_real_run() -> None:
    code, out, _ = run(["--dry-run", "-t", "a “b” — c…", "-q"])
    assert code == 0
    assert out == 'a "b" -- c...'


def test_no_fold_keeps_the_original_characters() -> None:
    _, out, _ = run(["--dry-run", "--no-fold", "-t", "a — b", "-q"])
    assert out == "a — b"


def test_text_comes_from_a_file(tmp_path: Path) -> None:
    path = tmp_path / "input.txt"
    path.write_text("from a file")
    _, out, _ = run(["--dry-run", "-f", str(path), "-q"])
    assert out == "from a file"


def test_conflicting_sources_are_refused() -> None:
    with pytest.raises(SystemExit):
        run(["--dry-run", "-t", "x", "-f", "y"])


def test_empty_input_is_an_error() -> None:
    code, _, err = run(["--dry-run", "-t", "   ", "-q"])
    assert code == 2
    assert "empty" in err


def test_flat_zeroes_the_rhythm_knobs() -> None:
    settings = Settings()
    args = cli.build_parser().parse_args(["--flat"])
    cadence = cli.apply_overrides(settings, args)
    assert (cadence.variance, cadence.rhythm, cadence.hesitation,
            cadence.sentence_pause, cadence.drift) == (0, 0, 0, 0, 0)


def test_explicit_flags_override_the_saved_settings() -> None:
    settings = Settings(wpm=50, typo_rate=0.0)
    args = cli.build_parser().parse_args(["--wpm", "123", "--typos", "0.05"])
    cadence = cli.apply_overrides(settings, args)
    assert cadence.wpm == 123
    assert cadence.typo_rate == 0.05


def test_flags_beat_flat_when_both_are_given() -> None:
    args = cli.build_parser().parse_args(["--flat", "--variance", "0.5"])
    assert cli.apply_overrides(Settings(), args).variance == 0.5


def test_out_of_range_values_are_refused_before_typing() -> None:
    with pytest.raises(SystemExit):
        run(["--dry-run", "-t", "x", "--typos", "0.9", "-q"])


def test_list_backends_names_every_backend() -> None:
    code, out, _ = run(["--list-backends"])
    assert code == 0
    assert "null" in out and "BACKEND" in out


def test_doctor_reports_without_raising() -> None:
    code, out, _ = run(["--doctor"])
    assert code in (0, 1)
    assert "backends:" in out


def test_the_summary_reports_normalization() -> None:
    _, _, err = run(["--dry-run", "-t", "a “b”"])
    assert "normalized" in err


def test_quiet_suppresses_the_summary() -> None:
    _, _, err = run(["--dry-run", "-t", "a “b”", "-q"])
    assert err == ""


def test_seed_makes_a_run_reproducible() -> None:
    first = run(["--dry-run", "-t", "repeatable text", "--typos", "0.2",
                 "--seed", "5", "-q"])[1]
    second = run(["--dry-run", "-t", "repeatable text", "--typos", "0.2",
                  "--seed", "5", "-q"])[1]
    assert first == second == "repeatable text"
