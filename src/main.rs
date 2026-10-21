//! Command line interface.

use std::io::{IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::{ArgAction, Parser};

use openautotyper::backend::{self, identity, xkb, NullBackend};
use openautotyper::config::{config_path, Settings};
use openautotyper::session::{Progress, SessionOptions, State, TypeSession};
use openautotyper::text::{normalize, stats, NormalizeOptions};
use openautotyper::timing::{format_duration, Cadence};
use openautotyper::{clipboard, panic as abort_key, VERSION};

#[derive(Parser, Debug)]
#[command(
    name = "openautotyper",
    version = VERSION,
    about = "Type text into the focused window as real keystrokes.",
    after_help = "examples:\n  openautotyper -f essay.txt --wpm 80\n  wl-paste | openautotyper --delay 3\n  openautotyper -t 'hello' --flat --wpm 200\n  openautotyper --doctor\n",
    disable_help_flag = false
)]
struct Args {
    /// read the text from a file
    #[arg(short, long, help_heading = "Text source (default: stdin)")]
    file: Option<PathBuf>,
    /// the text to type, given literally
    #[arg(short, long, help_heading = "Text source (default: stdin)")]
    text: Option<String>,
    /// read the text from the clipboard
    #[arg(short, long, help_heading = "Text source (default: stdin)")]
    clipboard: bool,

    /// words per minute, end to end (default 70)
    #[arg(long, help_heading = "Pace")]
    wpm: Option<f64>,
    /// countdown before typing starts, to focus the target window (default 5)
    #[arg(short, long, value_name = "SECS", help_heading = "Pace")]
    delay: Option<f64>,

    /// interval spread, 0-2 (default 0.30)
    #[arg(
        long,
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    variance: Option<f64>,
    /// finger-travel weighting, 0-1 (default 1.0)
    #[arg(
        long,
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    rhythm: Option<f64>,
    /// fraction of keys with a hitch (default 0.03)
    #[arg(
        long,
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    hesitation: Option<f64>,
    /// chance of pausing at a boundary, 0-1
    #[arg(
        long,
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    sentence_pause: Option<f64>,
    /// longest boundary pause
    #[arg(
        long,
        value_name = "SECS",
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    max_pause: Option<f64>,
    /// slow speed wander, 0-1 (default 1.0)
    #[arg(
        long,
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    drift: Option<f64>,
    /// per-character slip rate, 0-0.25; always corrected (default 0)
    #[arg(
        long,
        value_name = "RATE",
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    typos: Option<f64>,
    /// perfect metronome: zeroes every rhythm knob above
    #[arg(
        long,
        help_heading = "Rhythm (shape only; none of these change the wpm)"
    )]
    flat: bool,

    /// type smart quotes and dashes as-is instead of folding to ASCII
    #[arg(long, help_heading = "Text handling")]
    no_fold: bool,
    /// replace tabs with N spaces (use when Tab moves between fields)
    #[arg(long, value_name = "N", help_heading = "Text handling")]
    expand_tabs: Option<usize>,

    /// force a backend (see --list-backends)
    #[arg(long, help_heading = "Run")]
    backend: Option<String>,
    /// evdev key name that aborts, e.g. KEY_ESC (default) or KEY_F12
    #[arg(long, value_name = "KEY", help_heading = "Run")]
    panic_key: Option<String>,
    /// abort as soon as any key is pressed on a real keyboard
    #[arg(long, help_heading = "Run")]
    any_key_stops: bool,
    /// disable the abort key (not recommended)
    #[arg(long, help_heading = "Run")]
    no_panic_key: bool,
    /// reproducible rhythm, for testing
    #[arg(long, help_heading = "Run")]
    seed: Option<u64>,
    /// print what would be typed and how long it would take
    #[arg(short = 'n', long, help_heading = "Run")]
    dry_run: bool,
    /// no progress output
    #[arg(short, long, action = ArgAction::SetTrue, help_heading = "Run")]
    quiet: bool,

    /// show every backend and why it can or cannot run
    #[arg(long, help_heading = "Information")]
    list_backends: bool,
    /// diagnose this machine's setup
    #[arg(long, help_heading = "Information")]
    doctor: bool,
    /// launch the graphical interface
    #[arg(long, help_heading = "Information")]
    gui: bool,
}

/// Print a usage error the way argparse did, and exit with status 2.
fn usage_error(message: &str) -> ! {
    eprintln!("openautotyper: error: {message}");
    std::process::exit(2);
}

fn read_source(args: &Args) -> String {
    let given = [args.file.is_some(), args.text.is_some(), args.clipboard];
    if given.iter().filter(|g| **g).count() > 1 {
        usage_error("choose one of --file, --text or --clipboard");
    }
    if let Some(text) = &args.text {
        return text.clone();
    }
    if let Some(path) = &args.file {
        return match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) => usage_error(&format!("cannot read {}: {e}", path.display())),
        };
    }
    if args.clipboard {
        return clipboard::read().unwrap_or_else(|| {
            usage_error(&format!(
                "could not read the clipboard: {}",
                clipboard::available().1
            ))
        });
    }
    if std::io::stdin().is_terminal() {
        usage_error("no text given \u{2014} use --file, --text, --clipboard, or pipe to stdin");
    }
    let mut text = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut text) {
        usage_error(&format!("cannot read standard input: {e}"));
    }
    text
}

fn apply_overrides(settings: &mut Settings, args: &Args) -> Cadence {
    if args.flat {
        settings.variance = 0.0;
        settings.rhythm = 0.0;
        settings.hesitation = 0.0;
        settings.sentence_pause = 0.0;
        settings.drift = 0.0;
        // A metronome with slips is not a metronome, and the help text promises
        // every rhythm knob above is zeroed.
        settings.typo_rate = 0.0;
    }
    let set = |slot: &mut f64, value: Option<f64>| {
        if let Some(v) = value {
            *slot = v;
        }
    };
    set(&mut settings.wpm, args.wpm);
    set(&mut settings.variance, args.variance);
    set(&mut settings.rhythm, args.rhythm);
    set(&mut settings.hesitation, args.hesitation);
    set(&mut settings.sentence_pause, args.sentence_pause);
    set(&mut settings.max_pause, args.max_pause);
    set(&mut settings.drift, args.drift);
    set(&mut settings.typo_rate, args.typos);
    settings.cadence()
}

fn print_backends() -> ExitCode {
    let rows = backend::report();
    let width = rows
        .iter()
        .map(|(n, _, _)| n.len())
        .max()
        .unwrap_or(7)
        .max("BACKEND".len());
    println!("{:<width$}  USABLE  REASON", "BACKEND");
    for (name, ok, reason) in rows {
        println!(
            "{name:<width$}  {}  {reason}",
            if ok { "yes   " } else { "no    " }
        );
    }
    ExitCode::SUCCESS
}

/// Explain what this machine can and cannot do, and how to fix it.
fn doctor() -> ExitCode {
    println!(
        "openautotyper {VERSION}  \u{b7}  rust  \u{b7}  {} {}",
        std::env::consts::OS,
        std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .unwrap_or_default()
            .trim()
    );
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "unknown".into());
    println!(
        "session: {} ({})",
        env("XDG_SESSION_TYPE"),
        env("XDG_CURRENT_DESKTOP")
    );
    let path = config_path();
    println!(
        "config:  {}{}",
        path.display(),
        if path.exists() {
            ""
        } else {
            "  (not created yet)"
        }
    );

    println!("\nbackends:");
    let rows = backend::report();
    for (name, ok, reason) in &rows {
        println!("  {} {name:<8} {reason}", if *ok { "OK " } else { "-- " });
    }

    let layout = xkb::load_layout();
    println!(
        "\nkeyboard layout: {} \u{2014} {} characters reachable",
        layout.source,
        layout.len()
    );

    let ident = identity::for_this_machine(None);
    // Worth printing: this is the name the virtual keyboard appears under in
    // /proc/bus/input/devices, which anything on the machine can read.
    println!(
        "appears as:      {:?} \u{2014} {}",
        ident.name, ident.source
    );

    let (ok, reason) = abort_key::available();
    println!(
        "abort key:       {} \u{2014} {reason}",
        if ok { "OK" } else { "UNAVAILABLE" }
    );
    let (ok, reason) = clipboard::available();
    println!(
        "clipboard:       {} \u{2014} {reason}",
        if ok { "OK" } else { "UNAVAILABLE" }
    );

    let usable: Vec<&str> = rows
        .iter()
        .filter(|(n, ok, _)| *ok && *n != "null")
        .map(|(n, _, _)| *n)
        .collect();
    if usable.is_empty() {
        println!(
            "\nNo usable backend. On Linux this is almost always /dev/uinput permissions:\n  \
             echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | sudo tee /etc/udev/rules.d/99-uinput.rules\n  \
             sudo usermod -aG input \"$USER\"   # then log out and back in"
        );
        return ExitCode::FAILURE;
    }
    println!("\nReady. Typing will use: {}", usable[0]);
    ExitCode::SUCCESS
}

/// One-line live status on stderr, redrawn in place.
struct ProgressLine {
    enabled: bool,
    width: std::sync::Mutex<usize>,
}

impl ProgressLine {
    fn new(enabled: bool) -> Self {
        Self {
            enabled: enabled && std::io::stderr().is_terminal(),
            width: Default::default(),
        }
    }

    fn show(&self, p: &Progress) {
        if !self.enabled {
            return;
        }
        let line = match p.state {
            State::Countdown => format!("  {}", p.message),
            State::Typing | State::Paused => {
                let done = (p.fraction() * 24.0) as usize;
                let bar = format!("{}{}", "#".repeat(done), ".".repeat(24 - done));
                let tag = if p.state == State::Paused {
                    "paused "
                } else {
                    ""
                };
                format!(
                    "  [{bar}] {:3.0}%  {}/{}  {tag}{} left",
                    p.fraction() * 100.0,
                    p.typed,
                    p.total,
                    format_duration(p.remaining)
                )
            }
            _ => String::new(),
        };
        self.draw(&line);
    }

    fn draw(&self, line: &str) {
        let mut width = self.width.lock().unwrap();
        let pad = " ".repeat(width.saturating_sub(line.chars().count()));
        let mut err = std::io::stderr();
        let _ = write!(err, "\r{line}{pad}");
        let _ = err.flush();
        *width = line.chars().count();
    }

    fn clear(&self) {
        if self.enabled && *self.width.lock().unwrap() > 0 {
            self.draw("");
            let _ = write!(std::io::stderr(), "\r");
        }
    }
}

/// Commas between thousands, as Python's `{:,}`.
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn main() -> ExitCode {
    let args = Args::parse();

    if args.gui {
        return launch_gui();
    }
    if args.list_backends {
        return print_backends();
    }
    if args.doctor {
        return doctor();
    }

    let mut settings = Settings::load();
    let raw = read_source(&args);

    let normalized = normalize(
        &raw,
        NormalizeOptions {
            fold_typography: !args.no_fold && settings.fold_typography,
            expand_tabs: args.expand_tabs.unwrap_or(settings.expand_tabs),
            strip_trailing_ws: settings.strip_trailing_ws,
        },
    );
    let text = normalized.text.clone();
    if text.trim().is_empty() {
        eprintln!("nothing to type: the input is empty");
        return ExitCode::from(2);
    }

    let cadence = apply_overrides(&mut settings, &args);
    if let Err(e) = cadence.validate() {
        usage_error(&e);
    }

    let countdown = args.delay.unwrap_or(settings.countdown);

    let backend: Box<dyn backend::Backend> = if args.dry_run {
        // A preview is only worth reading if it says what the real run would
        // do. Giving the recorder the active layout's characters makes the
        // clipboard fallback show up here as it would there, instead of
        // accepting every character as reachable.
        let reachable: String = xkb::load_layout().table.keys().collect();
        Box::new(NullBackend::reaching(&reachable))
    } else {
        let wanted = args
            .backend
            .clone()
            .or_else(|| Some(settings.backend.clone()));
        match backend::select(wanted.as_deref()) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("error: {e}");
                eprintln!("\nrun 'openautotyper --doctor' for a diagnosis");
                return ExitCode::from(3);
            }
        }
    };

    let panic_key = if args.no_panic_key {
        None
    } else {
        let asked = args
            .panic_key
            .clone()
            .unwrap_or_else(|| settings.panic_key.clone());
        // An empty value from a hand-edited config means "the default", not
        // "invalid"; anything else is checked rather than silently replaced.
        let asked = if asked.trim().is_empty() {
            "KEY_ESC".to_string()
        } else {
            asked
        };
        match abort_key::resolve_key(&asked) {
            Ok(key) => Some(key),
            Err(e) => usage_error(&e),
        }
    };
    let any_key = args.any_key_stops || settings.any_key_stops;
    let line = Arc::new(ProgressLine::new(!args.quiet && !args.dry_run));

    let sink = line.clone();
    let mut session = match TypeSession::new(
        &text,
        &cadence,
        backend,
        SessionOptions {
            countdown: if args.dry_run { 0.0 } else { countdown },
            panic_key: if args.dry_run {
                None
            } else {
                panic_key.clone()
            },
            any_key_stops: if args.dry_run { false } else { any_key },
            on_progress: Some(Arc::new(move |p| sink.show(&p))),
            seed: args.seed,
            // A dry run is a preview. Pacing it would make it take exactly as
            // long as the typing it is previewing, which is the one thing
            // nobody wants from a preview.
            realtime: !args.dry_run,
            ..Default::default()
        },
    ) {
        Ok(s) => s,
        Err(e) => usage_error(&e),
    };

    let (chars, words, lines) = stats(&text);
    if !args.quiet {
        let where_ = if args.dry_run {
            "would type".to_string()
        } else {
            format!("typing via {}", session.backend().name())
        };
        eprintln!(
            "{where_}: {} chars \u{b7} {} words \u{b7} {} lines  \u{b7}  ~{} at {} wpm",
            grouped(chars),
            grouped(words),
            grouped(lines),
            format_duration(session.estimated),
            cadence.wpm
        );
        if normalized.changed() {
            eprintln!("  normalized: {}", normalized.summary());
        }
        if !args.dry_run && (panic_key.is_some() || any_key) {
            // Only promise the abort key if it can actually be delivered. It
            // needs to read /dev/input/event*, which is a different permission
            // from the /dev/uinput that typing needs -- so a machine that types
            // perfectly may have no way to stop, and saying "press Esc" when
            // Esc does nothing is worse than saying nothing at all.
            let (ok, why) = abort_key::available();
            if ok {
                let label = if any_key {
                    "any key".to_string()
                } else {
                    panic_key
                        .as_deref()
                        .unwrap_or("KEY_ESC")
                        .trim_start_matches("KEY_")
                        .to_string()
                };
                eprintln!("  press {label} on your keyboard to abort");
            } else {
                eprintln!("  WARNING: no abort key \u{2014} {why}.\n           Ctrl+C in this terminal still works.");
            }
        }
    }

    // Ctrl+C asks the session to stop, so every key is released on the way out.
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe { libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t) };
    let control = session.control();
    let finished = Arc::new(AtomicBool::new(false));
    let watcher = {
        let finished = finished.clone();
        std::thread::spawn(move || {
            while !finished.load(Ordering::SeqCst) {
                if INTERRUPTED.load(Ordering::SeqCst) {
                    control.stop("interrupted");
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        })
    };
    let result = session.run();
    finished.store(true, Ordering::SeqCst);
    let _ = watcher.join();
    line.clear();

    if !args.quiet && !args.dry_run && !result.panic_active && panic_key.is_none() && !any_key {
        // Silently running without a kill switch is the one thing worth saying
        // out loud even when nothing failed: --no-panic-key is easy to leave in
        // a shell alias.
        eprintln!("note: this run had no abort key attached \u{2014} Ctrl+C still works");
    }

    if args.dry_run {
        let typed = session.backend().recorded_text().unwrap_or_default();
        // `print!` panics when the far end of a pipe has gone away, and
        // `openautotyper -n ... | head` is a legitimate way to read a preview.
        let mut stdout = std::io::stdout();
        match stdout
            .write_all(typed.as_bytes())
            .and_then(|()| stdout.flush())
        {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
                return ExitCode::SUCCESS;
            }
            Err(e) => eprintln!("could not write the preview: {e}"),
        }
        if !args.quiet {
            eprintln!(
                "\n-- dry run: {} characters, ~{}",
                grouped(result.typed),
                format_duration(session.estimated)
            );
            let pasted = session.backend().pasted();
            if !pasted.is_empty() {
                // These are the characters the real run would route through
                // the clipboard: slow, visible, and destructive to whatever
                // the user had copied.
                let count: usize = pasted.iter().map(|p| p.chars().count()).sum();
                let mut shown = String::new();
                for ch in pasted.iter().flat_map(|p| p.chars()) {
                    if !shown.contains(ch) {
                        shown.push(ch);
                    }
                }
                let shown: String = shown.chars().take(20).collect();
                let (ok, why) = clipboard::available();
                if ok {
                    eprintln!("  {count} character(s) would go through the clipboard: {shown:?}");
                } else {
                    eprintln!(
                        "  {count} character(s) have no keystroke and the clipboard is \
                         unavailable ({why}); a real run would skip them: {shown:?}"
                    );
                }
            }
        }
        return ExitCode::SUCCESS;
    }

    match result.state {
        State::Failed => {
            eprintln!("failed: {}", result.reason);
            return ExitCode::from(3);
        }
        State::Stopped => {
            eprintln!(
                "{} after {} of {} characters",
                result.reason,
                grouped(result.typed),
                grouped(result.total)
            );
            return ExitCode::FAILURE;
        }
        _ => {}
    }

    if !args.quiet {
        let rate = if result.duration > 0.0 {
            result.typed as f64 / 5.0 / (result.duration / 60.0)
        } else {
            0.0
        };
        eprintln!(
            "done: {} characters in {} ({rate:.0} wpm)",
            grouped(result.typed),
            format_duration(result.duration)
        );
        if !result.skipped.is_empty() {
            let mut missing = String::new();
            for (_, ch) in &result.skipped {
                if !missing.contains(ch.as_str()) {
                    missing.push_str(ch);
                }
            }
            eprintln!(
                "  warning: {} character(s) could not be typed on this layout: {missing:?}",
                result.skipped.len()
            );
        }
        if result.slip > 0.5 {
            eprintln!(
                "  note: the schedule fell {:.1}s behind \u{2014} the machine was busy",
                result.slip
            );
        }
    }
    ExitCode::SUCCESS
}

#[cfg(feature = "gui")]
fn launch_gui() -> ExitCode {
    openautotyper::gui::run()
}

#[cfg(not(feature = "gui"))]
fn launch_gui() -> ExitCode {
    eprintln!("this build has no graphical interface (rebuild with the \"gui\" feature)");
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Args {
        Args::try_parse_from(std::iter::once("openautotyper").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn flat_zeroes_the_rhythm_knobs() {
        let mut settings = Settings {
            typo_rate: 0.2,
            ..Default::default()
        };
        let c = apply_overrides(&mut settings, &parse(&["--flat"]));
        assert_eq!(
            (
                c.variance,
                c.rhythm,
                c.hesitation,
                c.sentence_pause,
                c.drift,
                c.typo_rate,
            ),
            (0.0, 0.0, 0.0, 0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn explicit_flags_override_the_saved_settings() {
        let mut settings = Settings {
            wpm: 50.0,
            typo_rate: 0.0,
            ..Default::default()
        };
        let c = apply_overrides(&mut settings, &parse(&["--wpm", "123", "--typos", "0.05"]));
        assert_eq!((c.wpm, c.typo_rate), (123.0, 0.05));
    }

    #[test]
    fn flags_beat_flat_when_both_are_given() {
        let mut settings = Settings::default();
        let c = apply_overrides(&mut settings, &parse(&["--flat", "--variance", "0.5"]));
        assert_eq!(c.variance, 0.5);
    }

    #[test]
    fn saved_settings_apply_when_no_flag_says_otherwise() {
        let mut settings = Settings {
            wpm: 55.0,
            hesitation: 0.07,
            ..Default::default()
        };
        let c = apply_overrides(&mut settings, &parse(&[]));
        assert_eq!((c.wpm, c.hesitation), (55.0, 0.07));
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(1_234), "1,234");
        assert_eq!(grouped(12_345_678), "12,345,678");
    }
}
