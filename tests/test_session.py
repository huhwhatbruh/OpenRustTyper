"""Running a job: fidelity, control, and never leaving keys held."""

from __future__ import annotations

import time

import pytest

from openautotyper.backends.null import NullBackend
from openautotyper.session import State, TypeSession
from openautotyper.timing import Cadence

FAST = Cadence(wpm=1200)
SAMPLE = "Hello, world!\nSecond line.\tTabbed.\nDone."


def run(text: str, cadence: Cadence = FAST, **kw):
    backend = NullBackend()
    session = TypeSession(text, cadence, backend, countdown=0, panic_key=None, **kw)
    return session, backend, session.run()


def test_text_is_delivered_exactly() -> None:
    _session, backend, result = run(SAMPLE)
    assert result.state is State.DONE
    assert backend.text == SAMPLE
    assert result.typed == result.total == len(SAMPLE)


def test_typos_still_deliver_the_exact_text() -> None:
    """A slip that is not corrected is a corrupted document, not realism."""
    for seed in range(8):
        _, backend, result = run(SAMPLE, Cadence(wpm=1200, typo_rate=0.2), seed=seed)
        assert backend.text == SAMPLE, f"seed {seed}"
        assert result.state is State.DONE


def test_the_device_is_always_released_and_closed() -> None:
    _, backend, _ = run(SAMPLE)
    assert backend.released >= 1
    assert backend.closed


def test_device_is_released_even_when_the_run_is_stopped() -> None:
    backend = NullBackend()
    session = TypeSession("x" * 500, Cadence(wpm=60), backend,
                          countdown=0, panic_key=None)
    session.start()
    time.sleep(0.2)
    session.stop("test")
    session.join(timeout=5)
    assert session.result is not None
    assert session.result.state is State.STOPPED
    assert backend.released >= 1 and backend.closed


def test_a_failing_backend_reports_rather_than_raises() -> None:
    class Broken(NullBackend):
        def open(self) -> None:
            from openautotyper.backends.base import BackendError

            raise BackendError("no device here")

    session = TypeSession("abc", FAST, Broken(), countdown=0, panic_key=None)
    result = session.run()
    assert result.state is State.FAILED
    assert "no device here" in result.reason


def test_unreachable_characters_go_through_the_clipboard() -> None:
    backend = NullBackend(reachable="abc")
    result = TypeSession("abc€", FAST, backend, countdown=0, panic_key=None).run()
    assert backend.pasted == ["€"]
    assert result.skipped == []


def test_characters_nothing_can_type_are_reported_not_swallowed() -> None:
    class NoClipboard(NullBackend):
        def paste(self, text: str) -> bool:
            return False

    backend = NoClipboard(reachable="abc")
    result = TypeSession("abc€", FAST, backend, countdown=0, panic_key=None).run()
    assert result.skipped == [(3, "€")]
    assert result.state is State.DONE


def test_pause_holds_and_resume_continues() -> None:
    backend = NullBackend()
    session = TypeSession("y" * 200, Cadence(wpm=400), backend,
                          countdown=0, panic_key=None)
    session.start()
    time.sleep(0.2)
    session.pause()
    time.sleep(0.3)
    held = len(backend.output)
    time.sleep(0.3)
    assert len(backend.output) == held, "typing continued while paused"
    session.resume()
    session.join(timeout=10)
    assert backend.text == "y" * 200


def test_a_paused_session_can_still_be_stopped() -> None:
    """Otherwise Pause followed by Stop would hang the app."""
    backend = NullBackend()
    session = TypeSession("z" * 400, Cadence(wpm=200), backend,
                          countdown=0, panic_key=None)
    session.start()
    time.sleep(0.15)
    session.pause()
    time.sleep(0.1)
    session.stop("stopped while paused")
    session.join(timeout=3)
    assert session.result is not None
    assert session.result.state is State.STOPPED


def test_countdown_is_interruptible() -> None:
    backend = NullBackend()
    session = TypeSession("abc", FAST, backend, countdown=30, panic_key=None)
    session.start()
    time.sleep(0.1)
    started = time.perf_counter()
    session.stop("cancelled")
    session.join(timeout=3)
    assert time.perf_counter() - started < 1.0
    assert backend.text == ""


def test_progress_is_monotonic_and_ends_complete() -> None:
    seen: list[tuple[int, float]] = []
    backend = NullBackend()
    TypeSession(
        SAMPLE, FAST, backend, countdown=0, panic_key=None, progress_hz=0,
        on_progress=lambda p: seen.append((p.typed, p.remaining)),
    ).run()
    typed = [t for t, _ in seen]
    assert typed == sorted(typed)
    assert typed[-1] == len(SAMPLE)
    remaining = [r for _, r in seen]
    assert remaining == sorted(remaining, reverse=True)
    assert remaining[-1] == pytest.approx(0.0, abs=1e-9)


def test_a_throwing_progress_callback_does_not_break_typing() -> None:
    def boom(_p: object) -> None:
        raise RuntimeError("UI exploded")

    backend = NullBackend()
    result = TypeSession(SAMPLE, FAST, backend, countdown=0, panic_key=None,
                         progress_hz=0, on_progress=boom).run()
    assert result.state is State.DONE
    assert backend.text == SAMPLE


def test_estimate_matches_the_schedule_actually_run() -> None:
    """The estimate is a sum of the very events that will be typed.

    That is the whole guarantee, and it is stronger than agreeing with the
    closed-form arithmetic: each run draws its own pace, so the schedule is a
    few percent either side of the nominal rate and the estimate goes with it
    rather than reporting the nominal and being wrong.
    """
    from openautotyper.timing import Action

    session = TypeSession(SAMPLE, Cadence(wpm=80), NullBackend(),
                          countdown=0, panic_key=None)
    clock = last_press = 0.0
    for event in session.events:
        clock += event.delay
        if event.action is Action.DOWN:
            last_press = clock
    assert session.estimated == pytest.approx(last_press)
    assert session.estimated == pytest.approx(len(SAMPLE) / 5 * 60 / 80, rel=0.12)


def test_a_session_cannot_be_started_twice() -> None:
    session = TypeSession("a", FAST, NullBackend(), countdown=0, panic_key=None)
    session.start()
    with pytest.raises(RuntimeError):
        session.start()
    session.join(timeout=3)


def test_empty_text_completes_immediately() -> None:
    _, backend, result = run("")
    assert result.state is State.DONE
    assert backend.text == ""


# ---------------------------------------------------------------------------
# The abort key is a safety promise, so a failure to attach must be loud
# ---------------------------------------------------------------------------
def test_a_failed_abort_key_is_reported_rather_than_assumed(monkeypatch) -> None:
    """Typing needs /dev/uinput; the abort key needs /dev/input/event*.

    Those are different permissions, so a machine can type perfectly and have
    no way to stop. ``watcher.start()``'s return value used to be dropped on
    the floor: the run went ahead, the caller had already told the user which
    key to press, and that key did nothing.
    """
    import openautotyper.panic as panic

    monkeypatch.setattr(panic.PanicWatcher, "start", lambda self: False)
    messages: list[str] = []
    result = TypeSession(
        "hello", Cadence(wpm=2000), NullBackend(), countdown=0,
        panic_key="KEY_ESC", progress_hz=0,
        on_progress=lambda p: messages.append(p.message),
    ).run()

    assert result.state is State.DONE, "a missing abort key must not fail the run"
    assert result.panic_active is False
    assert any("abort key" in m for m in messages), messages


def test_a_working_abort_key_is_recorded_as_working(monkeypatch) -> None:
    import openautotyper.panic as panic

    monkeypatch.setattr(panic.PanicWatcher, "start", lambda self: True)
    monkeypatch.setattr(panic.PanicWatcher, "stop", lambda self: None)
    result = TypeSession("hello", Cadence(wpm=2000), NullBackend(), countdown=0,
                         panic_key="KEY_ESC", progress_hz=0).run()
    assert result.panic_active is True


def test_no_abort_key_asked_for_is_not_a_warning() -> None:
    result = TypeSession("hello", Cadence(wpm=2000), NullBackend(), countdown=0,
                         panic_key=None, progress_hz=0).run()
    assert result.panic_active is True


# ---------------------------------------------------------------------------
# A preview must not take as long as the thing it previews
# ---------------------------------------------------------------------------
def test_a_non_realtime_run_does_not_wait_out_the_schedule() -> None:
    """What --dry-run does. It used to sleep the full simulated duration.

    900 characters at 70 wpm is two and a half minutes of pacing, which the
    dry run sat through in silence before printing a word.
    """
    import time

    text = "The quick brown fox jumps over the lazy dog. " * 20
    session = TypeSession(text, Cadence(wpm=70), NullBackend(), countdown=0,
                          panic_key=None, progress_hz=0, realtime=False)
    assert session.estimated > 60, "the schedule really is minutes long"

    started = time.perf_counter()
    result = session.run()
    assert time.perf_counter() - started < 5.0
    assert result.state is State.DONE
    assert result.typed == len(text)


def test_a_non_realtime_run_types_exactly_the_same_text() -> None:
    """Speed must not cost fidelity: the preview is the point."""
    text = 'Fidelity: "quotes", (parens) & 100%!\nSecond line.'
    fast = NullBackend()
    TypeSession(text, Cadence(wpm=70, typo_rate=0.05), fast, countdown=0,
                panic_key=None, progress_hz=0, realtime=False, seed=4).run()
    paced = NullBackend()
    TypeSession(text, Cadence(wpm=2000, typo_rate=0.05), paced, countdown=0,
                panic_key=None, progress_hz=0, seed=4).run()
    assert fast.text == text
    assert fast.text == paced.text


def test_a_non_realtime_run_is_still_abortable() -> None:
    session = TypeSession("a" * 5000, Cadence(wpm=70), NullBackend(), countdown=0,
                          panic_key=None, progress_hz=0, realtime=False)
    session.stop("changed my mind")
    result = session.run()
    assert result.state is State.STOPPED
