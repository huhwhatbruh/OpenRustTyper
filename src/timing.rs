//! The cadence model: text in, a schedule of keystrokes out.
//!
//! Pure functions and plain data. Nothing here touches the keyboard, the clock
//! or the screen, which is what makes the timing testable and the ETA honest --
//! the progress bar sums the very same schedule the engine types from.
//!
//! The model is built on four observations about people at keyboards:
//!
//! * Inter-key intervals are lognormal, not normal. The distribution has a hard
//!   floor (you cannot press two keys in negative time), a dense body and a
//!   long right tail. Sampling from a Gaussian and clamping it produces a
//!   pile-up at the clamp that no hand ever makes.
//! * Consecutive keys struck by the same finger are much slower than keys that
//!   alternate hands, because the finger has to travel. This digraph effect is
//!   the single largest source of rhythm in real typing.
//! * Speed wanders slowly and reverts, rather than jumping. An
//!   Ornstein-Uhlenbeck walk gives a drifting baseline; independent draws give
//!   a twitch.
//! * Pauses cluster at syntactic boundaries -- after sentences, around
//!   paragraphs -- not uniformly across words.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

use rand::rngs::StdRng;
use rand::{Rng as _, SeedableRng};

/// Answers "which modifiers does this character need", in backend key names.
/// Only the active keyboard layout knows, and this module does not import one.
pub type ModifiersOf<'a> = &'a dyn Fn(&str) -> Vec<String>;

// ---------------------------------------------------------------------------
// Randomness
// ---------------------------------------------------------------------------

/// The handful of distributions the model draws from.
pub struct Rng(StdRng);

impl Rng {
    pub fn new(seed: Option<u64>) -> Self {
        Rng(match seed {
            Some(s) => StdRng::seed_from_u64(s),
            None => StdRng::from_entropy(),
        })
    }

    /// Uniform on `[0, 1)`.
    fn random(&mut self) -> f64 {
        self.0.gen::<f64>()
    }

    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.random()
    }

    /// Normal with the given mean and standard deviation (Box-Muller).
    fn gauss(&mut self, mu: f64, sigma: f64) -> f64 {
        let u1 = 1.0 - self.random(); // (0, 1], so the log is finite
        let u2 = self.random();
        mu + sigma * (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    fn pick(&mut self, items: &[char]) -> char {
        items[self.0.gen_range(0..items.len())]
    }
}

// ---------------------------------------------------------------------------
// Physical keyboard geometry
// ---------------------------------------------------------------------------
// Rows of QWERTY as struck, with the finger that strikes each key. Finger ids
// run 0-3 for the left hand (pinky->index) and 4-7 for the right (index->pinky).
// Geometry is only ever used to *shape rhythm*, so an approximate US layout is
// good enough even when the active layout differs; a wrong guess costs realism,
// never correctness.
#[rustfmt::skip]
const ROWS: [(&str, [u8; 13]); 4] = [
    ("`1234567890-=", [0, 0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 7, 7]),
    ("qwertyuiop[]\\", [0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 7, 7, 7]),
    ("asdfghjkl;'", [0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 7, 0, 0]),
    ("zxcvbnm,./", [0, 1, 2, 3, 3, 4, 4, 5, 6, 7, 0, 0, 0]),
];

// Characters reached by holding Shift, aligned with the unshifted rows above.
const SHIFTED: [&str; 4] = [
    "~!@#$%^&*()_+",
    "QWERTYUIOP{}|",
    "ASDFGHJKL:\"",
    "ZXCVBNM<>?",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Geo {
    finger: u8,
    row: i32,
    col: i32,
    shift: bool,
}

fn geometry() -> &'static HashMap<char, Geo> {
    static GEO: OnceLock<HashMap<char, Geo>> = OnceLock::new();
    GEO.get_or_init(|| {
        let mut geo = HashMap::new();
        for (row, ((keys, fingers), shifted)) in ROWS.iter().zip(SHIFTED).enumerate() {
            let shifted: Vec<char> = shifted.chars().collect();
            for (col, ch) in keys.chars().enumerate() {
                let finger = fingers[col];
                let (row, col_i) = (row as i32, col as i32);
                geo.insert(
                    ch,
                    Geo {
                        finger,
                        row,
                        col: col_i,
                        shift: false,
                    },
                );
                if let Some(&sh) = shifted.get(col) {
                    geo.insert(
                        sh,
                        Geo {
                            finger,
                            row,
                            col: col_i,
                            shift: true,
                        },
                    );
                }
            }
        }
        // Thumb, its own row.
        geo.insert(
            ' ',
            Geo {
                finger: 8,
                row: 4,
                col: 5,
                shift: false,
            },
        );
        geo
    })
}

/// Neighbours on the physical board, used to make typos land where a hand would
/// actually slip rather than on a random letter.
fn neighbours() -> &'static HashMap<char, Vec<char>> {
    static NEAR: OnceLock<HashMap<char, Vec<char>>> = OnceLock::new();
    NEAR.get_or_init(|| {
        let coords: Vec<(char, i32, i32)> = geometry()
            .iter()
            .filter(|(ch, g)| !g.shift && **ch != ' ')
            .map(|(ch, g)| (*ch, g.row, g.col))
            .collect();
        let mut near = HashMap::new();
        for &(ch, row, col) in &coords {
            let mut hits: Vec<char> = coords
                .iter()
                .filter(|&&(other, orow, ocol)| {
                    other != ch && (orow - row).abs() <= 1 && (ocol - col).abs() <= 1
                })
                .map(|&(other, _, _)| other)
                .collect();
            hits.sort_unstable();
            if !hits.is_empty() {
                near.insert(ch, hits);
            }
        }
        near
    })
}

/// High-frequency words are executed as single motor chunks rather than letter
/// by letter, so they come out faster than their length predicts.
fn chunked_words() -> &'static HashSet<&'static str> {
    static WORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| {
        "a i be to of in it is as at he we so on or an my by do go up no if me us
         the and for are but not you all can had her was one our out has his how its
         may new now old see way who did get let say she too use own say she try ask
         that this with have from they will been more when what your than them then
         into some time very just like know take come make able only over such most
         also back even want give year work well good need find last long much many
         where which while there their these those been being does done each else"
            .split_whitespace()
            .collect()
    })
}

fn is_sentence_end(c: char) -> bool {
    matches!(c, '.' | '!' | '?')
}

fn is_clause_end(c: char) -> bool {
    matches!(c, ',' | ';' | ':')
}

// ---------------------------------------------------------------------------
// Trend across a document, and across runs
// ---------------------------------------------------------------------------
// The Ornstein-Uhlenbeck walk reverts to a fixed mean, which makes the pace
// wander but never *go* anywhere. A person does not type a long document at a
// flat rate. They start below their own pace and find it over the first
// paragraph or two, and they give some of it back by the end. Both are slow
// enough to be invisible in any one interval and obvious in the trend of
// several thousand.

/// How much slower than the settled pace the very first keystrokes are.
const WARMUP_EXCESS: f64 = 0.10;
/// Characters over which the warm-up decays; roughly a paragraph.
const WARMUP_SCALE: f64 = 180.0;
/// How much slower the pace has become by the time fatigue is fully felt.
const FATIGUE_EXCESS: f64 = 0.06;
/// Characters before fatigue has arrived in full -- a few pages.
const FATIGUE_ONSET: f64 = 1500.0;

/// Spread of the per-run pace, as the sigma of a lognormal on total duration.
/// Small enough that the words-per-minute dial still means what it says, large
/// enough that no two runs land on the same number. Normalising to the exact
/// requested rate would make the total a function of the text and the dial
/// alone, identical to the microsecond on every run.
const RUN_TO_RUN: f64 = 0.035;

/// What a scheduled event does when it fires.
///
/// Presses and releases are scheduled independently, which is the whole point:
/// a key's release may fall after the *next* key's press, and on a real
/// keyboard it frequently does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Action {
    /// Release a key. Sorts first when a release and a press coincide.
    Up = 0,
    /// Press a key.
    Down = 1,
}

/// One scheduled key transition.
///
/// `delay` is the wait before the event fires. `index` is the position (in
/// characters) in the source text that this press completes; it is `None` for
/// releases and for the keystrokes belonging to a typo and its correction, so
/// progress never runs backwards.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub delay: f64,
    pub action: Action,
    pub value: String,
    pub index: Option<usize>,
}

/// Tunable shape of the typing rhythm.
///
/// `wpm` is the only setting most people need. Everything else moves the
/// character of the rhythm without changing its average speed, because the
/// finished schedule is normalized to the rate `wpm` asks for.
#[derive(Debug, Clone, PartialEq)]
pub struct Cadence {
    pub wpm: f64,
    /// Spread of the lognormal interval between presses. 0 is a metronome.
    pub variance: f64,
    /// How strongly finger travel stretches an interval. 0 disables geometry.
    pub rhythm: f64,
    /// Fraction of keystrokes preceded by a visible hitch.
    pub hesitation: f64,
    /// Probability of pausing at a sentence boundary, 0-1.
    pub sentence_pause: f64,
    /// Longest boundary pause, in seconds.
    pub max_pause: f64,
    /// Probability per character of striking a neighbouring key by mistake.
    /// The engine always corrects it; 0 types verbatim.
    pub typo_rate: f64,
    /// Strength of the slow speed wander. 0 holds a constant baseline.
    pub drift: f64,
    /// Mean key-hold time in seconds. 0 derives it from `wpm`.
    pub dwell: f64,
    /// Spread of the lognormal hold time.
    pub dwell_variance: f64,
    /// How much a key may still be held when the next one is pressed, 0-1.
    /// 0 releases every key before the next press; 1 lets holds overlap as
    /// freely as the hands allow.
    pub rollover: f64,
}

impl Default for Cadence {
    fn default() -> Self {
        Self {
            wpm: 70.0,
            variance: 0.30,
            rhythm: 1.0,
            hesitation: 0.03,
            sentence_pause: 0.55,
            max_pause: 1.8,
            typo_rate: 0.0,
            drift: 1.0,
            dwell: 0.0,
            dwell_variance: 0.26,
            rollover: 1.0,
        }
    }
}

impl Cadence {
    /// Seconds per character implied by `wpm` (a word is five keys).
    pub fn mean_interval(&self) -> f64 {
        60.0 / self.wpm.max(1.0) / 5.0
    }

    /// Seconds a key is held, on average.
    pub fn mean_dwell(&self) -> f64 {
        if self.dwell > 0.0 {
            return self.dwell;
        }
        let ms = DWELL_INTERCEPT_MS - DWELL_SLOPE_MS * self.wpm;
        (ms / 1000.0).clamp(DWELL_FLOOR, DWELL_CEILING)
    }

    pub fn validate(&self) -> Result<(), String> {
        let check = |ok: bool, msg: &str| if ok { Ok(()) } else { Err(msg.to_string()) };
        check(
            (1.0..=2000.0).contains(&self.wpm),
            "wpm must be between 1 and 2000",
        )?;
        check(
            (0.0..=2.0).contains(&self.variance),
            "variance must be between 0 and 2",
        )?;
        check(
            (0.0..=1.0).contains(&self.rhythm),
            "rhythm must be between 0 and 1",
        )?;
        check(
            (0.0..=1.0).contains(&self.hesitation),
            "hesitation must be between 0 and 1",
        )?;
        check(
            (0.0..=1.0).contains(&self.drift),
            "drift must be between 0 and 1",
        )?;
        check(
            (0.0..=0.25).contains(&self.typo_rate),
            "typo_rate must be between 0 and 0.25",
        )?;
        check(
            (0.0..=1.0).contains(&self.sentence_pause),
            "sentence_pause must be between 0 and 1",
        )?;
        check(self.max_pause >= 0.0, "max_pause cannot be negative")?;
        check(
            (0.0..=1.0).contains(&self.dwell),
            "dwell must be between 0 and 1 second",
        )?;
        check(
            (0.0..=2.0).contains(&self.dwell_variance),
            "dwell_variance must be between 0 and 2",
        )?;
        check(
            (0.0..=1.0).contains(&self.rollover),
            "rollover must be between 0 and 1",
        )?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// How long a key is held
// ---------------------------------------------------------------------------
// Hold time is the half of keystroke timing that auto-typers forget. Every
// keydown a page receives is paired with a keyup, and the interval between them
// is as characteristic as the interval between keys.
//
// The mean falls as typing speed rises: quick typists do not merely travel
// between keys sooner, they lift each finger sooner. These coefficients trace
// the trend reported for the Aalto 136M-keystroke corpus -- roughly 110 ms at
// 30 wpm, 95 ms at 70, 70 ms at 120 -- and the floor and ceiling keep
// extrapolation away from physically silly values.
const DWELL_INTERCEPT_MS: f64 = 125.0;
const DWELL_SLOPE_MS: f64 = 0.45;
const DWELL_FLOOR: f64 = 0.045;
const DWELL_CEILING: f64 = 0.135;

/// Hold time by finger. The thumb rests on the space bar and lingers; the
/// pinkies are slow to lift; the index fingers are quickest.
fn finger_dwell(finger: u8) -> f64 {
    match finger {
        0 | 7 => 1.10,
        1 | 6 => 1.05,
        2 | 5 => 1.00,
        3 | 4 => 0.96,
        8 => 1.14,
        _ => 1.0,
    }
}

/// The shortest hold that still registers reliably as a keypress.
const MIN_DWELL: f64 = 0.004;

/// The longest a key may be held. Two reasons, and they agree on the number.
/// Prose typing simply does not hold a key a fifth of a second. And the virtual
/// keyboard declares autorepeat, as a keyboard must; past the kernel's 250 ms
/// repeat delay the key would start duplicating itself, turning a long hold
/// into a character the text never contained.
pub const MAX_DWELL: f64 = 0.200;

fn geometry_of(value: &str) -> Option<Geo> {
    let mut chars = value.chars();
    let ch = chars.next()?;
    if chars.next().is_some() {
        return None; // a named key: enter, tab, backspace
    }
    let key = if ch.is_alphabetic() {
        ch.to_lowercase().next().unwrap_or(ch)
    } else {
        ch
    };
    geometry().get(&key).copied()
}

fn dwell_factor(value: &str) -> f64 {
    match geometry_of(value) {
        None => 1.0,
        Some(g) => {
            let factor = finger_dwell(g.finger);
            // A finger reaching with a modifier held lifts later.
            if g.shift {
                factor * 1.06
            } else {
                factor
            }
        }
    }
}

/// Which key cap a token uses. 'a' and 'A' are the same one.
#[derive(Debug, PartialEq, Eq)]
enum PhysicalKey<'a> {
    Named(&'a str),
    Cap(i32, i32),
}

fn physical_key(value: &str) -> PhysicalKey<'_> {
    match geometry_of(value) {
        None => PhysicalKey::Named(value),
        Some(g) => PhysicalKey::Cap(g.row, g.col),
    }
}

fn finger_of(value: &str) -> Option<u8> {
    geometry_of(value).map(|g| g.finger)
}

/// A key to strike, and how long to wait before striking it.
#[derive(Debug, Clone)]
struct Press {
    interval: f64,
    value: String,
    index: Option<usize>,
}

/// How much finger travel stretches the interval before `ch`.
fn digraph_factor(prev: Option<char>, ch: char, strength: f64) -> f64 {
    if strength <= 0.0 {
        return 1.0;
    }
    let Some(prev) = prev else { return 1.0 };
    let (Some(a), Some(b)) = (geometry_of(&prev.to_string()), geometry_of(&ch.to_string())) else {
        return 1.0;
    };

    let factor = if a.finger == b.finger {
        // Same finger: either a fast repeat of one key, or a real reach.
        if (a.row, a.col) == (b.row, b.col) {
            1.12
        } else {
            1.70 + 0.14 * f64::from((a.row - b.row).abs())
        }
    } else if (a.finger < 4) != (b.finger < 4) {
        0.86 // hands alternate, the fastest digraph there is
    } else {
        1.0 + 0.10 * f64::from((a.row - b.row).abs()) + 0.02 * f64::from((a.col - b.col).abs())
    };

    1.0 + (factor - 1.0) * strength
}

/// Pause earned by the syntax just typed, before position `i`.
fn boundary_pause(text: &[char], i: usize, cad: &Cadence, rng: &mut Rng) -> f64 {
    if i == 0 || cad.max_pause <= 0.0 {
        return 0.0;
    }
    let prev = text[i - 1];

    // Paragraph breaks are the longest pause in ordinary writing.
    if prev == '\n' {
        if rng.random() >= cad.sentence_pause {
            return 0.0;
        }
        if i >= 2 && text[i - 2] == '\n' {
            return rng.uniform(0.35, 1.0) * cad.max_pause;
        }
        return rng.uniform(0.10, 0.35) * cad.max_pause;
    }

    if text[i] != ' ' && prev == ' ' && i >= 2 {
        let anchor = text[i - 2];
        if is_sentence_end(anchor) {
            if rng.random() < cad.sentence_pause {
                return rng.uniform(0.25, 0.85) * cad.max_pause;
            }
        } else if is_clause_end(anchor) && rng.random() < cad.sentence_pause * 0.6 {
            return rng.uniform(0.08, 0.30) * cad.max_pause;
        }
    }
    0.0
}

/// The whitespace-delimited word starting at `i`, and where it ends.
fn word_at(text: &[char], i: usize) -> (String, usize) {
    let mut j = i;
    while j < text.len() && !text[j].is_whitespace() {
        j += 1;
    }
    (text[i..j].iter().collect(), j)
}

/// Decide which keys to strike, and how long to wait before each.
fn plan_presses(text: &[char], cadence: &Cadence, rng: &mut Rng) -> Vec<Press> {
    let mean = cadence.mean_interval();
    let mut presses: Vec<Press> = Vec::with_capacity(text.len() + text.len() / 8);

    // Log-space speed offset, wandering by Ornstein-Uhlenbeck so the baseline
    // reverts to the requested WPM instead of walking away from it.
    let mut z = 0.0_f64;
    let (theta, vol) = (0.02, 0.035 * cadence.drift);

    // Sigma of the lognormal is chosen so the *mean* stays at `mean`: for
    // lognormal, E[X] = exp(mu + sigma^2/2), hence mu = -sigma^2/2.
    let sigma = cadence.variance;
    let mu_correction = -0.5 * sigma * sigma;

    let mut chunk_factor = 1.0;
    let mut chunk_until = 0usize;
    let mut chunk_started = false;

    // Both trends are scaled by `drift`, which is the dial for "how much does
    // the pace wander"; at zero the model is a metronome and stays one. And
    // both are shape, not speed: `calibrate` normalises the mean away again.
    let warmup = WARMUP_EXCESS * cadence.drift;
    let fatigue = FATIGUE_EXCESS * cadence.drift;

    for (i, &ch) in text.iter().enumerate() {
        if (!chunk_started || i >= chunk_until) && !ch.is_whitespace() {
            let (word, end) = word_at(text, i);
            let stripped = word
                .trim_matches(|c| ".,;:!?\"'()[]{}-".contains(c))
                .to_lowercase();
            let chunked = chunked_words().contains(stripped.as_str()) && word.chars().count() <= 6;
            chunk_factor = if chunked {
                1.0 - 0.18 * cadence.rhythm
            } else {
                1.0
            };
            chunk_until = end;
            chunk_started = true;
        }

        z += -theta * z + vol * rng.gauss(0.0, 1.0);
        let prev = if i > 0 { Some(text[i - 1]) } else { None };

        let trend = 1.0
            + warmup * (-(i as f64) / WARMUP_SCALE).exp()
            + fatigue * (i as f64 / FATIGUE_ONSET).min(1.0);

        let mut interval = mean
            * (z + rng.gauss(mu_correction, sigma)).exp()
            * digraph_factor(prev, ch, cadence.rhythm)
            * chunk_factor
            * trend;

        if rng.random() < cadence.hesitation {
            interval += mean * rng.uniform(2.0, 6.0);
        }

        interval += boundary_pause(text, i, cadence, rng);
        interval = interval.max(0.0005);

        // A slip: strike a neighbouring key, notice, backspace, carry on.
        if cadence.typo_rate > 0.0 && ch.is_alphabetic() && rng.random() < cadence.typo_rate {
            let lower = ch.to_lowercase().next().unwrap_or(ch);
            if let Some(pool) = neighbours().get(&lower) {
                let mut wrong = rng.pick(pool);
                if ch.is_uppercase() {
                    wrong = wrong.to_ascii_uppercase();
                }
                presses.push(Press {
                    interval,
                    value: wrong.to_string(),
                    index: None,
                });
                // The hand runs on for a beat before the error registers.
                presses.push(Press {
                    interval: mean * rng.uniform(1.5, 5.0),
                    value: "backspace".into(),
                    index: None,
                });
                interval = mean * rng.uniform(0.8, 1.6);
            }
        }

        let value = match ch {
            '\n' => "enter".to_string(),
            '\t' => "tab".to_string(),
            c => c.to_string(),
        };
        presses.push(Press {
            interval,
            value,
            index: Some(i),
        });
    }
    presses
}

/// Scale the schedule so the requested words-per-minute is what happens.
///
/// Every multiplier in the model shapes the rhythm; none of them should move
/// the average, or the WPM dial would be a suggestion rather than a setting.
fn calibrate(presses: Vec<Press>, chars: usize, cadence: &Cadence, rng: &mut Rng) -> Vec<Press> {
    let total: f64 = presses.iter().map(|p| p.interval).sum();
    if total <= 0.0 {
        return presses;
    }
    // The target spans press to press: producing the text is what is being
    // timed, not the release of its final key.
    let mut target = chars as f64 / 5.0 * 60.0 / cadence.wpm;
    if cadence.drift > 0.0 {
        target *= rng.gauss(0.0, RUN_TO_RUN * cadence.drift).exp();
    }
    let scale = target / total;
    presses
        .into_iter()
        .map(|p| Press {
            interval: (p.interval * scale).max(0.0005),
            ..p
        })
        .collect()
}

/// When each key is let go -- the part that makes rollover possible.
fn release_times(
    presses: &[Press],
    press_at: &[f64],
    cadence: &Cadence,
    rng: &mut Rng,
) -> Vec<f64> {
    let mean = cadence.mean_dwell();
    let sigma = cadence.dwell_variance;
    let mu_correction = -0.5 * sigma * sigma;
    let n = presses.len();
    let mut release_at = vec![0.0; n];

    for (i, press) in presses.iter().enumerate() {
        let hold = mean * dwell_factor(&press.value) * rng.gauss(mu_correction, sigma).exp();
        let mut release = press_at[i] + hold.clamp(MIN_DWELL, MAX_DWELL);

        if i + 1 < n {
            let nxt = &presses[i + 1];
            let span = press_at[i + 1] - press_at[i];
            let gap = (0.010_f64).min(span * 0.3).max(0.0005);

            // Two constraints are physical, not stylistic. One finger cannot
            // be on two keys, and one key cannot be pressed while already down
            // -- and a second press of a held key would strand it down for
            // good.
            let same_cap = physical_key(&press.value) == physical_key(&nxt.value);
            let finger = finger_of(&press.value);
            let same_finger = finger.is_some() && finger == finger_of(&nxt.value);

            if same_cap || same_finger {
                release = release.min(press_at[i + 1] - gap);
            } else {
                let overlap = release - press_at[i + 1];
                if overlap > 0.0 {
                    release = press_at[i + 1] + overlap * cadence.rollover
                        - (1.0 - cadence.rollover) * gap;
                }
                // Holding a key across two further presses is not rollover, it
                // is a stuck key.
                if i + 2 < n {
                    release = release.min(press_at[i + 2] - gap);
                }
            }

            // Last word for a key that has to be up before the next one: the
            // release must fall strictly between its own press and that press.
            // At extreme speed a lognormal tail can compress an interval below
            // the minimum dwell, and letting the dwell win there puts a release
            // at or before its own press -- an up with no down, followed by a
            // key left held -- or after the next press of the same key, which
            // swallows that character. Ordering and "one finger, one key"
            // outrank hold time. Other pairs are allowed to roll over.
            let half = span * 0.5;
            let earliest = press_at[i] + MIN_DWELL.min(half);
            release = if same_cap || same_finger {
                let latest = press_at[i + 1] - gap.min(half);
                release.max(earliest).min(latest)
            } else {
                release.max(earliest)
            };
        } else {
            release = release.max(press_at[i] + MIN_DWELL);
        }

        release_at[i] = release;
    }
    release_at
}

// ---------------------------------------------------------------------------
// Modifiers are keystrokes too
// ---------------------------------------------------------------------------
// A capital letter is two key presses, and the first one is the tell. Pressed
// in the same breath as the letter it modifies, Shift arrives with a lead of
// 0.013-0.04 ms. A hand cannot do that. Shift is a separate finger starting a
// separate movement, and it arrives well ahead -- 50 to 150 ms is the ordinary
// range -- then lingers past the letter's own release.
//
// So modifiers are scheduled here, alongside the characters, instead of being
// an implementation detail of the device. Which modifier a character needs
// depends on the keyboard layout, which this module deliberately knows nothing
// about, so the caller passes a function that answers it.

/// Mean seconds a modifier goes down before the key it modifies.
const MODIFIER_LEAD: f64 = 0.075;
const MODIFIER_LEAD_SIGMA: f64 = 0.42;
/// Mean seconds a modifier stays down after the last key that needed it.
const MODIFIER_LAG: f64 = 0.048;
const MODIFIER_LAG_SIGMA: f64 = 0.40;
/// The smallest ordering gap worth preserving between two transitions.
const EPSILON: f64 = 0.002;

/// Maximal runs of consecutive presses that all want one modifier.
///
/// A run of capitals is one continuous Shift, not a press per letter, so the
/// grouping has to happen before any timing is decided.
fn modifier_runs(presses: &[Press], modifiers_of: ModifiersOf) -> Vec<(String, usize, usize)> {
    let wanted: Vec<HashSet<String>> = presses
        .iter()
        .map(|p| modifiers_of(&p.value).into_iter().collect())
        .collect();
    let names: BTreeSet<&String> = wanted.iter().flatten().collect();

    let mut runs = Vec::new();
    for name in names {
        let mut start: Option<usize> = None;
        for (i, group) in wanted.iter().enumerate() {
            if group.contains(name) {
                start.get_or_insert(i);
            } else if let Some(s) = start.take() {
                runs.push((name.clone(), s, i - 1));
            }
        }
        if let Some(s) = start {
            runs.push((name.clone(), s, presses.len() - 1));
        }
    }
    runs
}

type Timeline = Vec<(f64, Action, String, Option<usize>)>;

/// Place each modifier's own press and release on the timeline.
///
/// Every clamp here is correctness rather than realism. A modifier that lands
/// after the key it modifies produces the unmodified character; one that
/// outlives its run modifies a character that did not ask for it, which is the
/// bug that turns "Second" into "SEcond".
fn schedule_modifiers(
    presses: &[Press],
    press_at: &[f64],
    release_at: &[f64],
    modifiers_of: ModifiersOf,
    rng: &mut Rng,
) -> Timeline {
    let mut events = Vec::new();
    for (name, first, last) in modifier_runs(presses, modifiers_of) {
        // Reaching for Shift begins after the previous keystroke has been
        // pressed, not before it: a modifier that goes down early changes the
        // character before its own. It may well overlap that key's release.
        let earliest_down = if first > 0 {
            press_at[first - 1] + EPSILON
        } else {
            0.0
        };
        let latest_down = press_at[first] - EPSILON / 2.0;
        if latest_down <= earliest_down {
            // The two keys are closer together than the modifier needs to
            // travel. Placing it anyway puts it down over the character before,
            // which produces different text -- so the run is left unscheduled
            // and the backend presses the modifier with its own key.
            continue;
        }

        let lead = MODIFIER_LEAD * rng.gauss(0.0, MODIFIER_LEAD_SIGMA).exp();
        let down = (press_at[first] - lead).max(earliest_down).min(latest_down);

        let lag = MODIFIER_LAG * rng.gauss(0.0, MODIFIER_LAG_SIGMA).exp();
        let mut up = release_at[last] + lag;
        // The character is fixed once its key is down, so the modifier is free
        // to lift from that instant -- but never before it, and never before
        // its own press.
        let earliest_up = (press_at[last] + EPSILON).max(down + EPSILON);
        up = up.max(earliest_up);
        // And it must be gone before any key that does not want it.
        if last + 1 < presses.len() {
            let span = press_at[last + 1] - press_at[last];
            let latest_up = press_at[last + 1] - EPSILON.min(span * 0.5);
            if latest_up <= earliest_up {
                // No room to hold the modifier for any length of time at all.
                // Dropping the run is still correct: the backend presses the
                // modifier together with the key when a scheduled one is not
                // there. Keeping it would either hold Shift over a character
                // that never asked for it -- wrong text -- or release it before
                // it was pressed.
                continue;
            }
            up = up.min(latest_up);
        }

        events.push((down, Action::Down, name.clone(), None));
        events.push((up, Action::Up, name, None));
    }
    events
}

/// Compile `text` into the exact stream of key transitions to perform.
///
/// The returned list is both what the engine types and what the ETA is
/// measured from, so the estimate cannot drift away from reality. Passing a
/// `seed` makes the schedule reproducible, which is what the tests rely on.
///
/// `modifiers_of` answers which modifiers a character needs -- a question only
/// the keyboard layout can settle, which is why it arrives as a function rather
/// than a table. Given one, Shift and AltGr become scheduled keystrokes with a
/// lead and a lag of their own. Without one they are left to the backend, which
/// presses them at the same instant as the key.
pub fn plan(
    text: &str,
    cadence: &Cadence,
    seed: Option<u64>,
    modifiers_of: Option<ModifiersOf>,
) -> Result<Vec<Event>, String> {
    cadence.validate()?;
    let mut rng = Rng::new(seed);
    let chars: Vec<char> = text.chars().collect();

    let presses = plan_presses(&chars, cadence, &mut rng);
    if presses.is_empty() {
        return Ok(Vec::new());
    }
    let presses = calibrate(presses, chars.len(), cadence, &mut rng);

    let mut clock = 0.0;
    let press_at: Vec<f64> = presses
        .iter()
        .map(|p| {
            clock += p.interval;
            clock
        })
        .collect();
    let release_at = release_times(&presses, &press_at, cadence, &mut rng);

    let mut timeline: Timeline = Vec::with_capacity(presses.len() * 2);
    for (i, press) in presses.iter().enumerate() {
        timeline.push((press_at[i], Action::Down, press.value.clone(), press.index));
        timeline.push((release_at[i], Action::Up, press.value.clone(), None));
    }
    if let Some(modifiers_of) = modifiers_of {
        timeline.extend(schedule_modifiers(
            &presses,
            &press_at,
            &release_at,
            modifiers_of,
            &mut rng,
        ));
    }
    // Up sorts before Down, so a release scheduled for the same instant as a
    // press happens first -- never a phantom double-press. The sort is stable.
    timeline.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut previous = 0.0;
    Ok(timeline
        .into_iter()
        .map(|(moment, action, value, index)| {
            let event = Event {
                delay: (moment - previous).max(0.0),
                action,
                value,
                index,
            };
            previous = moment;
            event
        })
        .collect())
}

/// Total seconds the schedule takes, excluding any start countdown.
///
/// This runs to the final key *release*, so it is a hair longer than
/// [`typing_duration`] -- the difference is one hold time.
pub fn duration(events: &[Event]) -> f64 {
    events.iter().map(|e| e.delay).sum()
}

/// Seconds from the start to the last key *press*.
///
/// This is the span words-per-minute is defined over: text is produced when
/// keys go down, and the trailing release of the last character is not part of
/// writing it.
pub fn typing_duration(events: &[Event]) -> f64 {
    let mut clock = 0.0;
    let mut last_press = 0.0;
    for event in events {
        clock += event.delay;
        if event.action == Action::Down {
            last_press = clock;
        }
    }
    last_press
}

/// The timings a keystroke listener on the far end would measure.
#[derive(Debug, Clone, Default)]
pub struct StrokeStats {
    /// Press to its own release.
    pub dwell: Vec<f64>,
    /// Release to the next press; negative means rollover.
    pub flight: Vec<f64>,
    /// Press to the next press.
    pub interval: Vec<f64>,
}

impl StrokeStats {
    /// Share of transitions where one key is still down as the next goes.
    pub fn rollover_rate(&self) -> f64 {
        if self.flight.is_empty() {
            return 0.0;
        }
        self.flight.iter().filter(|f| **f < 0.0).count() as f64 / self.flight.len() as f64
    }
}

/// Measure a schedule the way the receiving end would.
pub fn stroke_stats(events: &[Event]) -> StrokeStats {
    let mut clock = 0.0;
    let mut down_at: HashMap<&str, f64> = HashMap::new();
    let mut order: Vec<(f64, f64)> = Vec::new(); // (press, release)
    for event in events {
        clock += event.delay;
        match event.action {
            Action::Down => {
                down_at.insert(&event.value, clock);
            }
            Action::Up => {
                if let Some(pressed) = down_at.remove(event.value.as_str()) {
                    order.push((pressed, clock));
                }
            }
        }
    }
    order.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));

    StrokeStats {
        dwell: order.iter().map(|(p, r)| r - p).collect(),
        interval: order.windows(2).map(|w| w[1].0 - w[0].0).collect(),
        flight: order.windows(2).map(|w| w[1].0 - w[0].1).collect(),
    }
}

/// Render a duration the way a status bar should show it.
pub fn format_duration(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    if seconds < 1.0 {
        return "<1s".into();
    }
    if seconds < 60.0 {
        return format!("{seconds:.0}s");
    }
    let whole = seconds as u64;
    let (minutes, secs) = (whole / 60, whole % 60);
    if minutes < 60 {
        return format!("{minutes}m {secs:02}s");
    }
    format!("{}h {:02}m", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    //! A one-to-one port of the Python suite (`tests/test_timing.py`). The
    //! statistical tests are the point: they are what says the Rust model
    //! behaves like the Python one, not merely that it compiles.
    use super::*;

    const SAMPLE: &str =
        "The quick brown fox jumps over the lazy dog. It was a bright cold day in \
        April, and the clocks were striking thirteen.\n\nA second paragraph, with \
        a comma; and a semicolon.";

    fn mean(xs: &[f64]) -> f64 {
        xs.iter().sum::<f64>() / xs.len() as f64
    }

    fn median(xs: &[f64]) -> f64 {
        let mut v = xs.to_vec();
        v.sort_by(f64::total_cmp);
        let n = v.len();
        if n % 2 == 1 {
            v[n / 2]
        } else {
            (v[n / 2 - 1] + v[n / 2]) / 2.0
        }
    }

    /// Sample standard deviation, as `statistics.stdev`.
    fn stdev(xs: &[f64]) -> f64 {
        let m = mean(xs);
        (xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() as f64 - 1.0)).sqrt()
    }

    fn approx(got: f64, want: f64, rel: f64) -> bool {
        (got - want).abs() <= want.abs() * rel
    }

    fn plan_seeded(text: &str, cadence: &Cadence, seed: u64) -> Vec<Event> {
        plan(text, cadence, Some(seed), None).unwrap()
    }

    fn cad(wpm: f64) -> Cadence {
        Cadence {
            wpm,
            ..Default::default()
        }
    }

    fn flat(wpm: f64) -> Cadence {
        Cadence {
            wpm,
            variance: 0.0,
            rhythm: 0.0,
            hesitation: 0.0,
            sentence_pause: 0.0,
            drift: 0.0,
            dwell_variance: 0.0,
            ..Default::default()
        }
    }

    fn presses(events: &[Event]) -> Vec<&Event> {
        events.iter().filter(|e| e.action == Action::Down).collect()
    }

    // -- Speed ---------------------------------------------------------------
    fn delivered(text: &str, cadence: &Cadence, seed: u64) -> f64 {
        let span = typing_duration(&plan_seeded(text, cadence, seed));
        text.chars().count() as f64 / 5.0 / (span / 60.0)
    }

    #[test]
    fn wpm_is_delivered_to_within_a_few_percent() {
        for wpm in [15.0, 40.0, 70.0, 120.0, 250.0, 600.0] {
            for seed in 0..12 {
                let got = delivered(SAMPLE, &cad(wpm), seed);
                assert!(approx(got, wpm, 0.12), "{wpm} wpm seed {seed}: {got}");
            }
        }
    }

    #[test]
    fn the_requested_rate_is_what_runs_average_to() {
        for wpm in [40.0, 70.0, 120.0] {
            let rates: Vec<f64> = (0..60).map(|s| delivered(SAMPLE, &cad(wpm), s)).collect();
            assert!(approx(mean(&rates), wpm, 0.02), "{wpm}: {}", mean(&rates));
        }
    }

    #[test]
    fn two_runs_of_one_text_do_not_take_the_same_time() {
        let spans: Vec<f64> = (0..20)
            .map(|s| typing_duration(&plan_seeded(SAMPLE, &cad(70.0), s)))
            .collect();
        let distinct: std::collections::HashSet<i64> =
            spans.iter().map(|s| (s * 1e6).round() as i64).collect();
        assert_eq!(distinct.len(), 20, "runs are sharing a duration");
        let lo = spans.iter().cloned().fold(f64::MAX, f64::min);
        let hi = spans.iter().cloned().fold(f64::MIN, f64::max);
        let spread = (hi - lo) / mean(&spans);
        assert!(0.02 < spread && spread < 0.40, "{spread}");
    }

    #[test]
    fn a_metronome_stays_exact() {
        for seed in [1, 2, 3] {
            let got = delivered(SAMPLE, &flat(70.0), seed);
            assert!(approx(got, 70.0, 1e-6), "{got}");
        }
    }

    #[test]
    fn shape_knobs_do_not_move_the_average() {
        let cadences = [
            Cadence {
                variance: 0.6,
                hesitation: 0.12,
                ..cad(70.0)
            },
            Cadence {
                rhythm: 0.0,
                ..cad(70.0)
            },
            Cadence {
                max_pause: 6.0,
                sentence_pause: 1.0,
                ..cad(70.0)
            },
            Cadence {
                typo_rate: 0.1,
                ..cad(70.0)
            },
            Cadence {
                drift: 0.0,
                ..cad(70.0)
            },
            Cadence {
                dwell: 0.2,
                rollover: 0.0,
                ..cad(70.0)
            },
            Cadence {
                dwell_variance: 1.0,
                ..cad(70.0)
            },
        ];
        for c in cadences {
            let rates: Vec<f64> = (0..40).map(|s| delivered(SAMPLE, &c, s)).collect();
            assert!(approx(mean(&rates), 70.0, 0.02), "{c:?}: {}", mean(&rates));
        }
    }

    #[test]
    fn the_full_stream_outlasts_typing_by_one_hold() {
        let events = plan_seeded(SAMPLE, &cad(70.0), 1);
        let extra = duration(&events) - typing_duration(&events);
        assert!(0.0 < extra && extra < 0.3, "{extra}");
    }

    // -- Hold time -----------------------------------------------------------
    #[test]
    fn hold_time_matches_human_measurements() {
        for (wpm, low, high) in [
            (30.0, 0.100, 0.125),
            (70.0, 0.085, 0.105),
            (120.0, 0.060, 0.082),
            (200.0, 0.040, 0.060),
        ] {
            let text = SAMPLE.repeat(3);
            let stats = stroke_stats(&plan_seeded(&text, &cad(wpm), 2));
            let m = mean(&stats.dwell);
            assert!(low <= m && m <= high, "{wpm} wpm: {m}");
        }
    }

    #[test]
    fn hold_time_is_right_skewed_not_uniform() {
        let stats = stroke_stats(&plan_seeded(&SAMPLE.repeat(3), &cad(75.0), 4));
        let m = mean(&stats.dwell);
        assert!(m > median(&stats.dwell));
        assert!(stdev(&stats.dwell) / m > 0.15);
    }

    #[test]
    fn hold_time_never_collapses_to_nothing() {
        for wpm in [600.0, 1200.0, 2000.0] {
            let stats = stroke_stats(&plan_seeded(SAMPLE, &cad(wpm), 5));
            assert!(stats.dwell.iter().cloned().fold(f64::MAX, f64::min) > 0.0);
        }
    }

    #[test]
    fn explicit_dwell_overrides_the_speed_derived_default() {
        let stats = stroke_stats(&plan_seeded(
            SAMPLE,
            &Cadence {
                dwell: 0.2,
                ..cad(70.0)
            },
            6,
        ));
        let m = mean(&stats.dwell);
        // The hold is capped at MAX_DWELL, which is exactly what keeps a long
        // hold from turning into autorepeat; 0.2 s sits right at that cap.
        assert!(approx(m, 0.2, 0.15), "{m}");
    }

    // -- Rollover ------------------------------------------------------------
    #[test]
    fn rollover_happens_and_rises_with_speed() {
        let text = SAMPLE.repeat(2);
        let slow = stroke_stats(&plan_seeded(&text, &cad(45.0), 8)).rollover_rate();
        let fast = stroke_stats(&plan_seeded(&text, &cad(130.0), 8)).rollover_rate();
        assert!(slow < fast, "{slow} {fast}");
        assert!((0.0..0.20).contains(&slow), "{slow}");
        assert!(fast > 0.20, "{fast}");
    }

    #[test]
    fn rollover_zero_releases_every_key_before_the_next() {
        let c = Cadence {
            rollover: 0.0,
            ..cad(130.0)
        };
        let stats = stroke_stats(&plan_seeded(&SAMPLE.repeat(2), &c, 9));
        assert_eq!(stats.rollover_rate(), 0.0);
        assert!(stats.flight.iter().cloned().fold(f64::MAX, f64::min) > 0.0);
    }

    #[test]
    fn one_finger_is_never_on_two_keys_at_once() {
        for text in ["ed".repeat(40), "ol".repeat(40), "aq".repeat(40)] {
            let stats = stroke_stats(&plan_seeded(&text, &cad(200.0), 10));
            assert_eq!(stats.rollover_rate(), 0.0, "{text}");
        }
    }

    #[test]
    fn a_repeated_letter_is_always_released_before_being_struck_again() {
        for wpm in [70.0, 200.0, 600.0] {
            let events = plan_seeded("aaabbbccc mmm lll", &cad(wpm), 12);
            let mut held: HashSet<&str> = HashSet::new();
            for e in &events {
                match e.action {
                    Action::Down => {
                        assert!(
                            !held.contains(e.value.as_str()),
                            "{:?} pressed while held",
                            e.value
                        );
                        held.insert(&e.value);
                    }
                    Action::Up => {
                        held.remove(e.value.as_str());
                    }
                }
            }
        }
    }

    #[test]
    fn every_press_is_matched_by_exactly_one_release() {
        let events = plan_seeded(
            SAMPLE,
            &Cadence {
                typo_rate: 0.1,
                ..cad(90.0)
            },
            13,
        );
        let downs = events.iter().filter(|e| e.action == Action::Down).count();
        let ups = events.iter().filter(|e| e.action == Action::Up).count();
        assert!(downs == ups && ups > 0);
        assert_eq!(events[0].action, Action::Down);
        assert_eq!(events.last().unwrap().action, Action::Up);
    }

    #[test]
    fn no_more_than_two_keys_are_ever_held_at_once() {
        // Overlapping a pair is rollover; holding three is a stuck key.
        let events = plan_seeded(&SAMPLE.repeat(2), &cad(250.0), 14);
        let (mut held, mut peak) = (0i32, 0i32);
        for e in &events {
            held += if e.action == Action::Down { 1 } else { -1 };
            peak = peak.max(held);
        }
        assert!(peak <= 2, "peak {peak}");
    }

    // -- Interval shape ------------------------------------------------------
    #[test]
    fn all_knobs_zero_is_a_metronome() {
        let intervals = stroke_stats(&plan_seeded(SAMPLE, &flat(70.0), 1)).interval;
        let lo = intervals.iter().cloned().fold(f64::MAX, f64::min);
        let hi = intervals.iter().cloned().fold(f64::MIN, f64::max);
        assert!(hi - lo < 1e-9, "{}", hi - lo);
        assert!(approx(intervals[0], 60.0 / 70.0 / 5.0, 1e-6));
    }

    #[test]
    fn default_cadence_is_not_a_metronome() {
        let intervals = stroke_stats(&plan_seeded(SAMPLE, &cad(70.0), 1)).interval;
        assert!(stdev(&intervals) / mean(&intervals) > 0.25);
    }

    #[test]
    fn intervals_are_never_negative() {
        for c in [
            Cadence {
                variance: 1.5,
                ..cad(600.0)
            },
            cad(5.0),
        ] {
            let events = plan_seeded(SAMPLE, &c, 3);
            assert!(events.iter().all(|e| e.delay >= 0.0));
            assert!(stroke_stats(&events).interval.iter().all(|i| *i > 0.0));
        }
    }

    #[test]
    fn same_finger_digraph_is_slower_than_alternating_hands() {
        // 'ed' reuses one finger; 'th' alternates hands. Rhythm should show it.
        let c = Cadence {
            variance: 0.0,
            hesitation: 0.0,
            sentence_pause: 0.0,
            drift: 0.0,
            rhythm: 1.0,
            ..cad(70.0)
        };
        let same = stroke_stats(&plan_seeded("ed", &c, 1)).interval[0];
        let alt = stroke_stats(&plan_seeded("th", &c, 1)).interval[0];
        assert!(same > alt, "{same} {alt}");
    }

    #[test]
    fn schedule_is_reproducible_under_a_seed() {
        let c = Cadence {
            typo_rate: 0.05,
            ..cad(70.0)
        };
        assert_eq!(plan_seeded(SAMPLE, &c, 42), plan_seeded(SAMPLE, &c, 42));
        assert_ne!(plan_seeded(SAMPLE, &c, 42), plan_seeded(SAMPLE, &c, 43));
    }

    // -- Content -------------------------------------------------------------
    #[test]
    fn newline_and_tab_become_named_keys() {
        let values: HashSet<String> = plan_seeded("a\n\tb", &Cadence::default(), 1)
            .into_iter()
            .map(|e| e.value)
            .collect();
        assert!(values.contains("enter") && values.contains("tab"));
    }

    #[test]
    fn typos_are_always_followed_by_a_correction() {
        let events = plan_seeded(
            "the quick brown fox",
            &Cadence {
                typo_rate: 0.2,
                ..cad(70.0)
            },
            5,
        );
        let downs = presses(&events);
        let corrections = downs.iter().filter(|e| e.value == "backspace").count();
        let strays = downs
            .iter()
            .filter(|e| e.index.is_none() && e.value != "backspace")
            .count();
        assert!(
            corrections == strays && strays > 0,
            "{corrections} {strays}"
        );
    }

    #[test]
    fn progress_indices_never_go_backwards() {
        let mut seen: Option<usize> = None;
        for e in plan_seeded(
            SAMPLE,
            &Cadence {
                typo_rate: 0.2,
                ..cad(70.0)
            },
            9,
        ) {
            if let Some(i) = e.index {
                assert_eq!(e.action, Action::Down);
                assert!(seen.is_none_or(|s| i > s));
                seen = Some(i);
            }
        }
        assert_eq!(seen, Some(SAMPLE.chars().count() - 1));
    }

    #[test]
    fn empty_text_produces_no_events() {
        assert!(plan_seeded("", &Cadence::default(), 1).is_empty());
        assert_eq!(duration(&[]), 0.0);
        assert_eq!(typing_duration(&[]), 0.0);
    }

    #[test]
    fn invalid_cadence_is_rejected() {
        let bad = [
            cad(0.0),
            cad(5000.0),
            Cadence {
                variance: -1.0,
                ..Default::default()
            },
            Cadence {
                typo_rate: 0.9,
                ..Default::default()
            },
            Cadence {
                sentence_pause: 2.0,
                ..Default::default()
            },
            Cadence {
                max_pause: -1.0,
                ..Default::default()
            },
            Cadence {
                dwell: -0.1,
                ..Default::default()
            },
            Cadence {
                dwell: 5.0,
                ..Default::default()
            },
            Cadence {
                rollover: 1.5,
                ..Default::default()
            },
            Cadence {
                dwell_variance: -1.0,
                ..Default::default()
            },
        ];
        for c in bad {
            assert!(plan("x", &c, None, None).is_err(), "{c:?}");
        }
    }

    #[test]
    fn duration_formatting() {
        for (seconds, expected) in [
            (0.4, "<1s"),
            (7.2, "7s"),
            (65.0, "1m 05s"),
            (3725.0, "1h 02m"),
        ] {
            assert_eq!(format_duration(seconds), expected);
        }
    }

    // -- Modifiers are keystrokes -------------------------------------------
    const SHIFT: &str = "KEY_LEFTSHIFT";

    /// A stand-in for a keyboard layout: shift for capitals and for !?:.
    fn us_modifiers(token: &str) -> Vec<String> {
        let mut chars = token.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_uppercase() || "!?:\"".contains(c) => vec![SHIFT.to_string()],
            _ => vec![],
        }
    }

    fn with_shift(text: &str, wpm: f64, seed: u64) -> Vec<Event> {
        plan(text, &cad(wpm), Some(seed), Some(&us_modifiers)).unwrap()
    }

    fn absolute(events: &[Event]) -> Vec<(f64, Action, &str)> {
        let mut clock = 0.0;
        events
            .iter()
            .map(|e| {
                clock += e.delay;
                (clock, e.action, e.value.as_str())
            })
            .collect()
    }

    fn modifier_spans(events: &[Event]) -> Vec<(f64, f64)> {
        let (mut spans, mut down) = (Vec::new(), None);
        for (moment, action, value) in absolute(events) {
            if value != SHIFT {
                continue;
            }
            match action {
                Action::Down => down = Some(moment),
                Action::Up => {
                    if let Some(d) = down.take() {
                        spans.push((d, moment));
                    }
                }
            }
        }
        spans
    }

    #[test]
    fn without_a_layout_no_modifier_is_scheduled() {
        let events = plan_seeded("ABC", &cad(70.0), 1);
        assert!(events.iter().all(|e| e.value != SHIFT));
    }

    #[test]
    fn shift_leads_the_key_it_modifies_by_a_human_margin() {
        let events = with_shift("Aa Bb Cc Dd Ee Ff Gg Hh", 70.0, 3);
        let timeline = absolute(&events);
        let mut leads = Vec::new();
        for (start, _end) in modifier_spans(&events) {
            let first = timeline
                .iter()
                .find(|(m, a, v)| *a == Action::Down && *v != SHIFT && *m > start)
                .unwrap()
                .0;
            leads.push(first - start);
        }
        assert!(!leads.is_empty(), "no modifier was scheduled");
        assert!(leads.iter().all(|l| 0.015 < *l && *l < 0.40), "{leads:?}");
        assert!((0.04..0.15).contains(&mean(&leads)), "{}", mean(&leads));
    }

    #[test]
    fn shift_is_still_held_after_the_key_it_modified_is_released() {
        // A hand does not lift both fingers together.
        let events = with_shift("Aa Bb Cc Dd", 70.0, 5);
        let timeline = absolute(&events);
        let mut lags = Vec::new();
        for (_start, end) in modifier_spans(&events) {
            let released = timeline
                .iter()
                .filter(|(m, a, v)| *a == Action::Up && *v != SHIFT && *m < end)
                .map(|(m, _, _)| *m)
                .fold(None, |acc: Option<f64>, m| {
                    Some(acc.map_or(m, |a| a.max(m)))
                });
            if let Some(r) = released {
                lags.push(end - r);
            }
        }
        assert!(
            !lags.is_empty() && lags.iter().any(|l| *l > 0.005),
            "{lags:?}"
        );
    }

    #[test]
    fn a_run_of_capitals_holds_one_continuous_shift() {
        let spans = modifier_spans(&with_shift("ABCDEF gh", 70.0, 2));
        assert_eq!(spans.len(), 1, "{spans:?}");
    }

    #[test]
    fn shift_is_never_held_over_a_character_that_did_not_ask_for_it() {
        // The correctness half. Shift may lift while the capital is still
        // physically down -- the character was decided when the key went down --
        // but it must be gone before the next unshifted key is pressed.
        let text = "Hello World! \"Quoted\" Xy: Zz? ABCdef Ghi";
        let events = with_shift(text, 70.0, 9);
        let spans = modifier_spans(&events);
        for (moment, action, value) in absolute(&events) {
            if action != Action::Down || value == SHIFT {
                continue;
            }
            let held = spans.iter().any(|(s, e)| *s <= moment && moment <= *e);
            assert_eq!(
                held,
                !us_modifiers(value).is_empty(),
                "{value:?} at {moment}"
            );
        }
    }

    #[test]
    fn shift_never_precedes_the_start_of_the_schedule() {
        for text in ["A", "Ab", "!"] {
            let events = with_shift(text, 70.0, 1);
            assert!(events.iter().all(|e| e.delay >= 0.0), "{text}");
        }
    }

    // -- Trend across a document ---------------------------------------------
    #[test]
    fn typing_starts_slow_and_ends_slower_than_its_middle() {
        // A flat pace across three thousand characters is not a person.
        //
        // The Python test asserts this with one seed and the windows "first half
        // of the first third" and "middle third". Neither is what the model
        // promises: the warm-up is gone within ~180 characters and fatigue is
        // still ramping up to character 1500, so over those windows the start
        // is *not* slower than the middle on average (measured on the Python
        // implementation: 0.987 and 1.003 over 60 seeds) and the test passed
        // on the strength of seed 4. This checks the property itself: windows
        // matched to the model's constants, compared within each run so the
        // per-run pace cancels, and averaged over enough runs to see through the
        // speed wander. Boundary pauses and hesitation are switched off only
        // because they are noise on this question, not part of the trend.
        let text = "the quick brown fox jumps over the lazy dog ".repeat(90);
        let c = Cadence {
            variance: 0.0,
            rhythm: 0.0,
            hesitation: 0.0,
            sentence_pause: 0.0,
            ..cad(70.0)
        };
        let (mut warm, mut tired, runs) = (0.0, 0.0, 120);
        for seed in 0..runs {
            let iv = stroke_stats(&plan_seeded(&text, &c, seed)).interval;
            let early = mean(&iv[..120]);
            let settled = mean(&iv[400..900]);
            let late = mean(&iv[2000..3000]);
            warm += early / settled;
            tired += late / settled;
        }
        let (warm, tired) = (warm / runs as f64, tired / runs as f64);
        assert!(warm > 1.01, "start vs settled: {warm}");
        assert!(tired > 1.02, "end vs settled: {tired}");
    }

    #[test]
    fn the_trend_is_shape_and_not_speed() {
        // Warm-up and fatigue must not cost the document its requested rate.
        let text = "The quick brown fox jumps over the lazy dog. ".repeat(45);
        let rates: Vec<f64> = (0..30).map(|s| delivered(&text, &cad(70.0), s)).collect();
        assert!(approx(mean(&rates), 70.0, 0.02), "{}", mean(&rates));
    }

    #[test]
    fn no_drift_means_no_trend() {
        let text = "the quick brown fox jumps over the lazy dog ".repeat(40);
        let intervals = stroke_stats(&plan_seeded(&text, &flat(70.0), 1)).interval;
        let lo = intervals.iter().cloned().fold(f64::MAX, f64::min);
        let hi = intervals.iter().cloned().fold(f64::MIN, f64::max);
        assert!(hi - lo < 1e-9, "{}", hi - lo);
    }

    // -- The clamps at the extremes ------------------------------------------
    // Everything above is about ordinary typing. At 2000 wpm with the widest
    // spread the model allows, a lognormal tail can compress an interval below
    // the minimum hold, and the clamp that decides when a key comes up has to
    // choose between hold time and ordering. Ordering is not negotiable: a
    // release at or before its own press is an up with no down, and a release
    // after the next press of the same key means the kernel sees a repeat and
    // drops the character.
    #[test]
    fn a_release_never_overtakes_its_own_press_at_any_speed() {
        let extreme = Cadence {
            wpm: 2000.0,
            variance: 2.0,
            drift: 0.0,
            sentence_pause: 0.0,
            ..Cadence::default()
        };
        for text in [
            "aa".repeat(40),
            "ed".repeat(40),
            "ll".repeat(40),
            "the quick brown fox ".repeat(10),
        ] {
            for seed in [1, 7] {
                let events = plan_seeded(&text, &extreme, seed);
                let mut held: std::collections::HashSet<&str> = Default::default();
                for event in &events {
                    match event.action {
                        Action::Down => {
                            assert!(
                                held.insert(&event.value),
                                "{} pressed while held",
                                event.value
                            );
                        }
                        Action::Up => {
                            assert!(
                                held.remove(event.value.as_str()),
                                "up without down: {}",
                                event.value
                            );
                        }
                    }
                }
                assert!(held.is_empty(), "{held:?}");
                assert!(
                    stroke_stats(&events).dwell.iter().all(|d| *d > 0.0),
                    "a hold collapsed to nothing"
                );
            }
        }
    }

    #[test]
    fn a_modifier_is_never_still_held_when_an_unmodified_key_is_struck() {
        // The extreme-speed version of the correctness rule above. When the
        // schedule is too tight to hold Shift for any length of time, the run is
        // dropped and the backend presses Shift with its own key -- which still
        // produces the capital. Keeping it would capitalise the next letter.
        for wpm in [400.0, 2000.0] {
            let cadence = Cadence {
                wpm,
                variance: 2.0,
                drift: 0.0,
                ..Cadence::default()
            };
            let events = plan(&"Ab".repeat(30), &cadence, Some(3), Some(&us_modifiers)).unwrap();
            let mut shift_down = false;
            for (_moment, action, value) in absolute(&events) {
                if value == SHIFT {
                    shift_down = action == Action::Down;
                } else if action == Action::Down && value == "b" {
                    assert!(!shift_down, "Shift still held at {wpm} wpm");
                }
            }
        }
    }

    #[test]
    fn out_of_range_shape_knobs_are_rejected() {
        // The help says 0-1; a value outside it has no meaning in the model.
        for bad in [
            Cadence {
                rhythm: 1.5,
                ..Cadence::default()
            },
            Cadence {
                hesitation: -0.1,
                ..Cadence::default()
            },
            Cadence {
                drift: 2.0,
                ..Cadence::default()
            },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
    }
}
