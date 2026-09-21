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


def test_a_key_this_build_does_not_know_survives_a_save(tmp_path) -> None:
    """The docstring promised this and the code did the opposite.

    The GUI calls save() on every window close, so opening and closing it once
    was enough to silently delete a setting written by a newer release or added
    by hand.
    """
    path = tmp_path / "config.toml"
    path.write_text(
        'wpm = 90\nfuture_setting = "keep me"\nfuture_count = 7\n'
        "future_flag = true\n",
        encoding="utf-8",
    )

    settings = Settings.load(path)
    assert settings.wpm == 90.0
    assert settings.unknown == {
        "future_setting": "keep me", "future_count": 7, "future_flag": True,
    }

    assert settings.save(path)
    reloaded = Settings.load(path)
    assert reloaded.wpm == 90.0
    assert reloaded.unknown == settings.unknown


def test_preserved_keys_are_not_mistaken_for_settings(tmp_path) -> None:
    """They round-trip, but they must not reach the cadence or the widgets."""
    path = tmp_path / "config.toml"
    path.write_text('wpm = 90\nwpm_typo = 999\n', encoding="utf-8")
    settings = Settings.load(path)
    assert settings.cadence().wpm == 90.0
    assert "wpm_typo" not in {f.name for f in __import__("dataclasses").fields(Settings)}


def test_a_config_with_no_surprises_writes_no_extra_section(tmp_path) -> None:
    path = tmp_path / "config.toml"
    Settings(wpm=85).save(path)
    text = path.read_text(encoding="utf-8")
    assert "does not understand" not in text
    assert Settings.load(path).unknown == {}


def test_a_string_with_quotes_survives_the_round_trip(tmp_path) -> None:
    path = tmp_path / "config.toml"
    Settings(backend='he said "uinput"').save(path)
    assert Settings.load(path).backend == 'he said "uinput"'
