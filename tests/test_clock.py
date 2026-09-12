"""Pacing: no accumulated drift, no uninterruptible waits."""

from __future__ import annotations

import threading
import time

import pytest

from openautotyper.clock import Pacer


def test_many_short_waits_do_not_accumulate_drift() -> None:
    pacer = Pacer()
    started = time.perf_counter()
    for _ in range(200):
        pacer.wait(0.005)
    elapsed = time.perf_counter() - started
    assert elapsed == pytest.approx(1.0, abs=0.02)


def test_naive_sleep_would_have_drifted() -> None:
    """Guards the premise: if sleep() stopped drifting, the Pacer is dead weight."""
    started = time.perf_counter()
    for _ in range(200):
        time.sleep(0.005)
    assert time.perf_counter() - started > 1.005


def test_stop_interrupts_a_long_wait_immediately() -> None:
    stop = threading.Event()
    pacer = Pacer(stop)
    threading.Timer(0.1, stop.set).start()
    started = time.perf_counter()
    assert pacer.wait(10.0) is False
    assert time.perf_counter() - started < 0.5


def test_an_already_raised_stop_returns_at_once() -> None:
    stop = threading.Event()
    stop.set()
    assert Pacer(stop).wait(10.0) is False


def test_pause_extends_the_schedule_rather_than_compressing_it() -> None:
    pause = threading.Event()
    pacer = Pacer(threading.Event(), pause)
    pause.set()
    threading.Timer(0.3, pause.clear).start()
    started = time.perf_counter()
    pacer.wait(0.1)
    elapsed = time.perf_counter() - started
    assert elapsed == pytest.approx(0.4, abs=0.08)
    assert pacer.paused_for == pytest.approx(0.3, abs=0.08)


def test_lost_time_is_written_off_not_repaid_in_a_burst() -> None:
    """Catching up would fire several keystrokes at once — visibly wrong."""
    pacer = Pacer()
    pacer.wait(0.01)
    time.sleep(0.3)              # the machine stalls
    started = time.perf_counter()
    pacer.wait(0.01)             # deadline is long past
    pacer.wait(0.01)             # must still honour its own interval
    assert time.perf_counter() - started == pytest.approx(0.01, abs=0.01)
    assert pacer.slip > 0.2
