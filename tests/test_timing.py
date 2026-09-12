"""The cadence model must be honest: the dial says what it does, and what
comes out the far end has to look like a person typing.
"""

from __future__ import annotations

import statistics

import pytest

from openautotyper.timing import (
    Action,
    Cadence,
    duration,
    format_duration,
    plan,
    stroke_stats,
    typing_duration,
)

SAMPLE = (
    "The quick brown fox jumps over the lazy dog. It was a bright cold day in "
    "April, and the clocks were striking thirteen.\n\nA second paragraph, with "
    "a comma; and a semicolon."
)


def presses(events):
    return [e for e in events if e.action is Action.DOWN]


# ---------------------------------------------------------------------------
# Speed
# ---------------------------------------------------------------------------
@pytest.mark.parametrize("wpm", [15, 40, 70, 120, 250, 600])
def test_wpm_is_delivered_exactly(wpm: int) -> None:
    """Whatever shapes the rhythm, the average must hit the requested rate."""
    span = typing_duration(plan(SAMPLE, Cadence(wpm=wpm), seed=7))
    assert len(SAMPLE) / 5.0 / (span / 60.0) == pytest.approx(wpm, rel=1e-6)


@pytest.mark.parametrize(
    "cadence",
    [
        Cadence(wpm=70, variance=0.6, hesitation=0.12),
        Cadence(wpm=70, rhythm=0.0),
        Cadence(wpm=70, max_pause=6.0, sentence_pause=1.0),
        Cadence(wpm=70, typo_rate=0.1),
        Cadence(wpm=70, drift=0.0),
        Cadence(wpm=70, dwell=0.2, rollover=0.0),
        Cadence(wpm=70, dwell_variance=1.0),
    ],
)
def test_shape_knobs_do_not_move_the_average(cadence: Cadence) -> None:
    span = typing_duration(plan(SAMPLE, cadence, seed=11))
    assert len(SAMPLE) / 5.0 / (span / 60.0) == pytest.approx(70, rel=1e-6)


def test_the_full_stream_outlasts_typing_by_one_hold(cadence=None) -> None:
    events = plan(SAMPLE, Cadence(wpm=70), seed=1)
    extra = duration(events) - typing_duration(events)
    assert 0.0 < extra < 0.3


# ---------------------------------------------------------------------------
# Hold time — the half of keystroke timing v1 never modelled
# ---------------------------------------------------------------------------
@pytest.mark.parametrize(
    "wpm,low,high",
    [(30, 0.100, 0.125), (70, 0.085, 0.105), (120, 0.060, 0.082), (200, 0.040, 0.060)],
)
def test_hold_time_matches_human_measurements(wpm: int, low: float, high: float) -> None:
    """Reference: ~110ms at 30wpm falling to ~50ms at 200, per the Aalto corpus.

    A flat 15ms hold, which is what v1 and the first cut of v2 emitted, is the
    loudest signal a keystroke listener can receive.
    """
    stats = stroke_stats(plan(SAMPLE * 3, Cadence(wpm=wpm), seed=2))
    assert low <= statistics.mean(stats.dwell) <= high


def test_hold_time_is_right_skewed_not_uniform() -> None:
    """Uniform holds have no tail; real ones do, and the shape is measurable."""
    stats = stroke_stats(plan(SAMPLE * 3, Cadence(wpm=75), seed=4))
    mean = statistics.mean(stats.dwell)
    median = statistics.median(stats.dwell)
    assert mean > median                                  # right-skewed
    assert statistics.stdev(stats.dwell) / mean > 0.15    # genuinely spread


def test_hold_time_never_collapses_to_nothing() -> None:
    for wpm in (600, 1200, 2000):
        stats = stroke_stats(plan(SAMPLE, Cadence(wpm=wpm), seed=5))
        assert min(stats.dwell) > 0.0


def test_explicit_dwell_overrides_the_speed_derived_default() -> None:
    stats = stroke_stats(plan(SAMPLE, Cadence(wpm=70, dwell=0.2), seed=6))
    assert statistics.mean(stats.dwell) == pytest.approx(0.2, rel=0.15)


# ---------------------------------------------------------------------------
# Rollover — humans press the next key before releasing the last
# ---------------------------------------------------------------------------
def test_rollover_happens_and_rises_with_speed() -> None:
    slow = stroke_stats(plan(SAMPLE * 2, Cadence(wpm=45), seed=8)).rollover_rate
    fast = stroke_stats(plan(SAMPLE * 2, Cadence(wpm=130), seed=8)).rollover_rate
    assert slow < fast
    assert 0.0 <= slow < 0.20
    assert fast > 0.20


def test_rollover_zero_releases_every_key_before_the_next():
    stats = stroke_stats(plan(SAMPLE * 2, Cadence(wpm=130, rollover=0.0), seed=9))
    assert stats.rollover_rate == 0.0
    assert min(stats.flight) > 0.0


def test_one_finger_is_never_on_two_keys_at_once() -> None:
    """'ed' and 'ol' reuse a finger. Overlapping them is physically impossible."""
    for text in ("ed" * 40, "ol" * 40, "aq" * 40):
        stats = stroke_stats(plan(text, Cadence(wpm=200), seed=10))
        assert stats.rollover_rate == 0.0, text


def test_a_repeated_letter_is_always_released_before_being_struck_again() -> None:
    """A second press of a held key would strand it down permanently."""
    for wpm in (70, 200, 600):
        events = plan("aaabbbccc mmm lll", Cadence(wpm=wpm), seed=12)
        held: set[str] = set()
        for event in events:
            if event.action is Action.DOWN:
                assert event.value not in held, f"{event.value!r} pressed while held"
                held.add(event.value)
            else:
                held.discard(event.value)


def test_every_press_is_matched_by_exactly_one_release() -> None:
    events = plan(SAMPLE, Cadence(wpm=90, typo_rate=0.1), seed=13)
    downs = sum(1 for e in events if e.action is Action.DOWN)
    ups = sum(1 for e in events if e.action is Action.UP)
    assert downs == ups > 0
    assert events[0].action is Action.DOWN
    assert events[-1].action is Action.UP


def test_no_more_than_two_keys_are_ever_held_at_once() -> None:
    """Overlapping a pair is rollover; holding three is a stuck key."""
    events = plan(SAMPLE * 2, Cadence(wpm=250), seed=14)
    held = 0
    peak = 0
    for event in events:
        held += 1 if event.action is Action.DOWN else -1
        peak = max(peak, held)
    assert peak <= 2


# ---------------------------------------------------------------------------
# Interval shape
# ---------------------------------------------------------------------------
def test_all_knobs_zero_is_a_metronome() -> None:
    flat = Cadence(wpm=70, variance=0, rhythm=0, hesitation=0,
                   sentence_pause=0, drift=0, dwell_variance=0)
    intervals = stroke_stats(plan(SAMPLE, flat, seed=1)).interval
    assert max(intervals) - min(intervals) < 1e-9
    assert intervals[0] == pytest.approx(60.0 / 70.0 / 5.0)


def test_default_cadence_is_not_a_metronome() -> None:
    intervals = stroke_stats(plan(SAMPLE, Cadence(wpm=70), seed=1)).interval
    assert statistics.stdev(intervals) / statistics.mean(intervals) > 0.25


def test_intervals_are_never_negative() -> None:
    for cadence in (Cadence(wpm=600, variance=1.5), Cadence(wpm=5)):
        events = plan(SAMPLE, cadence, seed=3)
        assert all(e.delay >= 0 for e in events)
        assert all(i > 0 for i in stroke_stats(events).interval)


def test_same_finger_digraph_is_slower_than_alternating_hands() -> None:
    """'ed' reuses one finger; 'th' alternates hands. Rhythm should show it."""
    flat = dict(wpm=70, variance=0, hesitation=0, sentence_pause=0, drift=0)
    same = stroke_stats(plan("ed", Cadence(rhythm=1.0, **flat), seed=1)).interval[0]
    alt = stroke_stats(plan("th", Cadence(rhythm=1.0, **flat), seed=1)).interval[0]
    assert same > alt


def test_schedule_is_reproducible_under_a_seed() -> None:
    a = plan(SAMPLE, Cadence(wpm=70, typo_rate=0.05), seed=42)
    b = plan(SAMPLE, Cadence(wpm=70, typo_rate=0.05), seed=42)
    assert a == b


# ---------------------------------------------------------------------------
# Content
# ---------------------------------------------------------------------------
def test_newline_and_tab_become_named_keys() -> None:
    values = {e.value for e in plan("a\n\tb", Cadence(), seed=1)}
    assert "enter" in values and "tab" in values


def test_typos_are_always_followed_by_a_correction() -> None:
    events = presses(plan("the quick brown fox", Cadence(wpm=70, typo_rate=0.2), seed=5))
    corrections = sum(1 for e in events if e.value == "backspace")
    strays = sum(1 for e in events if e.index == -1 and e.value != "backspace")
    assert corrections == strays > 0


def test_progress_indices_never_go_backwards() -> None:
    seen = -1
    for event in plan(SAMPLE, Cadence(wpm=70, typo_rate=0.2), seed=9):
        if event.index >= 0:
            assert event.action is Action.DOWN
            assert event.index > seen
            seen = event.index
    assert seen == len(SAMPLE) - 1


def test_empty_text_produces_no_events() -> None:
    assert plan("", Cadence(), seed=1) == []
    assert duration([]) == 0.0
    assert typing_duration([]) == 0.0


@pytest.mark.parametrize(
    "bad",
    [
        Cadence(wpm=0), Cadence(wpm=5000), Cadence(variance=-1),
        Cadence(typo_rate=0.9), Cadence(sentence_pause=2), Cadence(max_pause=-1),
        Cadence(dwell=-0.1), Cadence(dwell=5.0), Cadence(rollover=1.5),
        Cadence(dwell_variance=-1),
    ],
)
def test_invalid_cadence_is_rejected(bad: Cadence) -> None:
    with pytest.raises(ValueError):
        plan("x", bad)


@pytest.mark.parametrize(
    "seconds,expected",
    [(0.4, "<1s"), (7.2, "7s"), (65, "1m 05s"), (3725, "1h 02m")],
)
def test_duration_formatting(seconds: float, expected: str) -> None:
    assert format_duration(seconds) == expected
