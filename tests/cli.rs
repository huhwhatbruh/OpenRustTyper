//! The command line: sources, overrides, and exit codes.
//!
//! Every test here is a dry run or a read-only report; none of them types.

use std::io::Write;
use std::process::{Command, Stdio};

struct Ran {
    code: i32,
    out: String,
    err: String,
}

fn run_with(args: &[&str], stdin: Option<&str>) -> Ran {
    // An empty config dir, so a developer's saved settings cannot change what
    // these tests see.
    let config = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_openautotyper"))
        .args(args)
        .env("XDG_CONFIG_HOME", config.path())
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(text) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    }
    let output = child.wait_with_output().unwrap();
    Ran {
        code: output.status.code().unwrap_or(-1),
        out: String::from_utf8_lossy(&output.stdout).into_owned(),
        err: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn run(args: &[&str]) -> Ran {
    run_with(args, None)
}

#[test]
fn dry_run_reproduces_the_input_exactly() {
    let r = run(&["--dry-run", "-t", "Hello, world!", "-q"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.out, "Hello, world!");
}

#[test]
fn dry_run_folds_typography_like_a_real_run() {
    let r = run(&[
        "--dry-run",
        "-t",
        "a \u{201c}b\u{201d} \u{2014} c\u{2026}",
        "-q",
    ]);
    assert_eq!(r.code, 0);
    assert_eq!(r.out, "a \"b\" -- c...");
}

#[test]
fn no_fold_keeps_the_original_characters() {
    let r = run(&["--dry-run", "--no-fold", "-t", "a \u{2014} b", "-q"]);
    assert_eq!(r.out, "a \u{2014} b");
}

#[test]
fn text_comes_from_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("input.txt");
    std::fs::write(&path, "from a file").unwrap();
    let r = run(&["--dry-run", "-f", path.to_str().unwrap(), "-q"]);
    assert_eq!(r.out, "from a file");
}

#[test]
fn text_comes_from_standard_input() {
    let r = run_with(&["--dry-run", "-q"], Some("piped in\nacross lines"));
    assert_eq!(r.code, 0);
    assert_eq!(r.out, "piped in\nacross lines");
}

#[test]
fn a_missing_file_is_a_usage_error() {
    let r = run(&["--dry-run", "-f", "/nonexistent/nope.txt"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("cannot read"), "{}", r.err);
}

#[test]
fn conflicting_sources_are_refused() {
    let r = run(&["--dry-run", "-t", "x", "-f", "y"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("choose one of"), "{}", r.err);
}

#[test]
fn empty_input_is_an_error() {
    let r = run(&["--dry-run", "-t", "   ", "-q"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("empty"), "{}", r.err);
}

#[test]
fn out_of_range_values_are_refused_before_typing() {
    let r = run(&["--dry-run", "-t", "x", "--typos", "0.9", "-q"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("typo_rate"), "{}", r.err);
}

#[test]
fn unknown_flags_are_refused() {
    assert_eq!(run(&["--teleport"]).code, 2);
}

#[test]
fn list_backends_names_every_backend() {
    let r = run(&["--list-backends"]);
    assert_eq!(r.code, 0);
    assert!(
        r.out.contains("null") && r.out.contains("BACKEND"),
        "{}",
        r.out
    );
}

#[test]
fn doctor_reports_without_failing_to_run() {
    let r = run(&["--doctor"]);
    assert!(r.code == 0 || r.code == 1, "{}", r.code);
    assert!(r.out.contains("backends:"), "{}", r.out);
    assert!(r.out.contains("keyboard layout:"), "{}", r.out);
}

#[test]
fn version_is_reported() {
    let r = run(&["--version"]);
    assert_eq!(r.code, 0);
    assert!(r.out.contains(env!("CARGO_PKG_VERSION")), "{}", r.out);
}

#[test]
fn the_summary_reports_normalization() {
    let r = run(&["--dry-run", "-t", "a \u{201c}b\u{201d}"]);
    assert!(r.err.contains("normalized"), "{}", r.err);
}

#[test]
fn quiet_suppresses_the_summary() {
    let r = run(&["--dry-run", "-t", "a \u{201c}b\u{201d}", "-q"]);
    assert_eq!(r.err, "");
}

#[test]
fn the_summary_counts_and_estimates() {
    let r = run(&["--dry-run", "-t", "one two three", "--wpm", "60"]);
    assert!(r.err.contains("would type: 13 chars"), "{}", r.err);
    assert!(r.err.contains("3 words"), "{}", r.err);
    assert!(r.err.contains("at 60 wpm"), "{}", r.err);
    assert!(r.err.contains("-- dry run: 13 characters"), "{}", r.err);
}

#[test]
fn a_dry_run_says_which_characters_need_the_clipboard() {
    // The dry run used to accept every character, so it could not tell you
    // that a real run would have to reach for the clipboard.
    let r = run(&["--dry-run", "-t", "hi \u{1F600}"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.out, "hi \u{1F600}");
    assert!(r.err.contains("clipboard"), "{}", r.err);
}

#[test]
fn an_unknown_panic_key_is_refused_before_anything_is_typed() {
    // It used to print "press FOO" while the watcher fell back to Escape.
    let r = run(&["--dry-run", "-t", "x", "--panic-key", "KEY_NOT_A_KEY", "-q"]);
    assert_eq!(r.code, 2);
    assert!(r.err.contains("unknown panic key"), "{}", r.err);
}

#[test]
fn seed_makes_a_run_reproducible() {
    let args = [
        "--dry-run",
        "-t",
        "repeatable text",
        "--typos",
        "0.2",
        "--seed",
        "5",
        "-q",
    ];
    let (first, second) = (run(&args).out, run(&args).out);
    assert_eq!(first, second);
    assert_eq!(first, "repeatable text");
}

#[test]
fn typos_never_change_the_finished_text() {
    for seed in 0..5 {
        let seed = seed.to_string();
        let r = run(&[
            "--dry-run",
            "-t",
            "The quick brown fox.",
            "--typos",
            "0.25",
            "--seed",
            &seed,
            "-q",
        ]);
        assert_eq!(r.out, "The quick brown fox.");
    }
}

#[test]
fn tabs_can_be_expanded() {
    let r = run(&["--dry-run", "-t", "a\tb", "--expand-tabs", "4", "-q"]);
    assert_eq!(r.out, "a   b");
}

#[test]
fn a_dry_run_is_fast_whatever_the_estimate() {
    let began = std::time::Instant::now();
    let r = run(&[
        "--dry-run",
        "-t",
        &"slow typing indeed ".repeat(200),
        "--wpm",
        "10",
        "-q",
    ]);
    assert_eq!(r.code, 0);
    assert!(began.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn a_real_run_without_a_usable_backend_points_at_the_doctor() {
    // On a machine where /dev/uinput is writable this would type, so only the
    // failure path is asserted: naming a backend that does not exist.
    let r = run(&["-t", "x", "--backend", "telepathy", "-d", "0"]);
    assert_eq!(r.code, 3);
    assert!(r.err.contains("--doctor"), "{}", r.err);
}
