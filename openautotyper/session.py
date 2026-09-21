"""Running a typing job: countdown, keystrokes, progress, and stopping.

The one place that knows about all the moving parts. Everything it touches --
the schedule, the backend, the pacer, the panic watcher -- is independently
testable; this module's own job is to sequence them and to guarantee that no
matter how the run ends, the keyboard is left in the state it was found.
"""

from __future__ import annotations

import contextlib
import threading
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from enum import StrEnum

from .backends.base import Backend, BackendError
from .clock import Pacer
from .panic import PanicWatcher
from .timing import Action, Cadence, Event, plan, typing_duration


class State(StrEnum):
    IDLE = "idle"
    COUNTDOWN = "countdown"
    TYPING = "typing"
    PAUSED = "paused"
    DONE = "done"
    STOPPED = "stopped"
    FAILED = "failed"


@dataclass(frozen=True, slots=True)
class Progress:
    """A snapshot handed to the UI. Cheap to build, safe to drop."""

    state: State
    typed: int
    total: int
    elapsed: float
    remaining: float
    message: str = ""

    @property
    def fraction(self) -> float:
        return self.typed / self.total if self.total else 0.0

    @property
    def wpm(self) -> float:
        return (self.typed / 5.0) / (self.elapsed / 60.0) if self.elapsed > 0 else 0.0


@dataclass(slots=True)
class Result:
    """What happened, once the run is over."""

    state: State
    typed: int
    total: int
    duration: float
    reason: str = ""
    #: Characters no layout and no clipboard could produce, as (index, char).
    skipped: list[tuple[int, str]] = field(default_factory=list)
    #: Seconds the schedule fell behind because the machine was busy.
    slip: float = 0.0
    #: Whether the abort key was actually watching during this run. False means
    #: one was asked for and could not be attached -- the run went ahead with
    #: no kill switch, which the caller has to be able to say out loud.
    panic_active: bool = True

    @property
    def completed(self) -> bool:
        return self.state is State.DONE


class TypeSession:
    """One typing job. Not reusable: build a new session per run."""

    def __init__(
        self,
        text: str,
        cadence: Cadence,
        backend: Backend,
        *,
        countdown: float = 5.0,
        panic_key: str | None = "KEY_ESC",
        any_key_stops: bool = False,
        on_progress: Callable[[Progress], None] | None = None,
        progress_hz: float = 15.0,
        seed: int | None = None,
        realtime: bool = True,
    ) -> None:
        self.text = text
        self.backend = backend
        self.countdown = max(0.0, countdown)
        self.panic_key = panic_key
        self.any_key_stops = any_key_stops
        self._on_progress = on_progress
        self._min_gap = 1.0 / progress_hz if progress_hz > 0 else 0.0
        #: Whether to wait out the schedule or walk it as fast as it will go.
        #: A dry run wants the second: it is a preview, and a preview that takes
        #: as long as the typing would is not one.
        self._realtime = realtime
        #: Set by :meth:`run` once the abort key has been attached, or not.
        self.panic_active = True

        # The backend is the only thing that knows which modifiers a character
        # needs on this keyboard layout, so the schedule asks it. That is what
        # lets Shift be a keystroke with a lead of its own rather than an event
        # the device fires in the same instant as the letter.
        self.events: list[Event] = plan(
            text, cadence, seed=seed, modifiers_of=backend.modifiers_for
        )
        self.total = len(text)
        #: Seconds of work left after each event, precomputed so the ETA is a
        #: lookup rather than a guess.
        self._remaining_after = self._suffix_sums(self.events)
        #: Measured to the last key *press*, which is the span words-per-minute
        #: is defined over. Running to the final release instead would add one
        #: hold time and make the ETA disagree with the WPM that was asked for.
        self.estimated = typing_duration(self.events)

        self._stop = threading.Event()
        self._pause = threading.Event()
        self._state = State.IDLE
        self._typed = 0
        self._last_emit = 0.0
        self._thread: threading.Thread | None = None
        self._result: Result | None = None

    # -- introspection -----------------------------------------------------
    @staticmethod
    def _suffix_sums(events: list[Event]) -> list[float]:
        out = [0.0] * (len(events) + 1)
        acc = 0.0
        for i in range(len(events) - 1, -1, -1):
            acc += events[i].delay
            out[i] = acc
        return out

    @property
    def state(self) -> State:
        return self._state

    @property
    def result(self) -> Result | None:
        return self._result

    # -- control -----------------------------------------------------------
    def start(self) -> None:
        """Run in a background thread. Use :meth:`join` to wait for it."""
        if self._thread is not None:
            raise RuntimeError("session already started")
        self._thread = threading.Thread(target=self.run, name="typing", daemon=True)
        self._thread.start()

    def join(self, timeout: float | None = None) -> None:
        if self._thread is not None:
            self._thread.join(timeout)

    def stop(self, reason: str = "stopped by user") -> None:
        self._stop_reason = reason
        self._pause.clear()  # a paused run must still be abortable
        self._stop.set()

    def pause(self) -> None:
        if self._state is State.TYPING:
            self._pause.set()

    def resume(self) -> None:
        self._pause.clear()

    @property
    def paused(self) -> bool:
        return self._pause.is_set()

    # -- the run -----------------------------------------------------------
    def run(self) -> Result:
        self._stop_reason = "stopped by user"
        pacer = Pacer(self._stop, self._pause)
        skipped: list[tuple[int, str]] = []
        watcher: PanicWatcher | None = None
        opened = False

        try:
            try:
                self.backend.open()
                opened = True
            except BackendError as exc:
                return self._finish(State.FAILED, 0.0, str(exc), skipped, 0.0)

            if self.panic_key or self.any_key_stops:
                exclude = getattr(self.backend, "device_path", None)
                watcher = PanicWatcher(
                    lambda: self.stop(f"aborted with {self.panic_key or 'a key'}"),
                    key=self.panic_key or "KEY_ESC",
                    exclude_paths=(exclude,) if exclude else (),
                    any_key=self.any_key_stops,
                )
                # Dropping this bool on the floor was the worst bug in here.
                # The watcher needs to read /dev/input/event*, which is a
                # different permission from the /dev/uinput the typing needs:
                # a machine can be perfectly able to type and unable to stop.
                # The caller promised the user an abort key before this point,
                # so a failure has to be said out loud, not inferred later from
                # the fact that Escape did nothing.
                self.panic_active = watcher.start()
                if not self.panic_active:
                    watcher = None
                    self._publish(Progress(
                        State.COUNTDOWN, 0, self.total, 0.0, self.estimated,
                        "WARNING: the abort key could not be attached — no "
                        "keyboard in /dev/input is readable (add yourself to "
                        "group 'input'). Typing will not be stoppable from "
                        "the keyboard.",
                    ))

            if not self._run_countdown(pacer):
                return self._finish(State.STOPPED, pacer.elapsed, self._stop_reason,
                                    skipped, pacer.slip)

            self._state = State.TYPING
            pacer.reset()
            events = self.events

            for i, event in enumerate(events):
                if self._realtime:
                    if not pacer.wait(event.delay):
                        return self._finish(State.STOPPED, pacer.elapsed,
                                            self._stop_reason, skipped, pacer.slip)
                elif self._stop.is_set():
                    # Not paced, but still abortable: a preview of a long
                    # document is fast, not instantaneous.
                    return self._finish(State.STOPPED, pacer.elapsed,
                                        self._stop_reason, skipped, pacer.slip)
                try:
                    if event.action is Action.DOWN:
                        ok = self.backend.key_down(event.value)
                        if not ok and len(event.value) == 1:
                            # No keystroke reaches this character on this
                            # layout; the clipboard is the only way in. Named
                            # keys are excluded: there is nothing to paste.
                            ok = self.backend.paste(event.value)
                        if not ok and event.index >= 0:
                            skipped.append((event.index, event.value))
                        if event.index >= 0:
                            self._typed = event.index + 1
                    else:
                        # A release for a key that never went down is expected
                        # whenever the press fell back to the clipboard.
                        self.backend.key_up(event.value)
                except BackendError as exc:
                    return self._finish(State.FAILED, pacer.elapsed, str(exc),
                                        skipped, pacer.slip)
                self._emit(pacer, i)

            return self._finish(State.DONE, pacer.elapsed, "", skipped, pacer.slip)
        finally:
            if watcher is not None:
                watcher.stop()
            if opened:
                # Order matters: let go of held keys while the device still
                # exists, then tear it down.
                try:
                    self.backend.release_all()
                finally:
                    self.backend.close()

    def _run_countdown(self, pacer: Pacer) -> bool:
        if self.countdown <= 0:
            return not self._stop.is_set()
        self._state = State.COUNTDOWN
        deadline = time.perf_counter() + self.countdown
        while True:
            left = deadline - time.perf_counter()
            if left <= 0:
                return not self._stop.is_set()
            if self._stop.wait(min(0.1, left)):
                return False
            self._publish(
                Progress(
                    State.COUNTDOWN, 0, self.total, 0.0, self.estimated,
                    f"starting in {max(0.0, deadline - time.perf_counter()):.0f}s"
                    " — click the target window now",
                )
            )

    # -- progress ----------------------------------------------------------
    def _emit(self, pacer: Pacer, event_index: int) -> None:
        if self._on_progress is None:
            return
        now = time.perf_counter()
        if now - self._last_emit < self._min_gap:
            return
        self._last_emit = now
        state = State.PAUSED if self._pause.is_set() else State.TYPING
        self._publish(
            Progress(
                state, self._typed, self.total, pacer.elapsed,
                self._remaining_after[event_index + 1],
            )
        )

    def _publish(self, progress: Progress) -> None:
        if self._on_progress is None:
            return
        # A failing UI callback must never take the typing down with it.
        with contextlib.suppress(Exception):
            self._on_progress(progress)

    def _finish(
        self,
        state: State,
        elapsed: float,
        reason: str,
        skipped: list[tuple[int, str]],
        slip: float,
    ) -> Result:
        self._state = state
        result = Result(
            state=state, typed=self._typed, total=self.total,
            duration=elapsed, reason=reason, skipped=skipped, slip=slip,
            panic_active=self.panic_active,
        )
        self._result = result
        self._publish(
            Progress(state, self._typed, self.total, elapsed, 0.0, reason)
        )
        return result
