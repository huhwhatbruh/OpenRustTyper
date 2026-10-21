//! Running a typing job: countdown, keystrokes, progress, and stopping.
//!
//! The one place that knows about all the moving parts. Everything it touches
//! -- the schedule, the backend, the pacer, the panic watcher -- is
//! independently testable; this module's own job is to sequence them and to
//! guarantee that no matter how the run ends, the keyboard is left in the state
//! it was found.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::backend::Backend;
use crate::clock::{Flag, Pacer};
use crate::panic::PanicWatcher;
use crate::timing::{plan, typing_duration, Action, Cadence, Event};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Idle = 0,
    Countdown = 1,
    Typing = 2,
    Paused = 3,
    Done = 4,
    Stopped = 5,
    Failed = 6,
}

impl State {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Countdown,
            2 => Self::Typing,
            3 => Self::Paused,
            4 => Self::Done,
            5 => Self::Stopped,
            6 => Self::Failed,
            _ => Self::Idle,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Countdown => "countdown",
            Self::Typing => "typing",
            Self::Paused => "paused",
            Self::Done => "done",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// Whether the run has ended, one way or another.
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Done | Self::Stopped | Self::Failed)
    }
}

/// A snapshot handed to the UI. Cheap to build, safe to drop.
#[derive(Debug, Clone)]
pub struct Progress {
    pub state: State,
    pub typed: usize,
    pub total: usize,
    pub elapsed: f64,
    pub remaining: f64,
    pub message: String,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            self.typed as f64 / self.total as f64
        }
    }

    pub fn wpm(&self) -> f64 {
        if self.elapsed > 0.0 {
            (self.typed as f64 / 5.0) / (self.elapsed / 60.0)
        } else {
            0.0
        }
    }
}

/// What happened, once the run is over.
#[derive(Debug, Clone)]
pub struct RunResult {
    pub state: State,
    pub typed: usize,
    pub total: usize,
    pub duration: f64,
    pub reason: String,
    /// Characters no layout and no clipboard could produce, as (index, char).
    pub skipped: Vec<(usize, String)>,
    /// Seconds the schedule fell behind because the machine was busy.
    pub slip: f64,
    /// Whether a keyboard kill switch was live for this run. False covers both
    /// "none was asked for" and "one was asked for and could not be attached";
    /// either way the caller must not tell the user that a key will stop it.
    pub panic_active: bool,
}

impl RunResult {
    pub fn completed(&self) -> bool {
        self.state == State::Done
    }
}

type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

/// Options for [`TypeSession::new`].
pub struct SessionOptions {
    pub countdown: f64,
    pub panic_key: Option<String>,
    pub any_key_stops: bool,
    pub on_progress: Option<ProgressFn>,
    pub progress_hz: f64,
    pub seed: Option<u64>,
    /// Whether to wait out the schedule or walk it as fast as it will go. A dry
    /// run wants the second: it is a preview, and a preview that takes as long
    /// as the typing would is not one.
    pub realtime: bool,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            countdown: 5.0,
            panic_key: Some("KEY_ESC".into()),
            any_key_stops: false,
            on_progress: None,
            progress_hz: 15.0,
            seed: None,
            realtime: true,
        }
    }
}

struct Shared {
    stop: Flag,
    pause: Flag,
    state: AtomicU8,
    reason: Mutex<String>,
    /// The last progress snapshot, so pause and resume can publish an honest
    /// one without the worker.
    snapshot: Mutex<Option<Progress>>,
    on_progress: Option<ProgressFn>,
}

impl Shared {
    fn publish(&self, progress: Progress) {
        *self.snapshot.lock().unwrap() = Some(progress.clone());
        if let Some(f) = &self.on_progress {
            // A failing UI callback must never take the typing down with it.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(progress)));
        }
    }
}

/// A cloneable handle for steering a session from another thread.
#[derive(Clone)]
pub struct Control(Arc<Shared>);

impl Control {
    pub fn state(&self) -> State {
        State::from_u8(self.0.state.load(Ordering::SeqCst))
    }

    fn set_state(&self, state: State) {
        self.0.state.store(state as u8, Ordering::SeqCst);
    }

    pub fn stop(&self, reason: &str) {
        *self.0.reason.lock().unwrap() = reason.to_string();
        self.0.pause.clear(); // a paused run must still be abortable
        self.0.stop.set();
    }

    pub fn pause(&self) {
        if self.state() != State::Typing || self.0.pause.is_set() {
            return;
        }
        self.0.pause.set();
        // Progress is normally published after an event fires, and a paused run
        // is waiting before one fires -- so without this the window showed
        // "typing" for as long as the pause lasted.
        //
        // Clone out of the lock first: the guard from an `if let` scrutinee
        // lives for the whole body, and `publish` takes the same lock.
        let snapshot = self.0.snapshot.lock().unwrap().clone();
        if let Some(mut progress) = snapshot {
            progress.state = State::Paused;
            self.0.publish(progress);
        }
    }

    pub fn resume(&self) {
        if !self.0.pause.is_set() {
            return;
        }
        self.0.pause.clear();
        let snapshot = self.0.snapshot.lock().unwrap().clone();
        if let Some(mut progress) = snapshot {
            progress.state = State::Typing;
            self.0.publish(progress);
        }
    }

    pub fn paused(&self) -> bool {
        self.0.pause.is_set()
    }

    fn reason(&self) -> String {
        self.0.reason.lock().unwrap().clone()
    }
}

/// One typing job. Not reusable: build a new session per run.
pub struct TypeSession {
    backend: Box<dyn Backend>,
    countdown: f64,
    panic_key: Option<String>,
    any_key_stops: bool,
    min_gap: Duration,
    realtime: bool,
    pub events: Vec<Event>,
    pub total: usize,
    /// Seconds of work left after each event, precomputed so the ETA is a
    /// lookup rather than a guess.
    remaining_after: Vec<f64>,
    /// Measured to the last key *press*, which is the span words-per-minute is
    /// defined over. Running to the final release instead would add one hold
    /// time and make the ETA disagree with the WPM that was asked for.
    pub estimated: f64,
    control: Control,
    typed: usize,
    last_emit: Option<Instant>,
    panic_active: bool,
    /// Whether the slip the next backspace exists to undo was never typed.
    lost_slip: bool,
}

impl TypeSession {
    pub fn new(
        text: &str,
        cadence: &Cadence,
        mut backend: Box<dyn Backend>,
        opts: SessionOptions,
    ) -> Result<Self, String> {
        // The backend is the only thing that knows which modifiers a character
        // needs on this keyboard layout, so the schedule asks it. That is what
        // lets Shift be a keystroke with a lead of its own rather than an event
        // the device fires in the same instant as the letter.
        let events = {
            let cell = std::cell::RefCell::new(&mut backend);
            let ask = |token: &str| cell.borrow_mut().modifiers_for(token);
            plan(text, cadence, opts.seed, Some(&ask))?
        };

        let mut remaining_after = vec![0.0; events.len() + 1];
        let mut acc = 0.0;
        for i in (0..events.len()).rev() {
            acc += events[i].delay;
            remaining_after[i] = acc;
        }
        let estimated = typing_duration(&events);

        Ok(Self {
            backend,
            countdown: opts.countdown.max(0.0),
            panic_key: opts.panic_key,
            any_key_stops: opts.any_key_stops,
            min_gap: if opts.progress_hz > 0.0 {
                Duration::from_secs_f64(1.0 / opts.progress_hz)
            } else {
                Duration::ZERO
            },
            realtime: opts.realtime,
            total: text.chars().count(),
            events,
            remaining_after,
            estimated,
            control: Control(Arc::new(Shared {
                stop: Flag::new(),
                pause: Flag::new(),
                state: AtomicU8::new(State::Idle as u8),
                reason: Mutex::new("stopped by user".into()),
                snapshot: Mutex::new(None),
                on_progress: opts.on_progress,
            })),
            typed: 0,
            last_emit: None,
            // Nothing is watching until a watcher has actually opened a device.
            panic_active: false,
            lost_slip: false,
        })
    }

    /// A handle that can stop, pause and resume this session from anywhere.
    pub fn control(&self) -> Control {
        self.control.clone()
    }

    /// The backend, once the run is over -- a dry run reads its output here.
    pub fn backend(&self) -> &dyn Backend {
        &*self.backend
    }

    /// Run on a background thread; the handle yields the result.
    pub fn spawn(self) -> std::thread::JoinHandle<(RunResult, Self)> {
        std::thread::Builder::new()
            .name("typing".into())
            .spawn(move || {
                let mut session = self;
                let result = session.run();
                (result, session)
            })
            .expect("spawn typing thread")
    }

    /// Run to completion on this thread.
    pub fn run(&mut self) -> RunResult {
        let ctl = self.control.clone();
        let mut pacer = Pacer::new(ctl.0.stop.clone(), Some(ctl.0.pause.clone()));
        let mut skipped: Vec<(usize, String)> = Vec::new();
        let mut watcher: Option<PanicWatcher> = None;
        let mut opened = false;
        self.lost_slip = false;

        let result = (|| {
            if let Err(e) = self.backend.open() {
                return self.finish(State::Failed, 0.0, e.to_string(), skipped.clone(), 0.0);
            }
            opened = true;

            if self.panic_key.is_some() || self.any_key_stops {
                let exclude: Vec<String> = self.backend.device_path().into_iter().collect();
                // "Any key" reads the real keyboards. A backend that creates a
                // virtual one has to tell the watcher which node is ours, or the
                // first character typed aborts the run; a backend with no node
                // at all (XTEST) cannot be us. If the node exists but could not
                // be identified, drop the any-key half rather than aborting
                // every run on its own first keystroke.
                let any_key =
                    self.any_key_stops && (!self.backend.owns_device_node() || !exclude.is_empty());
                let key = self.panic_key.clone().unwrap_or_else(|| "KEY_ESC".into());
                let stopper = ctl.clone();
                let label = self.panic_key.clone().unwrap_or_else(|| "a key".into());
                let mut w = PanicWatcher::new(
                    move || stopper.stop(&format!("aborted with {label}")),
                    &key,
                    exclude,
                    any_key,
                );
                // The watcher needs to read /dev/input/event*, which is a
                // different permission from the /dev/uinput the typing needs: a
                // machine can be perfectly able to type and unable to stop. The
                // caller promised the user an abort key before this point, so a
                // failure has to be said out loud, not inferred later from the
                // fact that Escape did nothing.
                self.panic_active = w.start();
                if self.panic_active {
                    watcher = Some(w);
                } else {
                    self.publish(Progress {
                        state: State::Countdown,
                        typed: 0,
                        total: self.total,
                        elapsed: 0.0,
                        remaining: self.estimated,
                        message: "WARNING: the abort key could not be attached \u{2014} no \
                                  keyboard in /dev/input is readable (add yourself to group \
                                  'input'). Typing will not be stoppable from the keyboard."
                            .into(),
                    });
                }
            }

            if !self.run_countdown() {
                return self.finish(
                    State::Stopped,
                    pacer.elapsed(),
                    ctl.reason(),
                    skipped.clone(),
                    pacer.slip,
                );
            }

            ctl.set_state(State::Typing);
            pacer.reset();

            for i in 0..self.events.len() {
                let (delay, action) = (self.events[i].delay, self.events[i].action);
                let proceed = if self.realtime {
                    pacer.wait(delay)
                } else {
                    // Not paced, but still abortable: a preview of a long
                    // document is fast, not instantaneous.
                    !ctl.0.stop.is_set()
                };
                if !proceed {
                    return self.finish(
                        State::Stopped,
                        pacer.elapsed(),
                        ctl.reason(),
                        skipped.clone(),
                        pacer.slip,
                    );
                }

                let (value, index) = (self.events[i].value.clone(), self.events[i].index);
                let outcome = match action {
                    Action::Down => self.key_down(&value, index, &mut skipped),
                    // A release for a key that never went down is expected
                    // whenever the press fell back to the clipboard.
                    Action::Up => self.backend.key_up(&value).map(|_| ()),
                };
                if let Err(e) = outcome {
                    return self.finish(
                        State::Failed,
                        pacer.elapsed(),
                        e.to_string(),
                        skipped.clone(),
                        pacer.slip,
                    );
                }
                self.emit(&pacer, i);
            }

            self.finish(
                State::Done,
                pacer.elapsed(),
                String::new(),
                skipped.clone(),
                pacer.slip,
            )
        })();

        if let Some(mut w) = watcher {
            w.stop();
        }
        if opened {
            // Order matters: let go of held keys while the device still exists,
            // then tear it down.
            self.backend.release_all();
            self.backend.close();
        }
        result
    }

    fn key_down(
        &mut self,
        value: &str,
        index: Option<usize>,
        skipped: &mut Vec<(usize, String)>,
    ) -> Result<(), crate::backend::BackendError> {
        if self.lost_slip && value == "backspace" {
            // The mistyped key this exists to erase was never typed -- the
            // layout could not produce it, and the clipboard could not take it
            // -- so pressing backspace now would delete a character of the real
            // text instead of the slip.
            self.lost_slip = false;
            return Ok(());
        }
        let mut ok = self.backend.key_down(value)?;
        if !ok && value.chars().count() == 1 {
            // No keystroke reaches this character on this layout; the clipboard
            // is the only way in. Named keys are excluded: there is nothing to
            // paste.
            ok = self.backend.paste(value);
        }
        if value.chars().count() == 1 && index.is_none() {
            // This is a slip, not a character of the text: remember whether the
            // correction that follows has anything to correct.
            self.lost_slip = !ok;
        }
        if let Some(index) = index {
            if !ok {
                skipped.push((index, value.to_string()));
            } else {
                // Characters delivered, not positions reached: a character
                // nothing could type must not be counted as typed in the
                // summary.
                self.typed += 1;
            }
        }
        Ok(())
    }

    fn run_countdown(&mut self) -> bool {
        let ctl = self.control.clone();
        if self.countdown <= 0.0 {
            return !ctl.0.stop.is_set();
        }
        ctl.set_state(State::Countdown);
        let deadline = Instant::now() + Duration::from_secs_f64(self.countdown);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return !ctl.0.stop.is_set();
            }
            if ctl.0.stop.wait(left.min(Duration::from_millis(100))) {
                return false;
            }
            let secs = deadline
                .saturating_duration_since(Instant::now())
                .as_secs_f64();
            self.publish(Progress {
                state: State::Countdown,
                typed: 0,
                total: self.total,
                elapsed: 0.0,
                remaining: self.estimated,
                message: format!("starting in {secs:.0}s \u{2014} click the target window now"),
            });
        }
    }

    fn emit(&mut self, pacer: &Pacer, event_index: usize) {
        if self.control.0.on_progress.is_none() {
            return;
        }
        let now = Instant::now();
        if self
            .last_emit
            .is_some_and(|t| now.duration_since(t) < self.min_gap)
        {
            return;
        }
        self.last_emit = Some(now);
        let state = if self.control.paused() {
            State::Paused
        } else {
            State::Typing
        };
        self.publish(Progress {
            state,
            typed: self.typed,
            total: self.total,
            elapsed: pacer.elapsed(),
            remaining: self.remaining_after[event_index + 1],
            message: String::new(),
        });
    }

    fn publish(&self, progress: Progress) {
        self.control.0.publish(progress);
    }

    fn finish(
        &mut self,
        state: State,
        elapsed: f64,
        reason: String,
        skipped: Vec<(usize, String)>,
        slip: f64,
    ) -> RunResult {
        self.control.set_state(state);
        let result = RunResult {
            state,
            typed: self.typed,
            total: self.total,
            duration: elapsed,
            reason: reason.clone(),
            skipped,
            slip,
            panic_active: self.panic_active,
        };
        self.publish(Progress {
            state,
            typed: self.typed,
            total: self.total,
            elapsed,
            remaining: 0.0,
            message: reason,
        });
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendError, NullBackend};
    use std::sync::atomic::AtomicUsize;

    const TEXT: &str = "Hello, World! The quick brown fox.\nSecond line.\tTabbed.";

    fn quick() -> Cadence {
        Cadence {
            wpm: 2000.0,
            ..Default::default()
        }
    }

    fn opts() -> SessionOptions {
        SessionOptions {
            countdown: 0.0,
            panic_key: None,
            realtime: false,
            seed: Some(1),
            ..Default::default()
        }
    }

    fn session(
        text: &str,
        cadence: &Cadence,
        backend: NullBackend,
        o: SessionOptions,
    ) -> TypeSession {
        TypeSession::new(text, cadence, Box::new(backend), o).unwrap()
    }

    fn typed_text(s: &TypeSession) -> String {
        s.backend().recorded_text().unwrap()
    }

    #[test]
    fn text_is_delivered_exactly() {
        let mut s = session(TEXT, &quick(), NullBackend::default(), opts());
        let r = s.run();
        assert!(r.completed(), "{r:?}");
        assert_eq!(typed_text(&s), TEXT);
        assert_eq!(
            (r.typed, r.total),
            (TEXT.chars().count(), TEXT.chars().count())
        );
    }

    #[test]
    fn typos_still_deliver_the_exact_text() {
        let cadence = Cadence {
            typo_rate: 0.2,
            ..quick()
        };
        let mut s = session(TEXT, &cadence, NullBackend::default(), opts());
        assert!(s.run().completed());
        assert_eq!(typed_text(&s), TEXT);
    }

    #[test]
    fn the_device_is_always_released_and_closed() {
        let mut s = session(TEXT, &quick(), NullBackend::default(), opts());
        s.run();
        let backend = s.backend().recorded_text();
        assert!(backend.is_some());
    }

    /// Fails on the Nth key press.
    struct Flaky(NullBackend, usize);
    impl Backend for Flaky {
        fn name(&self) -> &'static str {
            "flaky"
        }
        fn open(&mut self) -> Result<(), BackendError> {
            self.0.open()
        }
        fn key_down(&mut self, t: &str) -> Result<bool, BackendError> {
            if self.0.text().chars().count() >= self.1 {
                return Err(BackendError("device vanished".into()));
            }
            self.0.key_down(t)
        }
        fn key_up(&mut self, t: &str) -> Result<bool, BackendError> {
            self.0.key_up(t)
        }
        fn release_all(&mut self) {
            self.0.release_all()
        }
        fn close(&mut self) {
            self.0.close()
        }
        fn recorded_text(&self) -> Option<String> {
            self.0.recorded_text()
        }
    }

    #[test]
    fn a_failing_backend_reports_rather_than_panics() {
        let mut s = TypeSession::new(
            TEXT,
            &quick(),
            Box::new(Flaky(NullBackend::default(), 5)),
            opts(),
        )
        .unwrap();
        let r = s.run();
        assert_eq!(r.state, State::Failed);
        assert_eq!(r.reason, "device vanished");
        assert_eq!(typed_text(&s), "Hello");
    }

    /// A backend that cannot even open.
    struct Broken;
    impl Backend for Broken {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn open(&mut self) -> Result<(), BackendError> {
            Err(BackendError("no device".into()))
        }
        fn key_down(&mut self, _: &str) -> Result<bool, BackendError> {
            unreachable!()
        }
        fn key_up(&mut self, _: &str) -> Result<bool, BackendError> {
            unreachable!()
        }
        fn release_all(&mut self) {
            panic!("never opened, nothing to release")
        }
        fn close(&mut self) {
            panic!("never opened, nothing to close")
        }
    }

    #[test]
    fn a_backend_that_cannot_open_fails_cleanly_without_touching_it() {
        let mut s = TypeSession::new("abc", &quick(), Box::new(Broken), opts()).unwrap();
        let r = s.run();
        assert_eq!(r.state, State::Failed);
        assert_eq!(r.reason, "no device");
    }

    #[test]
    fn unreachable_characters_go_through_the_clipboard() {
        let backend = NullBackend::reaching("abc ");
        let mut s = session("a\u{20AC}b", &quick(), backend, opts());
        let r = s.run();
        assert!(r.completed());
        assert!(r.skipped.is_empty());
        assert_eq!(typed_text(&s), "a\u{20AC}b");
    }

    /// A layout where nothing but a few letters can be typed, and no clipboard.
    struct NoClipboard(NullBackend);
    impl Backend for NoClipboard {
        fn name(&self) -> &'static str {
            "noclip"
        }
        fn open(&mut self) -> Result<(), BackendError> {
            self.0.open()
        }
        fn key_down(&mut self, t: &str) -> Result<bool, BackendError> {
            self.0.key_down(t)
        }
        fn key_up(&mut self, t: &str) -> Result<bool, BackendError> {
            self.0.key_up(t)
        }
        fn release_all(&mut self) {
            self.0.release_all()
        }
        fn close(&mut self) {
            self.0.close()
        }
        fn recorded_text(&self) -> Option<String> {
            self.0.recorded_text()
        }
    }

    #[test]
    fn characters_nothing_can_type_are_reported_not_swallowed() {
        let mut s = TypeSession::new(
            "a\u{20AC}b",
            &quick(),
            Box::new(NoClipboard(NullBackend::reaching("ab"))),
            opts(),
        )
        .unwrap();
        let r = s.run();
        assert!(r.completed());
        assert_eq!(r.skipped, vec![(1, "\u{20AC}".to_string())]);
        assert_eq!(typed_text(&s), "ab");
        // "Typed" has to mean delivered. Counting a position as complete when
        // the character was skipped made the summary claim text that never
        // arrived.
        assert_eq!(r.typed, 2);
    }

    #[test]
    fn a_correction_for_a_slip_that_never_landed_is_not_pressed() {
        // A backspace after a mistype that could not be typed deletes real text.
        // The neighbour the model picked is unreachable on this layout, and the
        // clipboard cannot take it either. The old engine pressed the correction
        // anyway, so every failed slip ate the character just typed.
        let text = "e".repeat(40);
        let cadence = Cadence {
            wpm: 2000.0,
            typo_rate: 0.25,
            ..Default::default()
        };
        for seed in 0..6 {
            let o = SessionOptions {
                seed: Some(seed),
                ..opts()
            };
            let mut s = TypeSession::new(
                &text,
                &cadence,
                Box::new(NoClipboard(NullBackend::reaching("e"))),
                o,
            )
            .unwrap();
            let r = s.run();
            assert!(r.completed());
            assert_eq!(typed_text(&s), text, "seed {seed}");
            assert!(r.skipped.is_empty(), "a failed slip is not a skip");
        }
    }

    #[test]
    fn pause_holds_and_resume_continues() {
        let o = SessionOptions {
            realtime: true,
            ..opts()
        };
        let cadence = Cadence {
            wpm: 300.0,
            ..Default::default()
        };
        let s = session(&"x".repeat(60), &cadence, NullBackend::default(), o);
        let ctl = s.control();
        let handle = s.spawn();
        std::thread::sleep(Duration::from_millis(150));
        ctl.pause();
        assert!(ctl.paused());
        std::thread::sleep(Duration::from_millis(100));
        assert!(ctl.paused());
        ctl.resume();
        let (r, _s) = handle.join().unwrap();
        assert!(r.completed(), "{r:?}");
    }

    #[test]
    fn pause_is_reported_before_the_next_keystroke() {
        // The window used to show "typing" throughout a pause: progress is
        // published after an event fires, and a paused run is waiting for one.
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        let o = SessionOptions {
            realtime: true,
            on_progress: Some(Arc::new(move |p| sink.lock().unwrap().push(p.state))),
            ..opts()
        };
        let cadence = Cadence {
            wpm: 60.0,
            ..Default::default()
        };
        let s = session(&"x".repeat(500), &cadence, NullBackend::default(), o);
        let ctl = s.control();
        let handle = s.spawn();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !seen.lock().unwrap().contains(&State::Typing) {
            std::thread::sleep(Duration::from_millis(5));
        }
        ctl.pause();
        assert!(seen.lock().unwrap().contains(&State::Paused));
        ctl.resume();
        ctl.stop("test finished");
        let _ = handle.join();
    }

    #[test]
    fn a_paused_session_can_still_be_stopped() {
        let o = SessionOptions {
            realtime: true,
            ..opts()
        };
        let cadence = Cadence {
            wpm: 100.0,
            ..Default::default()
        };
        let s = session(&"x".repeat(200), &cadence, NullBackend::default(), o);
        let ctl = s.control();
        let handle = s.spawn();
        std::thread::sleep(Duration::from_millis(100));
        ctl.pause();
        std::thread::sleep(Duration::from_millis(60));
        ctl.stop("enough");
        let (r, _s) = handle.join().unwrap();
        assert_eq!(r.state, State::Stopped);
        assert_eq!(r.reason, "enough");
        assert!(!ctl.paused());
    }

    #[test]
    fn countdown_is_interruptible() {
        let o = SessionOptions {
            countdown: 30.0,
            ..opts()
        };
        let s = session("abc", &quick(), NullBackend::default(), o);
        let ctl = s.control();
        let began = Instant::now();
        let handle = s.spawn();
        std::thread::sleep(Duration::from_millis(80));
        ctl.stop("changed my mind");
        let (r, s) = handle.join().unwrap();
        assert!(began.elapsed() < Duration::from_secs(3));
        assert_eq!(r.state, State::Stopped);
        assert_eq!(typed_text(&s), "");
    }

    #[test]
    fn progress_is_monotonic_and_ends_complete() {
        let seen = Arc::new(Mutex::new(Vec::<Progress>::new()));
        let sink = seen.clone();
        let o = SessionOptions {
            on_progress: Some(Arc::new(move |p| sink.lock().unwrap().push(p))),
            progress_hz: 0.0,
            ..opts()
        };
        let mut s = session(TEXT, &quick(), NullBackend::default(), o);
        s.run();
        let seen = seen.lock().unwrap();
        let typed: Vec<usize> = seen.iter().map(|p| p.typed).collect();
        assert!(typed.windows(2).all(|w| w[0] <= w[1]), "{typed:?}");
        let last = seen.last().unwrap();
        assert_eq!(last.state, State::Done);
        assert_eq!(last.typed, last.total);
    }

    #[test]
    fn a_panicking_progress_callback_does_not_break_typing() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let o = SessionOptions {
            on_progress: Some(Arc::new(move |_| {
                c.fetch_add(1, Ordering::SeqCst);
                panic!("ui blew up");
            })),
            progress_hz: 0.0,
            ..opts()
        };
        let mut s = session(TEXT, &quick(), NullBackend::default(), o);
        assert!(s.run().completed());
        assert_eq!(typed_text(&s), TEXT);
        assert!(calls.load(Ordering::SeqCst) > 0);
    }

    #[test]
    fn estimate_matches_the_schedule_actually_run() {
        let s = session(TEXT, &Cadence::default(), NullBackend::default(), opts());
        assert!((s.estimated - typing_duration(&s.events)).abs() < 1e-12);
        let remaining0 = s.remaining_after[0];
        assert!((remaining0 - crate::timing::duration(&s.events)).abs() < 1e-9);
    }

    #[test]
    fn empty_text_completes_immediately() {
        let mut s = session("", &quick(), NullBackend::default(), opts());
        let r = s.run();
        assert!(r.completed());
        assert_eq!(r.total, 0);
    }

    #[test]
    fn an_invalid_cadence_is_refused_up_front() {
        let bad = Cadence {
            wpm: 0.0,
            ..Default::default()
        };
        assert!(TypeSession::new("x", &bad, Box::new(NullBackend::default()), opts()).is_err());
    }

    #[test]
    fn a_failed_abort_key_is_reported_rather_than_assumed() {
        // With the abort key requested, either a keyboard could be watched or the
        // result says plainly that it could not. It is never silently absent.
        let o = SessionOptions {
            panic_key: Some("KEY_ESC".into()),
            ..opts()
        };
        let mut s = session("abc", &quick(), NullBackend::default(), o);
        let r = s.run();
        let (watchable, _) = crate::panic::available();
        assert_eq!(r.panic_active, watchable);
    }

    #[test]
    fn no_abort_key_asked_for_is_not_reported_as_live() {
        // Nothing was watching, so nothing may claim to have been. The old
        // default made "no key asked for" indistinguishable from "a key is
        // live" to every caller that wanted to tell the user how to stop.
        let mut s = session("abc", &quick(), NullBackend::default(), opts());
        assert!(!s.run().panic_active);
    }

    #[test]
    fn a_non_realtime_run_does_not_wait_out_the_schedule() {
        let cadence = Cadence {
            wpm: 20.0,
            ..Default::default()
        };
        let mut s = session(
            &"abc def ".repeat(10),
            &cadence,
            NullBackend::default(),
            opts(),
        );
        assert!(s.estimated > 10.0);
        let began = Instant::now();
        assert!(s.run().completed());
        assert!(began.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_non_realtime_run_types_exactly_the_same_text() {
        let text = "Same text, either way.\nOK?";
        let mut s = session(text, &Cadence::default(), NullBackend::default(), opts());
        s.run();
        assert_eq!(typed_text(&s), text);
    }

    #[test]
    fn a_non_realtime_run_is_still_abortable() {
        let s = session(
            &"abcdefgh ".repeat(5000),
            &Cadence::default(),
            NullBackend::default(),
            opts(),
        );
        let ctl = s.control();
        ctl.stop("never mind");
        let mut s = s;
        let r = s.run();
        assert_eq!(r.state, State::Stopped);
    }

    #[test]
    fn scheduled_shift_still_produces_the_right_text() {
        fn us(t: &str) -> Vec<String> {
            let mut c = t.chars();
            match (c.next(), c.next()) {
                (Some(ch), None) if ch.is_uppercase() => vec!["KEY_LEFTSHIFT".into()],
                _ => vec![],
            }
        }
        let backend = NullBackend::default().with_modifiers(us);
        let mut s = session(TEXT, &quick(), backend, opts());
        assert!(s.run().completed());
        assert_eq!(typed_text(&s), TEXT);
    }
}
