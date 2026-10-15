//! Drift-corrected pacing.
//!
//! Sleeping for each interval in a loop is wrong twice over. Each sleep
//! overshoots by a scheduler quantum, and the overshoots accumulate, so a
//! five-minute job lands a noticeable distance from its estimate. And a sleep
//! cannot be interrupted, so an abort during a two-second pause waits out the
//! pause.
//!
//! Pacing against absolute deadlines fixes the drift; waiting on a condition
//! variable instead of sleeping fixes the latency.

use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// A settable flag that can be waited on, which is what lets a stop interrupt
/// a long pause instead of waiting it out.
#[derive(Clone, Default)]
pub struct Flag(Arc<(Mutex<bool>, Condvar)>);

impl Flag {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }

    pub fn clear(&self) {
        *self.0 .0.lock().unwrap() = false;
        self.0 .1.notify_all();
    }

    pub fn is_set(&self) -> bool {
        *self.0 .0.lock().unwrap()
    }

    /// Block until the flag is set or `timeout` passes. True if it was set.
    pub fn wait(&self, timeout: Duration) -> bool {
        let (lock, cv) = &*self.0;
        let guard = lock.lock().unwrap();
        let (guard, _) = cv.wait_timeout_while(guard, timeout, |set| !*set).unwrap();
        *guard
    }

    /// Block until the flag is cleared, or until `until` is set. Used by a
    /// paused run, which must still be abortable.
    fn wait_cleared(&self, until: &Flag) {
        let (lock, cv) = &*self.0;
        let mut guard = lock.lock().unwrap();
        while *guard && !until.is_set() {
            guard = cv.wait_timeout(guard, Duration::from_millis(20)).unwrap().0;
        }
    }
}

/// Below this, the OS scheduler is not reliable enough and a short spin beats
/// a syscall. It costs a millisecond of one core per keystroke at most.
const SPIN_THRESHOLD: Duration = Duration::from_micros(1500);

/// Lateness beyond this is written off rather than repaid. Repaying it would
/// mean firing several keystrokes back to back to "catch up", which is both
/// visibly wrong and the fastest way to overrun an input field.
const SLIP_FORGIVENESS: f64 = 0.05;

/// Waits out an event schedule without drifting and without blocking abort.
pub struct Pacer {
    stop: Flag,
    pause: Option<Flag>,
    start: Instant,
    deadline: Instant,
    /// Total time lost to a busy machine, in seconds. Diagnostic only.
    pub slip: f64,
    /// Total time spent held by the pause flag.
    pub paused_for: f64,
}

impl Pacer {
    pub fn new(stop: Flag, pause: Option<Flag>) -> Self {
        let now = Instant::now();
        Self {
            stop,
            pause,
            start: now,
            deadline: now,
            slip: 0.0,
            paused_for: 0.0,
        }
    }

    pub fn elapsed(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// Re-anchor the schedule to now, e.g. after a countdown.
    pub fn reset(&mut self) {
        self.start = Instant::now();
        self.deadline = self.start;
    }

    /// Hold until `delay` seconds after the previous deadline. Returns false if
    /// the stop flag was raised while waiting.
    pub fn wait(&mut self, delay: f64) -> bool {
        self.deadline += Duration::from_secs_f64(delay.max(0.0));
        loop {
            if self.stop.is_set() {
                return false;
            }

            if let Some(pause) = self.pause.clone().filter(Flag::is_set) {
                let held = Instant::now();
                pause.wait_cleared(&self.stop);
                // Time spent paused is not time the schedule owes anyone.
                let gap = held.elapsed();
                self.deadline += gap;
                self.paused_for += gap.as_secs_f64();
                continue;
            }

            let now = Instant::now();
            let Some(remaining) = self.deadline.checked_duration_since(now) else {
                let late = (now - self.deadline).as_secs_f64();
                if late > SLIP_FORGIVENESS {
                    self.slip += late;
                    self.deadline = now;
                }
                return true;
            };

            if remaining > SPIN_THRESHOLD {
                if self.stop.wait(remaining - SPIN_THRESHOLD) {
                    return false;
                }
            } else {
                while Instant::now() < self.deadline {
                    std::hint::spin_loop();
                }
                return true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_does_not_accumulate_drift() {
        let mut pacer = Pacer::new(Flag::new(), None);
        for _ in 0..50 {
            assert!(pacer.wait(0.004));
        }
        let elapsed = pacer.elapsed();
        assert!((elapsed - 0.2).abs() < 0.02, "{elapsed}");
    }

    #[test]
    fn a_stop_interrupts_a_long_wait() {
        let stop = Flag::new();
        let mut pacer = Pacer::new(stop.clone(), None);
        let s = stop.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            s.set();
        });
        let began = Instant::now();
        assert!(!pacer.wait(5.0));
        assert!(began.elapsed() < Duration::from_secs(1));
        t.join().unwrap();
    }

    #[test]
    fn paused_time_is_not_owed_back() {
        let pause = Flag::new();
        let mut pacer = Pacer::new(Flag::new(), Some(pause.clone()));
        pause.set();
        let p = pause.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(120));
            p.clear();
        });
        let began = Instant::now();
        assert!(pacer.wait(0.02));
        assert!(began.elapsed() >= Duration::from_millis(120));
        assert!(pacer.paused_for >= 0.1);
        assert_eq!(pacer.slip, 0.0);
        t.join().unwrap();
    }

    #[test]
    fn a_paused_pacer_can_still_be_stopped() {
        let (stop, pause) = (Flag::new(), Flag::new());
        pause.set();
        let mut pacer = Pacer::new(stop.clone(), Some(pause));
        let s = stop.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(40));
            s.set();
        });
        assert!(!pacer.wait(0.01));
        t.join().unwrap();
    }
}
