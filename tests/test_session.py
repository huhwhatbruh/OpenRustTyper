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
    session = TypeSession(SAMPLE, Cadence(wpm=80), NullBackend(),
                          countdown=0, panic_key=None)
    assert session.estimated == pytest.approx(len(SAMPLE) / 5 * 60 / 80)


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
