"""Drift-corrected pacing.

``time.sleep(interval)`` in a loop is wrong twice over. Each sleep overshoots
by a scheduler quantum, and the overshoots accumulate, so a five-minute job
lands a noticeable distance from its estimate. And a sleep cannot be
interrupted, so an abort during a two-second pause waits out the pause.

Pacing against absolute deadlines fixes the drift; waiting on an Event instead
of sleeping fixes the latency.
"""

from __future__ import annotations

import threading
import time

#: Below this, the OS scheduler is not reliable enough and a short spin beats
#: a syscall. It costs a millisecond of one core per keystroke at most.
_SPIN_THRESHOLD = 0.0015

#: Lateness beyond this is written off rather than repaid. Repaying it would
#: mean firing several keystrokes back to back to "catch up", which is both
#: visibly wrong and the fastest way to overrun an input field.
_SLIP_FORGIVENESS = 0.05


class Pacer:
    """Waits out an event schedule without drifting and without blocking abort."""

    def __init__(
        self,
        stop: threading.Event | None = None,
        pause: threading.Event | None = None,
    ) -> None:
        self._stop = stop or threading.Event()
        self._pause = pause
        self._start = time.perf_counter()
        self._deadline = self._start
        #: Total time lost to a busy machine, in seconds. Diagnostic only.
        self.slip = 0.0
        #: Total time spent held by the pause flag.
        self.paused_for = 0.0

    @property
    def elapsed(self) -> float:
        return time.perf_counter() - self._start

    def reset(self) -> None:
        """Re-anchor the schedule to now, e.g. after a countdown."""
        self._start = self._deadline = time.perf_counter()

    def wait(self, delay: float) -> bool:
        """Hold until ``delay`` after the previous deadline.

        Returns False if the stop flag was raised while waiting.
        """
        self._deadline += delay
        while True:
            if self._stop.is_set():
                return False

            if self._pause is not None and self._pause.is_set():
                held = time.perf_counter()
                while self._pause.is_set() and not self._stop.is_set():
                    time.sleep(0.02)
                # Time spent paused is not time the schedule owes anyone.
                gap = time.perf_counter() - held
                self._deadline += gap
                self.paused_for += gap
                continue

            remaining = self._deadline - time.perf_counter()
            if remaining <= 0.0:
                if remaining < -_SLIP_FORGIVENESS:
                    self.slip += -remaining
                    self._deadline = time.perf_counter()
                return True

            if remaining > _SPIN_THRESHOLD:
                if self._stop.wait(remaining - _SPIN_THRESHOLD):
                    return False
            else:
                target = time.perf_counter() + remaining
                while time.perf_counter() < target:
                    pass
                return True
