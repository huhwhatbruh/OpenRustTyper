"""The cadence model: text in, a schedule of keystrokes out.

Pure functions and plain data. Nothing here touches the keyboard, the clock or
the screen, which is what makes the timing testable and the ETA honest -- the
progress bar sums the very same schedule the engine types from.

The model is built on four observations about people at keyboards:

* Inter-key intervals are lognormal, not normal. The distribution has a hard
  floor (you cannot press two keys in negative time), a dense body and a long
  right tail. Sampling from a Gaussian and clamping it, as v1 did, produces a
  pile-up at the clamp that no hand ever makes.
* Consecutive keys struck by the same finger are much slower than keys that
  alternate hands, because the finger has to travel. This digraph effect is
  the single largest source of rhythm in real typing.
* Speed wanders slowly and reverts, rather than jumping. An Ornstein-Uhlenbeck
  walk gives a drifting baseline; independent draws give a twitch.
* Pauses cluster at syntactic boundaries -- after sentences, around
  paragraphs -- not uniformly across words.
"""

from __future__ import annotations

import math
import random
from collections.abc import Callable
from dataclasses import dataclass
from enum import IntEnum

#: Answers "which modifiers does this character need", in backend key names.
#: Only the active keyboard layout knows, and this module does not import one.
ModifiersOf = Callable[[str], tuple[str, ...]]

# ---------------------------------------------------------------------------
# Physical keyboard geometry
# ---------------------------------------------------------------------------
# Rows of QWERTY as struck, with the finger that strikes each key. Finger ids
# run 0-3 for the left hand (pinky->index) and 4-7 for the right (index->pinky).
# Geometry is only ever used to *shape rhythm*, so an approximate US layout is
# good enough even when the active layout differs; a wrong guess costs realism,
# never correctness.
_ROWS: tuple[tuple[str, tuple[int, ...]], ...] = (
    ("`1234567890-=",  (0, 0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 7, 7)),
    ("qwertyuiop[]\\", (0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 7, 7, 7)),
    ("asdfghjkl;'",    (0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 7)),
    ("zxcvbnm,./",     (0, 1, 2, 3, 3, 4, 4, 5, 6, 7)),
)

# Characters reached by holding Shift, aligned with the unshifted rows above.
_SHIFTED = (
    "~!@#$%^&*()_+",
    "QWERTYUIOP{}|",
    'ASDFGHJKL:"',
    "ZXCVBNM<>?",
)


def _build_geometry() -> dict[str, tuple[int, int, int, bool]]:
    """Map each character to ``(finger, row, column, needs_shift)``."""
    geo: dict[str, tuple[int, int, int, bool]] = {}
    for row_idx, ((keys, fingers), shifted) in enumerate(zip(_ROWS, _SHIFTED, strict=True)):
        for col, (ch, finger) in enumerate(zip(keys, fingers, strict=True)):
            geo[ch] = (finger, row_idx, col, False)
            if col < len(shifted):
                geo[shifted[col]] = (finger, row_idx, col, True)
    geo[" "] = (8, 4, 5, False)  # thumb, its own row
    return geo


_GEOMETRY = _build_geometry()

# Neighbours on the physical board, used to make typos land where a hand would
# actually slip rather than on a random letter.
def _build_neighbours() -> dict[str, str]:
    near: dict[str, list[str]] = {}
    coords = {
        ch: (row, col)
        for ch, (_f, row, col, shift) in _GEOMETRY.items()
        if not shift and ch != " "
    }
    for ch, (row, col) in coords.items():
        hits = [
            other
            for other, (orow, ocol) in coords.items()
            if other != ch and abs(orow - row) <= 1 and abs(ocol - col) <= 1
        ]
        if hits:
            near[ch] = "".join(sorted(hits))
    return {k: v for k, v in near.items()}


_NEIGHBOURS = _build_neighbours()

# High-frequency words are executed as single motor chunks rather than
# letter by letter, so they come out faster than their length predicts.
_CHUNKED = frozenset(
    """a i be to of in it is as at he we so on or an my by do go up no if me us
    the and for are but not you all can had her was one our out has his how its
    may new now old see way who did get let say she too use own say she try ask
    that this with have from they will been more when what your than them then
    into some time very just like know take come make able only over such most
    also back even want give year work well good need find last long much many
    where which while there their these those been being does done each else""".split()
)

_SENTENCE_END = frozenset(".!?")
_CLAUSE_END = frozenset(",;:")

# ---------------------------------------------------------------------------
# Trend across a document, and across runs
# ---------------------------------------------------------------------------
# The Ornstein-Uhlenbeck walk reverts to a fixed mean, which makes the pace
# wander but never *go* anywhere: over three thousand characters the first
# third and the last third came out 173.1 ms and 175.7 ms apart, which is to
# say identical. A person does not type a long document at a flat rate. They
# start below their own pace and find it over the first paragraph or two, and
# they give some of it back by the end. Both are slow enough to be invisible
# in any one interval and obvious in the trend of several thousand.

#: How much slower than the settled pace the very first keystrokes are.
_WARMUP_EXCESS = 0.10
#: Characters over which the warm-up decays; roughly a paragraph.
_WARMUP_SCALE = 180.0
#: How much slower the pace has become by the time fatigue is fully felt.
_FATIGUE_EXCESS = 0.06
#: Characters before fatigue has arrived in full -- a few pages.
_FATIGUE_ONSET = 1500.0

# A second flatness, less obvious and more damning: the total. Because the
# schedule is normalised to the requested rate, one document at one setting
# took 514.285714 seconds on every seed tried -- the same figure to the
# microsecond, run after run. The rhythm inside differed every time; the sum
# could not. Nobody types a passage twice in the same length of time, and two
# runs that agree to seven figures agree because neither was typed.

#: Spread of the per-run pace, as the sigma of a lognormal on total duration.
#: Small enough that the words-per-minute dial still means what it says, large
#: enough that no two runs land on the same number.
_RUN_TO_RUN = 0.035


class Action(IntEnum):
    """What a scheduled event does when it fires.

    Presses and releases are scheduled independently, which is the whole point:
    a key's release may fall after the *next* key's press, and on a real
    keyboard it frequently does.
    """

    UP = 0    # release a key — sorts first when a release and a press coincide
    DOWN = 1  # press a key


@dataclass(frozen=True, slots=True)
class Event:
    """One scheduled key transition.

    ``delay`` is the wait before the event fires. ``index`` is the position in
    the source text that this press completes; it is -1 for releases and for
    the keystrokes belonging to a typo and its correction, so progress never
    runs backwards.
    """

    delay: float
    action: Action
    value: str
    index: int


@dataclass(slots=True)
class Cadence:
    """Tunable shape of the typing rhythm.

    ``wpm`` is the only setting most people need. Everything else moves the
    character of the rhythm without changing its average speed, because the
    finished schedule is normalized to the rate ``wpm`` asks for.
    """

    wpm: float = 70.0
    #: Spread of the lognormal interval between presses. 0 is a metronome.
    variance: float = 0.30
    #: How strongly finger travel stretches an interval. 0 disables geometry.
    rhythm: float = 1.0
    #: Fraction of keystrokes preceded by a visible hitch.
    hesitation: float = 0.03
    #: Probability of pausing at a sentence boundary, 0-1.
    sentence_pause: float = 0.55
    #: Longest boundary pause, in seconds.
    max_pause: float = 1.8
    #: Probability per character of striking a neighbouring key by mistake.
    #: The engine always corrects it; 0 types verbatim.
    typo_rate: float = 0.0
    #: Strength of the slow speed wander. 0 holds a constant baseline.
    drift: float = 1.0
    #: Mean key-hold time in seconds. 0 derives it from ``wpm``.
    dwell: float = 0.0
    #: Spread of the lognormal hold time.
    dwell_variance: float = 0.26
    #: How much a key may still be held when the next one is pressed, 0-1.
    #: 0 releases every key before the next press; 1 lets holds overlap as
    #: freely as the hands allow.
    rollover: float = 1.0

    def mean_interval(self) -> float:
        """Seconds per character implied by ``wpm`` (a word is five keys)."""
        return 60.0 / max(self.wpm, 1.0) / 5.0

    def mean_dwell(self) -> float:
        """Seconds a key is held, on average."""
        if self.dwell > 0.0:
            return self.dwell
        ms = _DWELL_INTERCEPT_MS - _DWELL_SLOPE_MS * self.wpm
        return max(_DWELL_FLOOR, min(_DWELL_CEILING, ms / 1000.0))

    def validate(self) -> None:
        if not 1.0 <= self.wpm <= 2000.0:
            raise ValueError("wpm must be between 1 and 2000")
        if not 0.0 <= self.variance <= 2.0:
            raise ValueError("variance must be between 0 and 2")
        if not 0.0 <= self.typo_rate <= 0.25:
            raise ValueError("typo_rate must be between 0 and 0.25")
        if not 0.0 <= self.sentence_pause <= 1.0:
            raise ValueError("sentence_pause must be between 0 and 1")
        if self.max_pause < 0.0:
            raise ValueError("max_pause cannot be negative")
        if not 0.0 <= self.dwell <= 1.0:
            raise ValueError("dwell must be between 0 and 1 second")
        if not 0.0 <= self.dwell_variance <= 2.0:
            raise ValueError("dwell_variance must be between 0 and 2")
        if not 0.0 <= self.rollover <= 1.0:
            raise ValueError("rollover must be between 0 and 1")


# ---------------------------------------------------------------------------
# How long a key is held
# ---------------------------------------------------------------------------
# Hold time is the half of keystroke timing that auto-typers forget. Every
# keydown a page receives is paired with a keyup, and the interval between them
# is as characteristic as the interval between keys -- more so, because it
# barely varies with what is being typed and so has a tight, recognisable
# distribution. A tool that holds every key for a flat 15 ms is announcing
# itself on a channel it never looked at.
#
# The mean falls as typing speed rises: quick typists do not merely travel
# between keys sooner, they lift each finger sooner. These coefficients trace
# the trend reported for the Aalto 136M-keystroke corpus -- roughly 110 ms at
# 30 wpm, 95 ms at 70, 70 ms at 120 -- and the floor and ceiling keep
# extrapolation away from physically silly values.
_DWELL_INTERCEPT_MS = 125.0
_DWELL_SLOPE_MS = 0.45
_DWELL_FLOOR = 0.045
_DWELL_CEILING = 0.135

#: Hold time by finger. The thumb rests on the space bar and lingers; the
#: pinkies are slow to lift; the index fingers are quickest.
_FINGER_DWELL = {
    0: 1.10, 1: 1.05, 2: 1.00, 3: 0.96,
    4: 0.96, 5: 1.00, 6: 1.05, 7: 1.10,
    8: 1.14,  # thumb / space
}

#: The shortest hold that still registers reliably as a keypress.
_MIN_DWELL = 0.004

#: The longest a key may be held. Two reasons, and they agree on the number.
#: Prose typing simply does not hold a key a fifth of a second -- the tail of
#: the lognormal reaches there, but a hand does not. And the virtual keyboard
#: declares autorepeat, as a keyboard must; past the kernel's 250 ms repeat
#: delay the key would start duplicating itself, turning a long hold into a
#: character the text never contained. Clamping here keeps both honest.
MAX_DWELL = 0.200


def _geometry_of(value: str) -> tuple[int, int, int, bool] | None:
    if len(value) != 1:
        return None  # a named key: enter, tab, backspace
    return _GEOMETRY.get(value.lower() if value.isalpha() else value)


def _dwell_factor(value: str) -> float:
    geo = _geometry_of(value)
    if geo is None:
        return 1.0
    finger, _row, _col, needs_shift = geo
    factor = _FINGER_DWELL.get(finger, 1.0)
    if needs_shift:
        factor *= 1.06  # a finger reaching with a modifier held lifts later
    return factor


def _physical_key(value: str) -> tuple[object, ...]:
    """Which key cap a token uses. 'a' and 'A' are the same one."""
    geo = _geometry_of(value)
    if geo is None:
        return ("named", value)
    _finger, row, col, _shift = geo
    return ("cap", row, col)


def _finger_of(value: str) -> int | None:
    geo = _geometry_of(value)
    return geo[0] if geo else None


@dataclass(frozen=True, slots=True)
class _Press:
    """A key to strike, and how long to wait before striking it."""

    interval: float
    value: str
    index: int


def _digraph_factor(prev: str | None, ch: str, strength: float) -> float:
    """How much finger travel stretches the interval before ``ch``."""
    if strength <= 0.0 or prev is None:
        return 1.0
    a = _geometry_of(prev)
    b = _geometry_of(ch)
    if a is None or b is None:
        return 1.0

    fa, ra, ca, _ = a
    fb, rb, cb, _ = b

    if fa == fb:
        # Same finger: either a fast repeat of one key, or a real reach.
        factor = 1.12 if (ra, ca) == (rb, cb) else 1.70 + 0.14 * abs(ra - rb)
    elif (fa < 4) != (fb < 4):
        factor = 0.86  # hands alternate, the fastest digraph there is
    else:
        factor = 1.0 + 0.10 * abs(ra - rb) + 0.02 * abs(ca - cb)

    return 1.0 + (factor - 1.0) * strength


def _boundary_pause(text: str, i: int, cad: Cadence, rng: random.Random) -> float:
    """Pause earned by the syntax just typed, before position ``i``."""
    if i <= 0 or cad.max_pause <= 0.0:
        return 0.0
    prev = text[i - 1]

    # Paragraph breaks are the longest pause in ordinary writing.
    if prev == "\n":
        if rng.random() >= cad.sentence_pause:
            return 0.0
        if i >= 2 and text[i - 2] == "\n":
            return rng.uniform(0.35, 1.0) * cad.max_pause
        return rng.uniform(0.10, 0.35) * cad.max_pause

    if text[i] != " " and prev == " " and i >= 2:
        anchor = text[i - 2]
        if anchor in _SENTENCE_END:
            if rng.random() < cad.sentence_pause:
                return rng.uniform(0.25, 0.85) * cad.max_pause
        elif anchor in _CLAUSE_END and rng.random() < cad.sentence_pause * 0.6:
            return rng.uniform(0.08, 0.30) * cad.max_pause
    return 0.0


def _word_at(text: str, i: int) -> tuple[str, int]:
    """The whitespace-delimited word starting at ``i``, and where it ends."""
    j = i
    n = len(text)
    while j < n and not text[j].isspace():
        j += 1
    return text[i:j], j


def _plan_presses(
    text: str, cadence: Cadence, rng: random.Random
) -> list[_Press]:
    """Decide which keys to strike, and how long to wait before each."""
    mean = cadence.mean_interval()
    presses: list[_Press] = []

    # Log-space speed offset, wandering by Ornstein-Uhlenbeck so the baseline
    # reverts to the requested WPM instead of walking away from it.
    z = 0.0
    theta, vol = 0.02, 0.035 * cadence.drift

    # Sigma of the lognormal is chosen so the *mean* stays at `mean`: for
    # lognormal, E[X] = exp(mu + sigma^2/2), hence mu = -sigma^2/2.
    sigma = cadence.variance
    mu_correction = -0.5 * sigma * sigma

    i, n = 0, len(text)
    chunk_factor = 1.0
    chunk_until = -1

    # Both trends are scaled by `drift`, which is the dial for "how much does
    # the pace wander"; at zero the model is a metronome and stays one. And
    # both are shape, not speed: _calibrate normalises the mean away again, so
    # the document still takes as long as the WPM says.
    warmup = _WARMUP_EXCESS * cadence.drift
    fatigue = _FATIGUE_EXCESS * cadence.drift

    while i < n:
        ch = text[i]

        if i >= chunk_until and not ch.isspace():
            word, end = _word_at(text, i)
            stripped = word.strip(".,;:!?\"'()[]{}-").lower()
            chunked = stripped in _CHUNKED and len(word) <= 6
            chunk_factor = 1.0 - 0.18 * cadence.rhythm if chunked else 1.0
            chunk_until = end

        z += -theta * z + vol * rng.gauss(0.0, 1.0)
        prev = text[i - 1] if i > 0 else None

        trend = 1.0 + warmup * math.exp(-i / _WARMUP_SCALE) + fatigue * min(
            1.0, i / _FATIGUE_ONSET
        )

        interval = (
            mean
            * math.exp(z + rng.gauss(mu_correction, sigma))
            * _digraph_factor(prev, ch, cadence.rhythm)
            * chunk_factor
            * trend
        )

        if rng.random() < cadence.hesitation:
            interval += mean * rng.uniform(2.0, 6.0)

        interval += _boundary_pause(text, i, cadence, rng)
        interval = max(interval, 0.0005)

        # A slip: strike a neighbouring key, notice, backspace, carry on.
        if cadence.typo_rate > 0.0 and ch.isalpha() and rng.random() < cadence.typo_rate:
            wrong_pool = _NEIGHBOURS.get(ch.lower())
            if wrong_pool:
                wrong = rng.choice(wrong_pool)
                if ch.isupper():
                    wrong = wrong.upper()
                presses.append(_Press(interval, wrong, -1))
                # The hand runs on for a beat before the error registers.
                presses.append(
                    _Press(mean * rng.uniform(1.5, 5.0), "backspace", -1)
                )
                interval = mean * rng.uniform(0.8, 1.6)

        if ch == "\n":
            presses.append(_Press(interval, "enter", i))
        elif ch == "\t":
            presses.append(_Press(interval, "tab", i))
        else:
            presses.append(_Press(interval, ch, i))
        i += 1

    return presses


def _calibrate(
    presses: list[_Press], text: str, cadence: Cadence, rng: random.Random
) -> list[_Press]:
    """Scale the schedule so the requested words-per-minute is what happens.

    Every multiplier in the model shapes the rhythm; none of them should move
    the average, or the WPM dial would be a suggestion rather than a setting.

    Not *quite* none, though. Normalising to the exact requested rate made the
    total duration a function of the text and the dial alone, identical to the
    microsecond on every run -- a fingerprint across runs rather than within
    one. So the day's pace is drawn once per schedule and the rate is delivered
    to within a few percent rather than to seven figures.
    """
    total = sum(p.interval for p in presses)
    if total <= 0.0:
        return presses
    # The target spans press to press: producing the text is what is being
    # timed, not the release of its final key.
    target = len(text) / 5.0 * 60.0 / cadence.wpm
    if cadence.drift > 0.0:
        target *= math.exp(rng.gauss(0.0, _RUN_TO_RUN * cadence.drift))
    scale = target / total
    return [
        _Press(max(p.interval * scale, 0.0005), p.value, p.index) for p in presses
    ]


def _release_times(
    presses: list[_Press], press_at: list[float], cadence: Cadence,
    rng: random.Random,
) -> list[float]:
    """When each key is let go — the part that makes rollover possible."""
    mean = cadence.mean_dwell()
    sigma = cadence.dwell_variance
    mu_correction = -0.5 * sigma * sigma
    n = len(presses)
    release_at = [0.0] * n

    for i, press in enumerate(presses):
        hold = mean * _dwell_factor(press.value) * math.exp(
            rng.gauss(mu_correction, sigma)
        )
        release = press_at[i] + min(max(hold, _MIN_DWELL), MAX_DWELL)

        if i + 1 < n:
            nxt = presses[i + 1]
            span = press_at[i + 1] - press_at[i]
            gap = max(min(0.010, span * 0.3), 0.0005)

            # Two constraints are physical, not stylistic. One finger cannot be
            # on two keys, and one key cannot be pressed while already down --
            # and a second press of a held key would strand it down for good.
            same_cap = _physical_key(press.value) == _physical_key(nxt.value)
            finger = _finger_of(press.value)
            same_finger = finger is not None and finger == _finger_of(nxt.value)

            if same_cap or same_finger:
                release = min(release, press_at[i + 1] - gap)
            else:
                overlap = release - press_at[i + 1]
                if overlap > 0.0:
                    release = (
                        press_at[i + 1]
                        + overlap * cadence.rollover
                        - (1.0 - cadence.rollover) * gap
                    )
                # Holding a key across two further presses is not rollover,
                # it is a stuck key.
                if i + 2 < n:
                    release = min(release, press_at[i + 2] - gap)

            # Whatever the clamps above decided, the release must still land
            # after its own press and before the same key is struck again.
            release = max(release, press_at[i] + _MIN_DWELL)
            if same_cap:
                release = min(release, press_at[i + 1] - 0.0005)
        else:
            release = max(release, press_at[i] + _MIN_DWELL)

        release_at[i] = release
    return release_at



# ---------------------------------------------------------------------------
# Modifiers are keystrokes too
# ---------------------------------------------------------------------------
# A capital letter is two key presses, and the first one is the tell. Left to
# the backend, Shift went down in the same breath as the letter it modified:
# measured on the wire, a lead of 0.013-0.04 ms. A hand cannot do that. Shift
# is a separate finger starting a separate movement, and it arrives well ahead
# -- 50 to 150 ms is the ordinary range -- then lingers past the letter's own
# release. A page listening for keydown sees the two events share a timestamp,
# which is a signature nothing else produces.
#
# So modifiers are scheduled here, alongside the characters, instead of being
# an implementation detail of the device. Which modifier a character needs
# depends on the keyboard layout, which this module deliberately knows nothing
# about, so the caller passes a function that answers it.

#: Mean seconds a modifier goes down before the key it modifies.
_MODIFIER_LEAD = 0.075
#: Spread of that lead, as the sigma of a lognormal.
_MODIFIER_LEAD_SIGMA = 0.42
#: Mean seconds a modifier stays down after the last key that needed it.
_MODIFIER_LAG = 0.048
_MODIFIER_LAG_SIGMA = 0.40
#: The smallest ordering gap worth preserving between two transitions.
_EPSILON = 0.002


def _modifier_runs(
    presses: list[_Press], modifiers_of: ModifiersOf
) -> list[tuple[str, int, int]]:
    """Maximal runs of consecutive presses that all want one modifier.

    A run of capitals is one continuous Shift, not a press per letter, so the
    grouping has to happen before any timing is decided.
    """
    wanted = [frozenset(modifiers_of(press.value)) for press in presses]
    runs: list[tuple[str, int, int]] = []
    for name in sorted({m for group in wanted for m in group}):
        start: int | None = None
        for i, group in enumerate(wanted):
            if name in group:
                if start is None:
                    start = i
            elif start is not None:
                runs.append((name, start, i - 1))
                start = None
        if start is not None:
            runs.append((name, start, len(presses) - 1))
    return runs


def _schedule_modifiers(
    presses: list[_Press],
    press_at: list[float],
    release_at: list[float],
    modifiers_of: ModifiersOf,
    rng: random.Random,
) -> list[tuple[float, Action, str, int]]:
    """Place each modifier's own press and release on the timeline.

    Every clamp here is correctness rather than realism. A modifier that lands
    after the key it modifies produces the unmodified character; one that
    outlives its run modifies a character that did not ask for it, which is
    the bug that turned "Second" into "SEcond" -- only now it would be the
    schedule making it rather than the backend.
    """
    events: list[tuple[float, Action, str, int]] = []
    for name, first, last in _modifier_runs(presses, modifiers_of):
        lead = _MODIFIER_LEAD * math.exp(rng.gauss(0.0, _MODIFIER_LEAD_SIGMA))
        down = press_at[first] - lead
        # Reaching for Shift begins after the previous keystroke has begun,
        # not before it -- but it may well overlap that key's release.
        if first > 0:
            down = max(down, press_at[first - 1] + _EPSILON)
        down = min(down, press_at[first] - _EPSILON / 2)
        down = max(down, 0.0)

        lag = _MODIFIER_LAG * math.exp(rng.gauss(0.0, _MODIFIER_LAG_SIGMA))
        up = release_at[last] + lag
        # The character is fixed once its key is down, so the modifier is free
        # to lift from that instant -- but never before it.
        up = max(up, press_at[last] + _EPSILON)
        # And it must be gone before any key that does not want it.
        if last + 1 < len(presses):
            up = min(up, press_at[last + 1] - _EPSILON)
        up = max(up, down + _EPSILON)

        events.append((down, Action.DOWN, name, -1))
        events.append((up, Action.UP, name, -1))
    return events


def plan(
    text: str,
    cadence: Cadence,
    *,
    seed: int | None = None,
    modifiers_of: ModifiersOf | None = None,
) -> list[Event]:
    """Compile ``text`` into the exact stream of key transitions to perform.

    The returned list is both what the engine types and what the ETA is
    measured from, so the estimate cannot drift away from reality. Passing a
    ``seed`` makes the schedule reproducible, which is what the tests rely on.

    ``modifiers_of`` answers which modifiers a character needs -- a question
    only the keyboard layout can settle, which is why it arrives as a function
    rather than a table. Given one, Shift and AltGr become scheduled
    keystrokes with a lead and a lag of their own. Without one they are left
    to the backend, which presses them at the same instant as the key.
    """
    cadence.validate()
    rng = random.Random(seed)

    presses = _plan_presses(text, cadence, rng)
    if not presses:
        return []
    presses = _calibrate(presses, text, cadence, rng)

    press_at: list[float] = []
    clock = 0.0
    for press in presses:
        clock += press.interval
        press_at.append(clock)
    release_at = _release_times(presses, press_at, cadence, rng)

    timeline: list[tuple[float, Action, str, int]] = []
    for i, press in enumerate(presses):
        timeline.append((press_at[i], Action.DOWN, press.value, press.index))
        timeline.append((release_at[i], Action.UP, press.value, -1))
    if modifiers_of is not None:
        timeline += _schedule_modifiers(
            presses, press_at, release_at, modifiers_of, rng
        )
    # UP is 0 and DOWN is 1, so a release scheduled for the same instant as a
    # press happens first — never a phantom double-press.
    timeline.sort(key=lambda item: (item[0], item[1]))

    events: list[Event] = []
    previous = 0.0
    for moment, action, value, index in timeline:
        events.append(Event(max(moment - previous, 0.0), action, value, index))
        previous = moment
    return events


def duration(events: list[Event]) -> float:
    """Total seconds the schedule takes, excluding any start countdown.

    This runs to the final key *release*, so it is a hair longer than
    :func:`typing_duration` -- the difference is one hold time.
    """
    return sum(event.delay for event in events)


def typing_duration(events: list[Event]) -> float:
    """Seconds from the start to the last key *press*.

    This is the span words-per-minute is defined over: text is produced when
    keys go down, and the trailing release of the last character is not part
    of writing it.
    """
    clock = 0.0
    last_press = 0.0
    for event in events:
        clock += event.delay
        if event.action is Action.DOWN:
            last_press = clock
    return last_press


@dataclass(frozen=True, slots=True)
class StrokeStats:
    """The timings a keystroke listener on the far end would measure."""

    dwell: list[float]     # press to its own release
    flight: list[float]    # release to the next press; negative means rollover
    interval: list[float]  # press to the next press

    @property
    def rollover_rate(self) -> float:
        """Share of transitions where one key is still down as the next goes."""
        if not self.flight:
            return 0.0
        return sum(1 for f in self.flight if f < 0) / len(self.flight)


def stroke_stats(events: list[Event]) -> StrokeStats:
    """Measure a schedule the way the receiving end would.

    Used by the tests: the claim that this looks like typing is only worth
    making if it can be checked.
    """
    clock = 0.0
    down_at: dict[str, float] = {}
    order: list[tuple[float, float]] = []  # (press, release)
    for event in events:
        clock += event.delay
        if event.action is Action.DOWN:
            down_at[event.value] = clock
        else:
            pressed = down_at.pop(event.value, None)
            if pressed is not None:
                order.append((pressed, clock))

    order.sort()
    dwell = [release - press for press, release in order]
    interval = [
        order[i + 1][0] - order[i][0] for i in range(len(order) - 1)
    ]
    flight = [
        order[i + 1][0] - order[i][1] for i in range(len(order) - 1)
    ]
    return StrokeStats(dwell=dwell, flight=flight, interval=interval)


def format_duration(seconds: float) -> str:
    """Render a duration the way a status bar should show it."""
    seconds = max(0.0, seconds)
    if seconds < 1:
        return "<1s"
    if seconds < 60:
        return f"{seconds:.0f}s"
    minutes, secs = divmod(int(seconds), 60)
    if minutes < 60:
        return f"{minutes}m {secs:02d}s"
    hours, minutes = divmod(minutes, 60)
    return f"{hours}h {minutes:02d}m"
