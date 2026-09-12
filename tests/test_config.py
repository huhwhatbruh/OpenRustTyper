"""Settings must survive a round trip and tolerate a damaged file."""

from __future__ import annotations

from pathlib import Path

from openautotyper.config import Settings


def test_round_trip(tmp_path: Path) -> None:
    path = tmp_path / "config.toml"
    original = Settings(wpm=95.5, typo_rate=0.02, backend="uinput",
                        any_key_stops=True, expand_tabs=4)
    assert original.save(path)
    assert Settings.load(path) == original


def test_a_missing_file_gives_defaults(tmp_path: Path) -> None:
    assert Settings.load(tmp_path / "absent.toml") == Settings()


def test_unparseable_file_gives_defaults(tmp_path: Path) -> None:
    path = tmp_path / "config.toml"
    path.write_text("this is not [ valid toml")
    assert Settings.load(path) == Settings()


def test_one_bad_key_does_not_discard_the_others(tmp_path: Path) -> None:
    path = tmp_path / "config.toml"
    path.write_text('wpm = "fast"\ncountdown = 3.0\n')
    loaded = Settings.load(path)
    assert loaded.wpm == Settings().wpm
    assert loaded.countdown == 3.0


def test_out_of_range_values_fall_back_to_defaults(tmp_path: Path) -> None:
    path = tmp_path / "config.toml"
    path.write_text("wpm = 99999\n")
    assert Settings.load(path) == Settings()


def test_unknown_keys_are_ignored(tmp_path: Path) -> None:
    path = tmp_path / "config.toml"
    path.write_text('wpm = 80\nfavourite_colour = "blue"\n')
    assert Settings.load(path).wpm == 80


def test_settings_produce_a_valid_cadence() -> None:
    Settings().cadence().validate()


def test_saving_creates_missing_directories(tmp_path: Path) -> None:
    path = tmp_path / "deep" / "nested" / "config.toml"
    assert Settings(wpm=42).save(path)
    assert Settings.load(path).wpm == 42
