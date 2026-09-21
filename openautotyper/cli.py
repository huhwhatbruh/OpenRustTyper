"""Command line interface."""

from __future__ import annotations

import argparse
import signal
import sys
from pathlib import Path

from . import __version__, clipboard
from .backends import BackendError, report, select
from .backends.null import NullBackend
from .config import Settings, config_path
from .session import Progress, State, TypeSession
from .text import normalize, stats
from .timing import Cadence, format_duration


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="openautotyper",
        description="Type text into the focused window as real keystrokes.",
        epilog=(
            "examples:\n"
            "  openautotyper -f essay.txt --wpm 80\n"
            "  pbpaste | openautotyper --delay 3\n"
            "  openautotyper -t 'hello' --flat --wpm 200\n"
            "  openautotyper --doctor\n"
        ),
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )

    source = parser.add_argument_group("text source (default: stdin)")
    source.add_argument("-f", "--file", type=Path, help="read the text from a file")
    source.add_argument("-t", "--text", help="the text to type, given literally")
    source.add_argument(
        "-c", "--clipboard", action="store_true", help="read the text from the clipboard"
    )

    pace = parser.add_argument_group("pace")
    pace.add_argument("--wpm", type=float, help="words per minute, end to end (default 70)")
    pace.add_argument(
        "-d", "--delay", type=float, metavar="SECS",
        help="countdown before typing starts, to focus the target window (default 5)",
    )

    shape = parser.add_argument_group("rhythm (shape only — none of these change the wpm)")
    shape.add_argument("--variance", type=float, help="interval spread, 0-2 (default 0.30)")
    shape.add_argument("--rhythm", type=float, help="finger-travel weighting, 0-1 (default 1.0)")
    shape.add_argument("--hesitation", type=float, help="fraction of keys with a hitch (default 0.03)")
    shape.add_argument("--sentence-pause", type=float, help="chance of pausing at a boundary, 0-1")
    shape.add_argument("--max-pause", type=float, metavar="SECS", help="longest boundary pause")
    shape.add_argument("--drift", type=float, help="slow speed wander, 0-1 (default 1.0)")
    shape.add_argument(
        "--typos", type=float, metavar="RATE",
        help="per-character slip rate, 0-0.25; always corrected (default 0)",
    )
    shape.add_argument(
        "--flat", action="store_true",
        help="perfect metronome: zeroes every rhythm knob above",
    )

    text_opts = parser.add_argument_group("text handling")
    text_opts.add_argument(
        "--no-fold", action="store_true",
        help="type smart quotes and dashes as-is instead of folding to ASCII",
    )
    text_opts.add_argument(
        "--expand-tabs", type=int, metavar="N",
        help="replace tabs with N spaces (use when Tab moves between fields)",
    )

    run = parser.add_argument_group("run")
    run.add_argument("--backend", help="force a backend (see --list-backends)")
    run.add_argument("--panic-key", default=None, metavar="KEY",
                     help="evdev key name that aborts, e.g. KEY_ESC (default) or KEY_F12")
    run.add_argument("--any-key-stops", action="store_true",
                     help="abort as soon as any key is pressed on a real keyboard")
    run.add_argument("--no-panic-key", action="store_true",
                     help="disable the abort key (not recommended)")
    run.add_argument("--seed", type=int, help="reproducible rhythm, for testing")
    run.add_argument("-n", "--dry-run", action="store_true",
                     help="print what would be typed and how long it would take")
    run.add_argument("-q", "--quiet", action="store_true", help="no progress output")

    info = parser.add_argument_group("information")
    info.add_argument("--list-backends", action="store_true", help="show every backend and why it can or cannot run")
    info.add_argument("--doctor", action="store_true", help="diagnose this machine's setup")
    info.add_argument("--gui", action="store_true", help="launch the graphical interface")
    info.add_argument("--version", action="version", version=f"openautotyper {__version__}")
    return parser


def read_source(args: argparse.Namespace, parser: argparse.ArgumentParser) -> str:
    given = [bool(args.file), bool(args.text), bool(args.clipboard)]
    if sum(given) > 1:
        parser.error("choose one of --file, --text or --clipboard")
    if args.text is not None:
        return args.text
    if args.file is not None:
        try:
            return args.file.read_text(encoding="utf-8")
        except OSError as exc:
            parser.error(f"cannot read {args.file}: {exc}")
    if args.clipboard:
        content = clipboard.read()
        if content is None:
            parser.error("could not read the clipboard: " + clipboard.available()[1])
        return content
    if sys.stdin.isatty():
        parser.error("no text given — use --file, --text, --clipboard, or pipe to stdin")
    return sys.stdin.read()


def apply_overrides(settings: Settings, args: argparse.Namespace) -> Cadence:
    if args.flat:
        settings.variance = settings.rhythm = settings.hesitation = 0.0
        settings.sentence_pause = settings.drift = 0.0
    for name, attr in (
        ("wpm", "wpm"), ("variance", "variance"), ("rhythm", "rhythm"),
        ("hesitation", "hesitation"), ("sentence_pause", "sentence_pause"),
        ("max_pause", "max_pause"), ("drift", "drift"), ("typos", "typo_rate"),
    ):
        value = getattr(args, name, None)
        if value is not None:
            setattr(settings, attr, value)
    return settings.cadence()


def print_backends() -> int:
    rows = report()
    width = max(len(name) for name, _, _ in rows)
    print(f"{'BACKEND'.ljust(width)}  USABLE  REASON")
    for name, ok, reason in rows:
        print(f"{name.ljust(width)}  {'yes   ' if ok else 'no    '}  {reason}")
    return 0


def doctor() -> int:
    """Explain what this machine can and cannot do, and how to fix it."""
    import os
    import platform

    print(f"openautotyper {__version__}  ·  python {platform.python_version()}"
          f"  ·  {platform.system()} {platform.release()}")
    session = os.environ.get("XDG_SESSION_TYPE", "unknown")
    desktop = os.environ.get("XDG_CURRENT_DESKTOP", "unknown")
    print(f"session: {session} ({desktop})")
    print(f"config:  {config_path()}"
          f"{'' if config_path().exists() else '  (not created yet)'}")

    print("\nbackends:")
    rows = report()
    for name, ok, reason in rows:
        print(f"  {'OK ' if ok else '-- '} {name:<8} {reason}")

    from .backends.xkb import load_layout
    layout = load_layout()
    print(f"\nkeyboard layout: {layout.source} — {len(layout)} characters reachable")

    if sys.platform.startswith("linux"):
        from .backends.identity import for_this_machine
        identity = for_this_machine()
        # Worth printing: this is the name the virtual keyboard appears under
        # in /proc/bus/input/devices, which anything on the machine can read.
        print(f"appears as:      {identity.name!r} — {identity.source}")

    from .panic import available as panic_available
    ok, reason = panic_available()
    print(f"abort key:       {'OK' if ok else 'UNAVAILABLE'} — {reason}")

    ok, reason = clipboard.available()
    print(f"clipboard:       {'OK' if ok else 'UNAVAILABLE'} — {reason}")

    usable = [name for name, ok, _ in rows if ok and name != "null"]
    if not usable:
        print("\nNo usable backend. On Linux this is almost always /dev/uinput "
              "permissions:\n"
              "  echo 'KERNEL==\"uinput\", GROUP=\"input\", MODE=\"0660\"' | "
              "sudo tee /etc/udev/rules.d/99-uinput.rules\n"
              "  sudo usermod -aG input \"$USER\"   # then log out and back in")
        return 1
    print(f"\nReady. Typing will use: {usable[0]}")
    return 0


class _ProgressLine:
    """One-line live status on stderr, redrawn in place."""

    def __init__(self, enabled: bool) -> None:
        self.enabled = enabled and sys.stderr.isatty()
        self._width = 0

    def __call__(self, progress: Progress) -> None:
        if not self.enabled:
            return
        if progress.state is State.COUNTDOWN:
            line = f"  {progress.message}"
        elif progress.state in (State.TYPING, State.PAUSED):
            done = int(progress.fraction * 24)
            bar = "#" * done + "." * (24 - done)
            tag = "paused " if progress.state is State.PAUSED else ""
            line = (f"  [{bar}] {progress.fraction * 100:3.0f}%  "
                    f"{progress.typed}/{progress.total}  {tag}"
                    f"{format_duration(progress.remaining)} left")
        else:
            line = ""
        self._draw(line)

    def _draw(self, line: str) -> None:
        pad = " " * max(0, self._width - len(line))
        sys.stderr.write("\r" + line + pad)
        sys.stderr.flush()
        self._width = len(line)

    def clear(self) -> None:
        if self.enabled and self._width:
            self._draw("")
            sys.stderr.write("\r")
            sys.stderr.flush()


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.gui:
        from .gui import run_gui

        return run_gui()
    if args.list_backends:
        return print_backends()
    if args.doctor:
        return doctor()

    settings = Settings.load()
    raw = read_source(args, parser)

    normalized = normalize(
        raw,
        fold_typography=not (args.no_fold or not settings.fold_typography),
        expand_tabs=args.expand_tabs if args.expand_tabs is not None else settings.expand_tabs,
        strip_trailing_ws=settings.strip_trailing_ws,
    )
    text = normalized.text
    if not text.strip():
        print("nothing to type: the input is empty", file=sys.stderr)
        return 2

    try:
        cadence = apply_overrides(settings, args)
        cadence.validate()
    except ValueError as exc:
        parser.error(str(exc))

    countdown = args.delay if args.delay is not None else settings.countdown

    try:
        backend = NullBackend() if args.dry_run else select(args.backend or settings.backend or None)
    except BackendError as exc:
        print(f"error: {exc}", file=sys.stderr)
        print("\nrun 'openautotyper --doctor' for a diagnosis", file=sys.stderr)
        return 3

    panic_key = None if args.no_panic_key else (args.panic_key or settings.panic_key)
    line = _ProgressLine(enabled=not args.quiet and not args.dry_run)

    session = TypeSession(
        text, cadence, backend,
        countdown=0.0 if args.dry_run else countdown,
        panic_key=None if args.dry_run else panic_key,
        any_key_stops=args.any_key_stops or settings.any_key_stops,
        on_progress=line,
        seed=args.seed,
        # A dry run is a preview. Pacing it would make it take exactly as long
        # as the typing it is previewing, which is the one thing nobody wants
        # from a preview.
        realtime=not args.dry_run,
    )

    chars, words, lines = stats(text)
    if not args.quiet:
        where = "would type" if args.dry_run else f"typing via {backend.name}"
        print(f"{where}: {chars:,} chars · {words:,} words · {lines:,} lines"
              f"  ·  ~{format_duration(session.estimated)} at {cadence.wpm:g} wpm",
              file=sys.stderr)
        if normalized.changed:
            print(f"  normalized: {normalized.summary()}", file=sys.stderr)
        if not args.dry_run and (panic_key or args.any_key_stops):
            # Only promise the abort key if it can actually be delivered. It
            # needs to read /dev/input/event*, which is a different permission
            # from the /dev/uinput that typing needs -- so a machine that types
            # perfectly may have no way to stop, and saying "press Esc" when
            # Esc does nothing is worse than saying nothing at all.
            from .panic import available as panic_available
            ok, why = panic_available()
            if ok:
                label = "any key" if args.any_key_stops else panic_key.removeprefix("KEY_")
                print(f"  press {label} on your keyboard to abort", file=sys.stderr)
            else:
                print(f"  WARNING: no abort key — {why}.\n"
                      f"           Ctrl+C in this terminal still works.",
                      file=sys.stderr)

    signal.signal(signal.SIGINT, lambda *_: session.stop("interrupted"))
    try:
        result = session.run()
    finally:
        line.clear()

    if not args.quiet and not result.panic_active:
        print("note: this ran with no abort key attached", file=sys.stderr)

    if args.dry_run:
        sys.stdout.write(backend.text)  # type: ignore[union-attr]
        if not args.quiet:
            print(f"\n-- dry run: {result.typed:,} characters, "
                  f"~{format_duration(session.estimated)}, "
                  f"text delivered exactly: {backend.text == text}",  # type: ignore[union-attr]
                  file=sys.stderr)
        return 0

    if result.state is State.FAILED:
        print(f"failed: {result.reason}", file=sys.stderr)
        return 3
    if result.state is State.STOPPED:
        print(f"{result.reason} after {result.typed:,} of {result.total:,} characters",
              file=sys.stderr)
        return 1

    if not args.quiet:
        rate = result.typed / 5.0 / (result.duration / 60.0) if result.duration else 0.0
        print(f"done: {result.typed:,} characters in {format_duration(result.duration)}"
              f" ({rate:.0f} wpm)", file=sys.stderr)
        if result.skipped:
            missing = "".join(dict.fromkeys(ch for _, ch in result.skipped))
            print(f"  warning: {len(result.skipped)} character(s) could not be typed"
                  f" on this layout: {missing!r}", file=sys.stderr)
        if result.slip > 0.5:
            print(f"  note: the schedule fell {result.slip:.1f}s behind — "
                  "the machine was busy", file=sys.stderr)
    return 0
