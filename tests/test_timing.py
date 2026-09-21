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
def _delivered(text: str, cadence: Cadence, seed: int) -> float:
    span = typing_duration(plan(text, cadence, seed=seed))
    return len(text) / 5.0 / (span / 60.0)


@pytest.mark.parametrize("wpm", [15, 40, 70, 120, 250, 600])
def test_wpm_is_delivered_to_within_a_few_percent(wpm: int) -> None:
    """Whatever shapes the rhythm, the rate must be the one that was asked for.

    Not to the microsecond, though: see the run-to-run test below. Each run
    draws its own pace for the day, so the dial is honest within a few percent
    rather than exact, which is the difference between a setting and a
    fingerprint.
    """
    for seed in range(12):
        assert _delivered(SAMPLE, Cadence(wpm=wpm), seed) == pytest.approx(
            wpm, rel=0.12
        )


@pytest.mark.parametrize("wpm", [40, 70, 120])
def test_the_requested_rate_is_what_runs_average_to(wpm: int) -> None:
    """The per-run wobble is symmetric: it must not bias the dial."""
    rates = [_delivered(SAMPLE, Cadence(wpm=wpm), seed) for seed in range(60)]
    assert statistics.mean(rates) == pytest.approx(wpm, rel=0.02)


def test_two_runs_of_one_text_do_not_take_the_same_time() -> None:
    """The flatness that no amount of internal jitter hid.

    Normalising every schedule to the requested rate made total duration a
    function of (length, wpm) and nothing else -- 514.285714 seconds on every
    seed, identical to the microsecond. Within a run the rhythm was different
    every time; across runs it could not be. A passage typed twice in exactly
    the same number of microseconds was not typed.
    """
    spans = {
        round(typing_duration(plan(SAMPLE, Cadence(wpm=70), seed=seed)), 6)
        for seed in range(20)
    }
    assert len(spans) == 20, "runs are sharing a duration"
    spread = (max(spans) - min(spans)) / statistics.mean(spans)
    assert 0.02 < spread < 0.40, spread


def test_a_metronome_stays_exact() -> None:
    """Asking for no wander at all means the rate is a setting again."""
    flat = Cadence(wpm=70, variance=0, rhythm=0, hesitation=0,
                   sentence_pause=0, drift=0, dwell_variance=0)
    for seed in (1, 2, 3):
        assert _delivered(SAMPLE, flat, seed) == pytest.approx(70, rel=1e-6)


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
    """These change the character of the typing, never its speed."""
    rates = [_delivered(SAMPLE, cadence, seed) for seed in range(40)]
    assert statistics.mean(rates) == pytest.approx(70, rel=0.02)


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


# ---------------------------------------------------------------------------
# Modifiers are keystrokes
# ---------------------------------------------------------------------------
# Left to the backend, Shift went down in the same instant as the letter it
# modified -- measured on the wire at 0.013-0.04 ms of lead, where a hand gives
# 50-150. A page listening for keydown sees the two events share a timestamp,
# which is a thing no keyboard produces.
_SHIFT = "KEY_LEFTSHIFT"


def _us_modifiers(token: str) -> tuple[str, ...]:
    """A stand-in for a keyboard layout: shift for capitals and for !?:."""
    if len(token) != 1:
        return ()
    return (_SHIFT,) if token.isupper() or token in '!?:"' else ()


def _absolute(events):
    """(time, action, value) for each event, on one clock."""
    clock = 0.0
    out = []
    for event in events:
        clock += event.delay
        out.append((clock, event.action, event.value))
    return out


def _modifier_spans(events) -> list[tuple[float, float]]:
    spans, down = [], None
    for moment, action, value in _absolute(events):
        if value != _SHIFT:
            continue
        if action is Action.DOWN:
            down = moment
        elif down is not None:
            spans.append((down, moment))
            down = None
    return spans


def test_without_a_layout_no_modifier_is_scheduled() -> None:
    """The old behaviour stays available: the backend presses them itself."""
    events = plan("ABC", Cadence(wpm=70), seed=1)
    assert not [e for e in events if e.value == _SHIFT]


def test_shift_leads_the_key_it_modifies_by_a_human_margin() -> None:
    events = plan("Aa Bb Cc Dd Ee Ff Gg Hh", Cadence(wpm=70), seed=3,
                  modifiers_of=_us_modifiers)
    timeline = _absolute(events)
    leads = []
    for start, _end in _modifier_spans(events):
        first = next(
            moment for moment, action, value in timeline
            if action is Action.DOWN and value != _SHIFT and moment > start
        )
        leads.append(first - start)
    assert leads, "no modifier was scheduled"
    assert all(0.015 < lead < 0.40 for lead in leads), leads
    assert 0.04 < statistics.mean(leads) < 0.15, statistics.mean(leads)


def test_shift_is_still_held_after_the_key_it_modified_is_released() -> None:
    """A hand does not lift both fingers together."""
    events = plan("Aa Bb Cc Dd", Cadence(wpm=70), seed=5,
                  modifiers_of=_us_modifiers)
    timeline = _absolute(events)
    lags = []
    for _start, end in _modifier_spans(events):
        released = [
            moment for moment, action, value in timeline
            if action is Action.UP and value != _SHIFT and moment < end
        ]
        if released:
            lags.append(end - max(released))
    assert lags and any(lag > 0.005 for lag in lags), lags


def test_a_run_of_capitals_holds_one_continuous_shift() -> None:
    spans = _modifier_spans(
        plan("ABCDEF gh", Cadence(wpm=70), seed=2, modifiers_of=_us_modifiers)
    )
    assert len(spans) == 1, spans


def test_shift_is_never_held_over_a_character_that_did_not_ask_for_it() -> None:
    """The correctness half. A modifier that outlives its run changes the text.

    Shift may lift while the capital is still physically down -- the character
    was decided when the key went down -- but it must be gone before the next
    unshifted key is pressed, or "Ab" arrives as "AB".
    """
    text = 'Hello World! "Quoted" Xy: Zz? ABCdef Ghi'
    events = plan(text, Cadence(wpm=70), seed=9, modifiers_of=_us_modifiers)
    spans = _modifier_spans(events)
    for moment, action, value in _absolute(events):
        if action is not Action.DOWN or value == _SHIFT:
            continue
        held = any(start <= moment <= end for start, end in spans)
        assert held == bool(_us_modifiers(value)), (value, moment, held)


def test_shift_never_precedes_the_start_of_the_schedule() -> None:
    for text in ("A", "Ab", "!"):
        events = plan(text, Cadence(wpm=70), seed=1, modifiers_of=_us_modifiers)
        assert all(event.delay >= 0.0 for event in events), text


# ---------------------------------------------------------------------------
# Trend across a document
# ---------------------------------------------------------------------------
def test_typing_starts_slow_and_ends_slower_than_its_middle() -> None:
    """A flat pace across three thousand characters is not a person.

    The Ornstein-Uhlenbeck walk wanders but reverts, so before this the first
    and last thirds of a long document came out 173.1 ms and 175.7 ms apart --
    which is to say identical.
    """
    text = ("The quick brown fox jumps over the lazy dog. "
            "Pack my box with five dozen liquor jugs. ") * 45
    intervals = stroke_stats(plan(text, Cadence(wpm=70), seed=4)).interval
    third = len(intervals) // 3
    start = statistics.mean(intervals[:third // 2])
    middle = statistics.mean(intervals[third:2 * third])
    end = statistics.mean(intervals[-third:])
    assert start > middle * 1.02, (start, middle)
    assert end > middle * 1.01, (end, middle)


def test_the_trend_is_shape_and_not_speed() -> None:
    """Warm-up and fatigue must not cost the document its requested rate."""
    text = "The quick brown fox jumps over the lazy dog. " * 45
    rates = [_delivered(text, Cadence(wpm=70), seed) for seed in range(30)]
    assert statistics.mean(rates) == pytest.approx(70, rel=0.02)


def test_no_drift_means_no_trend() -> None:
    text = "the quick brown fox jumps over the lazy dog " * 40
    flat = Cadence(wpm=70, variance=0, rhythm=0, hesitation=0,
                   sentence_pause=0, drift=0, dwell_variance=0)
    intervals = stroke_stats(plan(text, flat, seed=1)).interval
    assert max(intervals) - min(intervals) < 1e-9
