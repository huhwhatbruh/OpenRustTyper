# OpenAutoTyper 2.0

Types text into whatever window has focus, as real keystrokes, at a cadence you
control.

That is the entire job. There is no browser automation, no network access, no
model in the loop, and no telemetry. It reads text, it presses keys.

```bash
openautotyper -f essay.txt --wpm 80      # from a file
pbpaste | openautotyper --delay 3        # from a pipe
openautotyper --gui                      # or with a window
```

---

## Why 2.0 is a rewrite rather than a patch

v1.0 was built on `pyautogui`, which types through **XTEST** — an X11 protocol
extension. On a Wayland session XTEST still appears to work, because XWayland is
running, but the keystrokes only reach XWayland clients. Aim them at a native
Wayland window — KDE's own apps, Firefox or Chromium in Wayland mode, anything
Electron since 2023 — and they land nowhere at all.

That one fact accounts for most of what got reported as "laggy", "drops
characters" and "sometimes just does nothing". Wayland is the default session on
Fedora, Ubuntu, and every current KDE and GNOME spin, so for a growing share of
users v1 could not have worked.

v2 types through a **virtual keyboard on `/dev/uinput`** instead. The kernel
presents it as a real keyboard, so the compositor routes its events like any
other keyboard's — on Wayland, on X11, and in a plain TTY.

Run `openautotyper --doctor` and it will tell you exactly what your machine can
do and what to fix if something is missing.

### Also fixed along the way

| v1 | v2 |
|---|---|
| ETA was `characters × interval`, ignoring every pause — routinely 40% out | The WPM dial is calibrated: the estimate is exact by construction |
| `time.sleep()` in a loop, accumulating scheduler drift | Deadline-based pacing; measured within 0.5% over a full run |
| Stop meant `Process.terminate()`, which could leave Shift or Ctrl **physically held** | Cooperative stop, with an unconditional modifier release on every exit path |
| Abort was "fling the mouse into a screen corner", via the same X11 path that may be dead | A real abort key read from `/dev/input`, working regardless of focus |
| Gaussian delays, clamped — piling up at the clamp in a way no hand produces | Lognormal intervals, which is the distribution real keystrokes follow |
| One character at a time through the clipboard for anything non-ASCII, destroying what you had copied | Typographic characters fold to ASCII; the clipboard is a last resort, and it is restored |
| Hardcoded US QWERTY assumptions | The active XKB layout is compiled and queried, so non-US layouts type correctly |
| 830 lines in one file, no tests | A package with 133 tests |

---

## Install

Requires Python 3.11 or newer.

```bash
pip install openautotyper          # engine + CLI
pip install openautotyper[gui]     # ...and the graphical interface
```

### Arch Linux

A `PKGBUILD` is in `packaging/`. It installs the udev rule and the desktop
entry for you, so the only step left is the `input` group:

```bash
cd packaging && makepkg -si
sudo usermod -aG input "$USER"
```

### Linux: one-time permission setup

Creating a virtual keyboard needs write access to `/dev/uinput`, and the abort
key needs read access to `/dev/input/event*`. Both come with the `input` group.
The Arch package does the first half; everywhere else, do it by hand:

```bash
sudo cp packaging/99-openautotyper-uinput.rules /etc/udev/rules.d/
sudo udevadm control --reload-rules && sudo udevadm trigger
sudo usermod -aG input "$USER"
```

Log out and back in, then check with `openautotyper --doctor`.

No setup is needed on Windows or macOS — except that macOS requires
Accessibility permission for the terminal or app you launch from
(System Settings → Privacy & Security → Accessibility).

---

## Using it

```
openautotyper [source] [options]
```

The text comes from `--file`, `--text`, `--clipboard`, or standard input.

| Option | |
|---|---|
| `--wpm N` | Words per minute, measured end to end. Default 70. |
| `--delay SECS` | Countdown before typing starts, to focus the target window. Default 5. |
| `--dry-run` | Print what would be typed, and how long it would take. Types nothing. |
| `--flat` | Perfect metronome — zero every rhythm setting. |
| `--typos RATE` | Mistype and correct, 0 to 0.25. The finished text is identical. |
| `--panic-key KEY` | What aborts, e.g. `KEY_ESC` (default) or `KEY_F12`. |
| `--any-key-stops` | Abort the moment you touch the keyboard. |
| `--doctor` | Diagnose this machine's setup. |

`--variance`, `--rhythm`, `--hesitation`, `--sentence-pause`, `--max-pause` and
`--drift` change the character of the typing. None of them changes its speed;
see below.

Settings persist in `~/.config/openautotyper/config.toml`. Command-line flags
override the file for that run.

### Stopping

Press **Esc** on your keyboard. It is read directly from the input devices
rather than registered with the desktop, so it works no matter which window has
focus, and on Wayland, where applications cannot register global hotkeys at all.

`Ctrl+C` in the terminal and the Stop button in the GUI both work too. Whichever
you use, every modifier is released before the program lets go of the keyboard.

---

## The cadence model

Four things are true of people at keyboards, and the model implements each:

- **Intervals are lognormal.** The distribution has a hard floor, a dense body
  and a long right tail. Sampling a Gaussian and clamping it — v1's approach —
  produces a spike at the clamp that no hand makes.
- **Finger travel dominates rhythm.** Consecutive keys struck by the same finger
  are much slower than keys that alternate hands. This is the largest single
  source of texture in real typing, and it is why `ed` takes longer than `th`.
- **Speed wanders and reverts.** An Ornstein–Uhlenbeck walk drifts the baseline
  without letting it escape the rate you asked for. Independent draws give a
  twitch instead.
- **Pauses cluster at syntax.** After sentences, around paragraphs — not spread
  evenly across words.

**The WPM dial is honest.** Every setting above shapes the rhythm; none of them
moves the average. The finished schedule is scaled to the rate you asked for, so
70 wpm means 70 wpm end to end, pauses and corrections included — and the ETA is
therefore closed-form arithmetic rather than a guess. There is a test for it.

---

## Architecture

```
openautotyper/
  text.py          normalization: line endings, typography, zero-width characters
  timing.py        the cadence model — pure functions, no I/O
  clock.py         drift-corrected, interruptible pacing
  session.py       countdown, keystrokes, progress, guaranteed cleanup
  panic.py         the abort key, read straight from /dev/input
  config.py        settings, as TOML you can edit by hand
  cli.py           the command line
  backends/
    xkb.py         compiles the active keyboard layout into a keystroke table
    linux_uinput.py  virtual keyboard — Wayland, X11, TTY
    linux_x11.py     XTEST — X11 only, and it says so
    windows.py       SendInput with Unicode events
    macos.py         Quartz CGEvent
    null.py          types into a string, for --dry-run and the tests
  gui/             PySide6 interface
```

The engine, the CLI and every backend outside Linux run on the standard library
alone. Linux needs `evdev`; the GUI needs `PySide6`. Nothing else.

```bash
pip install -e ".[gui,dev]"
pytest
```

---

## Scope

This types text. It does not know what it is typing into, and it has no
awareness of any particular website or application.

If you are using it somewhere with rules about how work gets produced — a
course, an exam, an employer's systems — those rules are yours to follow, and a
tool cannot settle that for you.

---

## License

MIT. See [LICENSE](LICENSE).

Created by [huhwhatbruh](https://github.com/huhwhatbruh).
