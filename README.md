# OpenAutoTyper 1.0

Types text into whatever window has focus, as real keystrokes, at a cadence you
control. Linux only, and deliberately.

That is the entire job. There is no browser automation, no network access, no
model in the loop, and no telemetry. It reads text, it presses keys.

```bash
openautotyper -f essay.txt --wpm 80      # from a file
wl-paste | openautotyper --delay 3       # from a pipe
openautotyper-gui                        # or with a window
```

OpenAutoTyper 1.0 is written in Rust: a command-line tool and an
[egui](https://github.com/emilk/egui) window from one crate, with no
interpreter and no GUI toolkit to install. It carries on from the Python
releases that came before it — same settings file, same behaviour, same
cadence model — and their history is in git.

---

## How it types

It types through a **virtual keyboard on `/dev/uinput`**. The kernel presents it
as a real keyboard, so the compositor routes its events like any other
keyboard's — on Wayland, on X11, and in a plain TTY.

That matters because the usual alternative, **XTEST**, is an X11 protocol
extension. On a Wayland session it still appears to work, because XWayland is
running, but the keystrokes only reach XWayland clients. Aim them at a native
Wayland window — KDE's own apps, Firefox or Chromium in Wayland mode, anything
Electron since 2023 — and they land nowhere at all. That one fact accounts for
most of what gets reported as "laggy", "drops characters" and "sometimes just
does nothing". XTEST is still here as a fallback for X11 sessions where
`/dev/uinput` is not writable, and it refuses to run under Wayland at all,
where it would only look as though it worked.

Run `openautotyper --doctor` and it will tell you exactly what your machine can
do and what to fix if something is missing.

### Why it is Linux only

Windows sends `SendInput` events with `LLKHF_INJECTED` set on every low-level
hook event, and `KEYEVENTF_UNICODE` arrives as the virtual key `VK_PACKET`,
which a web page reads as `keyCode === 231` with an empty `code`. macOS and
Windows both mark injected events as synthetic and hand the flag to any
application that asks. There is no backend on those platforms that types as
indistinguishably as a uinput device does, so rather than make a claim on three
platforms that is only true on one, there is only the one.

### What you get

- **An honest estimate.** The ETA is the sum of the schedule that will actually
  be typed — pauses and corrections included — not characters × interval.
- **Pacing that does not drift.** Deadline-based, so a long run lands on its
  estimate, and a stop interrupts a long pause instead of waiting it out.
- **A stop that always lets go.** Cooperative, with an unconditional release of
  every key and modifier on every exit path. A stuck Shift or Ctrl outlives the
  program and wrecks the session, so this is the property tested hardest.
- **An abort key that works anywhere.** Read from `/dev/input`, so it works no
  matter which window has focus, and on Wayland, where applications cannot
  register global hotkeys at all.
- **Your layout, not a US one.** The active XKB layout is compiled and queried,
  so non-US layouts type correctly.
- **Your clipboard back.** Typographic characters fold to ASCII; the clipboard
  is a last resort for characters no key can produce, and it is restored.

---

## Install

Rust 1.92 or newer builds it.

```bash
cargo install --path .                        # both programs
cargo install --path . --no-default-features  # command line only, no window
```

The window needs no toolkit installed on your machine: egui draws it, on
Wayland or X11, using the OpenGL your desktop already has. The only runtime
libraries are the ones every desktop provides (`libxkbcommon`, `libwayland`,
`libX11`), and they are loaded when the program starts rather than linked, so a
machine missing one gets an explanation instead of a crash.

The window's **Open file…** button uses your desktop's file-chooser portal
(`xdg-desktop-portal`); everything else works without it.

### Arch Linux

The `PKGBUILD` in `packaging/` builds the tagged release. It installs both
programs, the udev rule, the icon and the desktop entry, so the only step left
is the `input` group:

```bash
cd packaging && makepkg -si
sudo usermod -aG input "$USER"
```

To build a package from the commit you have checked out, rather than from a
published tag:

```bash
packaging/build-local.sh          # writes dist/openautotyper-*.pkg.tar.zst
sudo pacman -U dist/openautotyper-*.pkg.tar.zst
```

`dist/` is generated build output and is not committed.

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
| `--backend NAME` | Force `uinput` or `x11` instead of the automatic choice. |
| `--doctor` | Diagnose this machine's setup. |
| `--list-backends` | Every backend, and why it can or cannot run. |

`--variance`, `--rhythm`, `--hesitation`, `--sentence-pause`, `--max-pause` and
`--drift` change the character of the typing. None of them changes its speed;
see below.

Settings persist in `~/.config/openautotyper/config.toml`. Command-line flags
override the file for that run. Keys the program does not recognise are kept
when it saves, so a setting added by hand, or by a newer release, survives.

### The window

`openautotyper-gui` (or `openautotyper --gui`) is the same engine with a
window: paste or open text, pick a preset — **Careful**, **Natural**, **Brisk**
or **Machine** — or open *Rhythm and safety* for every dial, then **Start
typing** (`Ctrl+Enter`). **Pause**, **Stop** and `Esc` work while it types, and
if the abort key cannot be read the window says so rather than promising one.

If it cannot open at all it says why in a dialog, because a launcher started
from a menu has nowhere to print an error.

### Stopping

Press **Esc** on your keyboard. `Ctrl+C` in the terminal and the Stop button in
the window both work too. Whichever you use, every modifier is released before
the program lets go of the keyboard.

---

## The cadence model

Things that are true of people at keyboards, each of which the model implements:

- **Intervals are lognormal.** The distribution has a hard floor, a dense body
  and a long right tail. Sampling a Gaussian and clamping it produces a spike
  at the clamp that no hand makes.
- **Finger travel dominates rhythm.** Consecutive keys struck by the same finger
  are much slower than keys that alternate hands. This is the largest single
  source of texture in real typing, and it is why `ed` takes longer than `th`.
- **Speed wanders and reverts.** An Ornstein–Uhlenbeck walk drifts the baseline
  without letting it escape the rate you asked for. Independent draws give a
  twitch instead.
- **Pauses cluster at syntax.** After sentences, around paragraphs — not spread
  evenly across words.
- **Hold time is half of keystroke timing.** Every keydown a page receives is
  paired with a keyup, and how long the key was held is as characteristic as
  the gap between keys. The mean falls as speed rises, the way real hands do:
  roughly 110 ms at 30 wpm, 95 at 70, 70 at 120. Holds are capped well under
  the kernel's autorepeat delay, so a long hold can never type a character the
  text did not contain.
- **Keys overlap.** The next key often goes down before the last comes up. One
  finger is never on two keys, and no key is pressed while it is already down.
- **Shift is a keystroke.** A capital is two presses, and the first is the one
  that gives a typer away: pressed with the letter it modifies, Shift and the
  letter arrive in the same millisecond, which no hand does. Here it leads by
  a mean of about 75 ms and lingers about 50 ms past the letter's own release,
  and a run of capitals holds one continuous Shift.
- **Typing warms up and tires.** The pace starts a few percent below its settled
  rate, finds it over a paragraph or two, and gives some of it back over the
  first few pages.

**The WPM dial is honest, to within a few percent.** Every setting above shapes
the rhythm and none of them moves the average, so 70 wpm means 70 wpm end to
end, pauses and corrections included. It is not exact, on purpose: normalising
each schedule to the requested rate would make the total duration a function of
length and dial alone — identical to the microsecond on every run of one
document, which is a fingerprint across runs even though the rhythm inside
differed every time. So each run draws its own pace for the day, and the
average over many runs is the rate you asked for. `--flat` gives an exact
metronome.

---

## What this does and does not hide

Typing through `/dev/uinput` means the kernel presents the device as a real
keyboard, so the compositor routes it like one. That is worth nothing if the
device then announces itself, and `/proc/bus/input/devices` is world-readable. A
freshly created uinput device announces itself in four ways:

```
N: Name="OpenAutoTyper Virtual Keyboard"
P: Phys=
B: EV=200003
```

against a keyboard that is actually plugged in:

```
N: Name="HID 0d62:3740 Keyboard"
P: Phys=usb-0000:00:14.0-10/input1
B: EV=12001f
B: MSC=10
B: LED=7
```

The name is the loudest giveaway, but an empty `Phys` and a capability bitmap
that describes a keyboard with force feedback, no Caps Lock light, no autorepeat
and no scancodes are a device that has never existed. So the identity is modelled
on a keyboard the machine genuinely has — same name and ids, a neighbouring USB
port — the capabilities are a keyboard's, and every key event is preceded by the
`MSC_SCAN` that real hardware sends. A test creates the device and reads the
kernel's own listing back to check it.

**What it does not hide.** The kernel puts every uinput device under
`/sys/devices/virtual/input/`, and nothing a userspace process passes to
`UI_DEV_SETUP` changes that. Anything willing to follow the sysfs path knows
this device is virtual. 

---

## Architecture

```
src/
  text.rs          normalization: line endings, typography, control characters
  timing.rs        the cadence model — pure functions, no I/O
  clock.rs         drift-corrected, interruptible pacing
  session.rs       countdown, keystrokes, progress, guaranteed cleanup
  panic.rs         the abort key, read straight from /dev/input
  config.rs        settings, as TOML you can edit by hand
  clipboard.rs     the last-resort paste, and putting back what it replaced
  main.rs          the command line
  gui.rs           the egui window
  bin/gui.rs       the launcher the desktop entry starts
  backend/
    xkb.rs         compiles the active keyboard layout into a keystroke table
    identity.rs    what the virtual keyboard looks like to anything inspecting it
    uinput.rs      virtual keyboard — Wayland, X11, TTY
    x11.rs         XTEST — X11 only, and it says so
    null.rs        types into a string, for --dry-run and the tests
    codes.rs       evdev key codes and the HID scancodes that go with them
tests/
  cli.rs           the command line, as subprocesses
  device.rs        real /dev/uinput devices; skips itself without the permission
  packaging.rs     the desktop entry and the Arch package agree with each other
```

The engine, the cadence model and the command line know nothing about egui:
build with `--no-default-features` and the window, and everything it pulls in,
is gone. `libxkbcommon` and the X libraries are opened at run time rather than
linked, so a minimal machine still gets the built-in US layout.

```bash
cargo test
cargo clippy --all-targets -- -D warnings
```

There are over 200 tests. The timing ones are statistical — they check that the
delivered rate, the hold times, the rollover and the Shift lead behave like
typing, not just that the code runs. The device tests that read keystrokes back
off `/dev/input` need the `input` group *and* grab the virtual keyboard first,
so a test can never type into whatever window you have open. Without the
permission they skip, and say so.

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
