//! The graphical interface.
//!
//! Two rules shape this file, the same two that shaped the Qt one it replaces.
//! Progress from the typing thread arrives as a message, never as a direct
//! mutation of UI state, because touching the interface from another thread is
//! how a toolkit ends up crashing minutes into a long job. And nothing
//! expensive runs on a keystroke: the estimate is closed-form arithmetic, and
//! the schedule -- a quarter of a second of work for a novel-length document --
//! is built on the typing thread when you press Start, so the window never
//! freezes while it is made.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align, Button, Color32, CornerRadius, FontId, Frame, Key, Layout, Margin, Modifiers,
    ProgressBar, RichText, ScrollArea, Slider, Stroke, TextEdit, TextStyle, Theme, Vec2,
};

use crate::backend;
use crate::clipboard;
use crate::config::Settings;
use crate::panic as abort_key;
use crate::session::{Control, Progress, RunResult, SessionOptions, State, TypeSession};
use crate::text::{normalize, stats, NormalizeOptions};
use crate::timing::format_duration;
use crate::VERSION;

// ---------------------------------------------------------------------------
// Theme
// ---------------------------------------------------------------------------

/// Colour tokens, resolved once against the system theme.
struct Palette {
    bg: Color32,
    surface: Color32,
    surface_alt: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    faint: Color32,
    accent: Color32,
    accent_hover: Color32,
    accent_text: Color32,
    danger: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

const DARK: Palette = Palette {
    bg: hex(0x16161e),
    surface: hex(0x1d1d26),
    surface_alt: hex(0x24242f),
    border: hex(0x33333f),
    text: hex(0xe4e4ef),
    muted: hex(0x9a9ab0),
    faint: hex(0x6a6a80),
    accent: hex(0x7aa2f7),
    accent_hover: hex(0x8fb3ff),
    accent_text: hex(0x0d1117),
    danger: hex(0xf7768e),
};

const LIGHT: Palette = Palette {
    bg: hex(0xf6f6f8),
    surface: hex(0xffffff),
    surface_alt: hex(0xeeeef2),
    border: hex(0xd8d8e0),
    text: hex(0x1c1c24),
    muted: hex(0x5c5c70),
    faint: hex(0x8a8a9c),
    accent: hex(0x3b5fd4),
    accent_hover: hex(0x2f4fbb),
    accent_text: hex(0xffffff),
    danger: hex(0xc4384f),
};

fn palette(dark: bool) -> &'static Palette {
    if dark {
        &DARK
    } else {
        &LIGHT
    }
}

/// One style per theme; egui picks between them as the system theme changes.
fn install_theme(ctx: &egui::Context) {
    for (theme, p) in [(Theme::Dark, &DARK), (Theme::Light, &LIGHT)] {
        ctx.style_mut_of(theme, |style| {
            style
                .text_styles
                .insert(TextStyle::Body, FontId::proportional(13.0));
            style
                .text_styles
                .insert(TextStyle::Button, FontId::proportional(13.0));
            style
                .text_styles
                .insert(TextStyle::Small, FontId::proportional(12.0));
            style
                .text_styles
                .insert(TextStyle::Monospace, FontId::monospace(13.0));
            style
                .text_styles
                .insert(TextStyle::Heading, FontId::proportional(19.0));
            style.spacing.item_spacing = Vec2::new(8.0, 8.0);
            style.spacing.button_padding = Vec2::new(14.0, 7.0);
            style.spacing.interact_size.y = 22.0;

            let v = &mut style.visuals;
            v.panel_fill = p.bg;
            v.window_fill = p.surface;
            v.window_stroke = Stroke::new(1.0, p.border);
            v.window_corner_radius = CornerRadius::same(10);
            v.extreme_bg_color = p.surface;
            v.faint_bg_color = p.surface_alt;
            v.selection.bg_fill = p.accent;
            v.selection.stroke = Stroke::new(1.0, p.accent_text);
            v.hyperlink_color = p.accent;
            v.text_cursor.stroke = Stroke::new(2.0, p.accent);

            let radius = CornerRadius::same(8);
            let w = &mut v.widgets;
            w.noninteractive.bg_fill = p.surface;
            w.noninteractive.weak_bg_fill = p.surface;
            w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
            w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
            for state in [&mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
                state.corner_radius = radius;
                state.weak_bg_fill = p.surface_alt;
                state.bg_fill = p.surface_alt;
                state.bg_stroke = Stroke::new(1.0, p.border);
                state.fg_stroke = Stroke::new(1.0, p.text);
            }
            w.noninteractive.corner_radius = radius;
            w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
            w.active.bg_stroke = Stroke::new(1.0, p.accent);
            w.hovered.fg_stroke = Stroke::new(1.0, p.text);
        });
    }
}

// ---------------------------------------------------------------------------
// Presets and choices
// ---------------------------------------------------------------------------

/// A named speed setting. Each is a full cadence, not just a WPM.
struct Preset {
    name: &'static str,
    wpm: f64,
    variance: f64,
    rhythm: f64,
    hesitation: f64,
    sentence_pause: f64,
    max_pause: f64,
    drift: f64,
}

#[rustfmt::skip]
const PRESETS: [Preset; 4] = [
    Preset { name: "Careful", wpm: 45.0, variance: 0.34, rhythm: 1.0, hesitation: 0.05, sentence_pause: 0.75, max_pause: 2.6, drift: 1.0 },
    Preset { name: "Natural", wpm: 70.0, variance: 0.30, rhythm: 1.0, hesitation: 0.03, sentence_pause: 0.55, max_pause: 1.8, drift: 1.0 },
    Preset { name: "Brisk", wpm: 110.0, variance: 0.26, rhythm: 0.9, hesitation: 0.02, sentence_pause: 0.35, max_pause: 1.1, drift: 0.8 },
    Preset { name: "Machine", wpm: 200.0, variance: 0.0, rhythm: 0.0, hesitation: 0.0, sentence_pause: 0.0, max_pause: 0.0, drift: 0.0 },
];

const PANIC_KEYS: [(&str, &str); 5] = [
    ("Esc", "KEY_ESC"),
    ("F12", "KEY_F12"),
    ("F9", "KEY_F9"),
    ("Scroll Lock", "KEY_SCROLLLOCK"),
    ("Pause", "KEY_PAUSE"),
];

impl Preset {
    fn matches(&self, s: &Settings) -> bool {
        (s.wpm - self.wpm).abs() < 0.5
            && (s.variance - self.variance).abs() < 0.01
            && (s.rhythm - self.rhythm).abs() < 0.01
            && (s.sentence_pause - self.sentence_pause).abs() < 0.01
    }

    fn apply(&self, s: &mut Settings) {
        s.wpm = self.wpm;
        s.variance = self.variance;
        s.rhythm = self.rhythm;
        s.hesitation = self.hesitation;
        s.sentence_pause = self.sentence_pause;
        s.max_pause = self.max_pause;
        s.drift = self.drift;
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// What the typing and file threads tell the window.
enum Msg {
    /// The session exists and can be steered.
    Started(Control),
    Progress(Progress),
    Finished(RunResult),
    /// The session could not even be built.
    Failed(String),
    Loaded(Result<String, String>),
}

/// A modal message the user must dismiss.
struct Notice {
    title: String,
    body: String,
}

struct Run {
    control: Option<Control>,
    /// Stop was pressed before the worker had a session to stop.
    stop_pending: Option<String>,
    worker: Option<JoinHandle<()>>,
}

/// How long to wait after the last keystroke before recounting the text. Counting
/// words means splitting the whole document, so it waits for a gap in typing.
const META_DEBOUNCE: Duration = Duration::from_millis(180);

pub struct App {
    settings: Settings,
    text: String,

    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    run: Option<Run>,

    meta: String,
    meta_due: Option<Instant>,
    backend_chip: (String, String),
    abort_ok: (bool, String),

    advanced: bool,
    notice: Option<Notice>,
    diagnostics: Option<String>,
    confirm_quit: bool,
    quitting: bool,

    status: String,
    fraction: f32,
    paused: bool,
    backends: Vec<(&'static str, bool, String)>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_theme(&cc.egui_ctx);
        Self::with_settings(Settings::load())
    }

    fn with_settings(settings: Settings) -> Self {
        let (tx, rx) = channel();
        let mut app = Self {
            settings,
            text: String::new(),
            tx,
            rx,
            run: None,
            meta: String::new(),
            meta_due: None,
            backend_chip: (String::new(), String::new()),
            abort_ok: abort_key::available(),
            advanced: false,
            notice: None,
            diagnostics: None,
            confirm_quit: false,
            quitting: false,
            status: String::new(),
            fraction: 0.0,
            paused: false,
            backends: backend::report(),
        };
        app.refresh_meta();
        app.refresh_backend_chip();
        app.refresh_hint();
        app
    }

    // -- derived state -----------------------------------------------------
    fn normalized_text(&self) -> String {
        normalize(
            &self.text,
            NormalizeOptions {
                fold_typography: self.settings.fold_typography,
                expand_tabs: self.settings.expand_tabs,
                strip_trailing_ws: self.settings.strip_trailing_ws,
            },
        )
        .text
    }

    fn refresh_meta(&mut self) {
        let text = self.normalized_text();
        let (chars, words, lines) = stats(&text);
        // The cadence model normalizes to exactly this, so the estimate is
        // arithmetic rather than a simulation -- no planning on a keystroke.
        let seconds = chars as f64 / 5.0 * 60.0 / self.settings.wpm.max(1.0);
        self.meta = format!(
            "{} chars \u{b7} {} words \u{b7} {} lines \u{b7} ~{}",
            group(chars),
            group(words),
            group(lines),
            format_duration(seconds)
        );
        self.meta_due = None;
    }

    fn refresh_backend_chip(&mut self) {
        let wanted = (!self.settings.backend.is_empty()).then_some(self.settings.backend.as_str());
        self.backend_chip = match backend::select(wanted) {
            Ok(b) => (
                format!("via {}", b.name()),
                "Keystrokes are delivered through this backend".to_string(),
            ),
            Err(e) => ("no backend".to_string(), e.to_string()),
        };
    }

    /// Only promise the abort key if it can be delivered. Reading
    /// /dev/input/event* is a different permission from writing /dev/uinput, so
    /// this window can be perfectly able to type and have no way to stop -- and
    /// "Press Esc to stop" when Esc does nothing is worse than saying nothing.
    fn refresh_hint(&mut self) {
        let (ok, why) = &self.abort_ok;
        self.status = if !ok {
            format!("No abort key \u{2014} {why}. Use the Stop button.")
        } else if self.settings.any_key_stops {
            "Typing stops the moment you touch the keyboard.".into()
        } else {
            let label = PANIC_KEYS
                .iter()
                .find(|(_, k)| *k == self.settings.panic_key)
                .map_or("Esc", |(l, _)| l);
            format!("Press {label} on your keyboard at any time to stop.")
        };
    }

    fn running(&self) -> bool {
        self.run.is_some()
    }

    fn warn(&mut self, title: &str, body: impl Into<String>) {
        self.notice = Some(Notice {
            title: title.into(),
            body: body.into(),
        });
    }

    // -- running -----------------------------------------------------------
    fn start(&mut self, ctx: &egui::Context) {
        if self.running() {
            return;
        }
        let text = self.normalized_text();
        if text.trim().is_empty() {
            self.warn("Nothing to type", "Put some text in the box first.");
            return;
        }

        let settings = self.settings.clone();
        let cadence = settings.cadence();
        if let Err(e) = cadence.validate() {
            self.warn("Cannot start", e);
            return;
        }
        let device = match backend::select(Some(&settings.backend)) {
            Ok(b) => b,
            Err(e) => {
                self.warn("Cannot start", e.to_string());
                return;
            }
        };

        self.fraction = 0.0;
        self.paused = false;
        let (tx, repaint) = (self.tx.clone(), ctx.clone());
        let worker = std::thread::Builder::new()
            .name("typing".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let progress_ctx = repaint.clone();
                let built = TypeSession::new(
                    &text,
                    &cadence,
                    device,
                    SessionOptions {
                        countdown: settings.countdown,
                        panic_key: Some(settings.panic_key.clone()),
                        any_key_stops: settings.any_key_stops,
                        on_progress: Some(Box::new(move |p| {
                            let _ = progress_tx.send(Msg::Progress(p));
                            progress_ctx.request_repaint();
                        })),
                        ..Default::default()
                    },
                );
                match built {
                    Ok(mut session) => {
                        let _ = tx.send(Msg::Started(session.control()));
                        repaint.request_repaint();
                        let result = session.run();
                        let _ = tx.send(Msg::Finished(result));
                    }
                    Err(e) => {
                        let _ = tx.send(Msg::Failed(e));
                    }
                }
                repaint.request_repaint();
            })
            .expect("spawn typing thread");
        self.run = Some(Run {
            control: None,
            stop_pending: None,
            worker: Some(worker),
        });
    }

    fn stop(&mut self, reason: &str) {
        if let Some(run) = &mut self.run {
            match &run.control {
                Some(c) => c.stop(reason),
                None => run.stop_pending = Some(reason.to_string()),
            }
        }
    }

    fn toggle_pause(&mut self) {
        if let Some(c) = self.run.as_ref().and_then(|r| r.control.as_ref()) {
            if c.paused() {
                c.resume();
            } else {
                c.pause();
            }
        }
    }

    fn drain(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Started(control) => {
                    if let Some(run) = &mut self.run {
                        if let Some(reason) = run.stop_pending.take() {
                            control.stop(&reason);
                        }
                        run.control = Some(control);
                    }
                }
                Msg::Progress(p) => self.on_progress(&p),
                Msg::Finished(result) => self.on_finished(result),
                Msg::Failed(e) => {
                    self.end_run();
                    self.status = "Failed".into();
                    self.warn("Cannot start", e);
                }
                Msg::Loaded(Ok(text)) => {
                    self.text = text;
                    self.refresh_meta();
                }
                Msg::Loaded(Err(e)) => self.warn("Could not open file", e),
            }
            ctx.request_repaint();
        }
    }

    fn on_progress(&mut self, p: &Progress) {
        match p.state {
            State::Countdown => {
                self.status = p.message.clone();
                self.fraction = 0.0;
            }
            State::Typing | State::Paused => {
                self.fraction = p.fraction() as f32;
                self.paused = p.state == State::Paused;
                let tag = if self.paused { "Paused \u{2014} " } else { "" };
                self.status = format!(
                    "{tag}{} of {} \u{b7} {} left \u{b7} {:.0} wpm",
                    group(p.typed),
                    group(p.total),
                    format_duration(p.remaining),
                    p.wpm()
                );
            }
            // Terminal states arrive as `Finished`, which carries the detail.
            _ => {}
        }
    }

    fn on_finished(&mut self, r: RunResult) {
        self.end_run();
        match r.state {
            State::Done => {
                self.fraction = 1.0;
                self.status = format!(
                    "Done \u{2014} {} characters in {}",
                    group(r.typed),
                    format_duration(r.duration)
                );
                if !r.skipped.is_empty() {
                    let mut missing = String::new();
                    for (_, ch) in &r.skipped {
                        if !missing.contains(ch.as_str()) {
                            missing.push_str(ch);
                        }
                    }
                    self.status.push_str(&format!(
                        " \u{b7} {} could not be typed on this layout: {missing}",
                        r.skipped.len()
                    ));
                }
            }
            State::Stopped => {
                self.status = format!(
                    "{} \u{2014} {} of {} characters typed",
                    capitalize(&r.reason),
                    group(r.typed),
                    group(r.total)
                );
            }
            _ => {
                self.fraction = 0.0;
                self.status = "Failed".into();
                self.warn("Typing failed", r.reason);
            }
        }
    }

    fn end_run(&mut self) {
        if let Some(mut run) = self.run.take() {
            if let Some(worker) = run.worker.take() {
                let _ = worker.join();
            }
        }
        self.paused = false;
    }

    fn load_file(&mut self, ctx: &egui::Context) {
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let start = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let picked = rfd::FileDialog::new()
                .set_title("Open text file")
                .set_directory(start)
                .add_filter("Text files", &["txt", "md", "py", "json", "csv"])
                .add_filter("All files", &["*"])
                .pick_file();
            if let Some(path) = picked {
                let loaded = std::fs::read_to_string(&path).map_err(|e| e.to_string());
                let _ = tx.send(Msg::Loaded(loaded));
                ctx.request_repaint();
            }
        });
    }

    fn paste_clipboard(&mut self) {
        match clipboard::read() {
            Some(content) => {
                self.text = content;
                self.refresh_meta();
            }
            None => self.warn("Clipboard unavailable", clipboard::available().1),
        }
    }

    fn show_diagnostics(&mut self) {
        let mut lines = vec!["Backends:".to_string()];
        for (name, ok, reason) in backend::report() {
            lines.push(format!(
                "  {}  {name}: {reason}",
                if ok { "OK" } else { "--" }
            ));
        }
        let layout = backend::xkb::load_layout();
        lines.push(format!("\nKeyboard layout: {}", layout.source));
        lines.push(format!(
            "  {} characters reachable without the clipboard",
            layout.len()
        ));
        let (ok, reason) = abort_key::available();
        lines.push(format!(
            "\nAbort key: {} \u{2014} {reason}",
            if ok { "OK" } else { "unavailable" }
        ));
        let (ok, reason) = clipboard::available();
        lines.push(format!(
            "Clipboard: {} \u{2014} {reason}",
            if ok { "OK" } else { "unavailable" }
        ));
        self.diagnostics = Some(lines.join("\n"));
    }

    // -- panels ------------------------------------------------------------
    fn header(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            ui.label(RichText::new("OpenAutoTyper").size(19.0).strong());
            ui.label(
                RichText::new(format!("v{VERSION}"))
                    .size(12.0)
                    .color(p.faint),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if link(ui, "Diagnostics", p).clicked() {
                    self.show_diagnostics();
                }
                Frame::new()
                    .fill(p.surface_alt)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(CornerRadius::same(11))
                    .inner_margin(Margin::symmetric(10, 3))
                    .show(ui, |ui| {
                        let r = ui.label(RichText::new(&self.backend_chip.0).color(p.muted));
                        r.on_hover_text(&self.backend_chip.1);
                    });
            });
        });
    }

    fn editor(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let running = self.running();
        let height = ui.available_height();
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(6))
            .show(ui, |ui| {
                ScrollArea::vertical().auto_shrink([false, false]).max_height(height - 16.0).show(ui, |ui| {
                    let response = ui.add_sized(
                        [ui.available_width(), (height - 28.0).max(80.0)],
                        TextEdit::multiline(&mut self.text)
                            .hint_text("Paste or type the text here, focus the target window, then press Start.")
                            .font(TextStyle::Monospace)
                            .frame(Frame::NONE)
                            .lock_focus(true)
                            .interactive(!running),
                    );
                    if response.changed() {
                        self.meta_due = Some(Instant::now() + META_DEBOUNCE);
                    }
                });
            });
    }

    fn links(&mut self, ui: &mut egui::Ui, p: &Palette, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if link(ui, "Open file\u{2026}", p).clicked() {
                self.load_file(ctx);
            }
            if link(ui, "From clipboard", p).clicked() {
                self.paste_clipboard();
            }
            if link(ui, "Clear", p).clicked() {
                self.text.clear();
                self.refresh_meta();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(&self.meta).size(12.0).color(p.muted));
            });
        });
    }

    fn controls(&mut self, ui: &mut egui::Ui, p: &Palette) {
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                let mut retime = false;

                // -- speed -----------------------------------------------
                ui.horizontal(|ui| {
                    ui.label("Speed");
                    let mut slider = self.settings.wpm.clamp(10.0, 300.0);
                    ui.spacing_mut().slider_width = (ui.available_width() - 270.0).max(120.0);
                    let r = ui
                        .add(
                            Slider::new(&mut slider, 10.0..=300.0)
                                .step_by(1.0)
                                .show_value(false),
                        )
                        .on_hover_text("Words per minute, measured end to end");
                    if r.changed() {
                        self.settings.wpm = slider;
                        retime = true;
                    }
                    let r = ui.add_sized(
                        [92.0, 22.0],
                        egui::DragValue::new(&mut self.settings.wpm)
                            .range(1.0..=2000.0)
                            .speed(1.0)
                            .max_decimals(0)
                            .suffix(" wpm"),
                    );
                    if r.changed() {
                        self.settings.wpm = self.settings.wpm.round();
                        retime = true;
                    }
                    ui.add_space(12.0);
                    ui.label("Start delay");
                    ui.add_sized(
                        [80.0, 22.0],
                        egui::DragValue::new(&mut self.settings.countdown)
                            .range(0.0..=120.0)
                            .speed(0.5)
                            .fixed_decimals(1)
                            .suffix(" s"),
                    )
                    .on_hover_text("Time to click into the target window before typing begins");
                });

                // -- presets ---------------------------------------------
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label("Preset");
                    for preset in &PRESETS {
                        let active = preset.matches(&self.settings);
                        let button = Button::new(RichText::new(preset.name).size(12.0))
                            .selected(active)
                            .min_size(Vec2::new(0.0, 0.0));
                        if ui.add(button).clicked() {
                            preset.apply(&mut self.settings);
                            retime = true;
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let arrow = if self.advanced {
                            "\u{25be}"
                        } else {
                            "\u{25b8}"
                        };
                        if link(ui, &format!("Rhythm and safety  {arrow}"), p).clicked() {
                            self.advanced = !self.advanced;
                        }
                    });
                });

                if retime {
                    self.refresh_meta();
                }
                if self.advanced {
                    ui.add_space(4.0);
                    self.advanced_controls(ui, p);
                }
            });
    }

    /// The rhythm sliders and the safety and text options. Each control
    /// refreshes only what it affects: the text options change the estimate,
    /// the abort options change what the hint promises, the backend picker
    /// changes the chip.
    fn advanced_controls(&mut self, ui: &mut egui::Ui, p: &Palette) {
        let (mut recount, mut rehint, mut rechip) = (false, false, false);
        ui.label(
            RichText::new(
                "These change the character of the typing, not its speed \u{2014} the \
                 words-per-minute above is delivered either way.",
            )
            .size(12.0)
            .color(p.faint),
        );

        let s = &mut self.settings;
        egui::Grid::new("rhythm")
            .num_columns(6)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                let pct = |v: f64| format!("{:.0}%", v * 100.0);
                let secs = |v: f64| format!("{v:.1}s");

                slider_row(
                    ui,
                    p,
                    "Variation",
                    &mut s.variance,
                    0.0..=1.0,
                    0.01,
                    "Spread of the gaps between keystrokes",
                    pct,
                );
                slider_row(
                    ui,
                    p,
                    "Pauses",
                    &mut s.sentence_pause,
                    0.0..=1.0,
                    0.01,
                    "Chance of pausing at the end of a sentence or paragraph",
                    pct,
                );
                ui.end_row();
                slider_row(
                    ui,
                    p,
                    "Finger travel",
                    &mut s.rhythm,
                    0.0..=1.0,
                    0.01,
                    "How much awkward letter pairs slow the typing down",
                    pct,
                );
                slider_row(
                    ui,
                    p,
                    "Longest pause",
                    &mut s.max_pause,
                    0.0..=6.0,
                    0.1,
                    "Upper bound on a single pause",
                    secs,
                );
                ui.end_row();
                slider_row(
                    ui,
                    p,
                    "Hesitation",
                    &mut s.hesitation,
                    0.0..=0.20,
                    0.01,
                    "Share of keystrokes preceded by a visible hitch",
                    pct,
                );
                slider_row(
                    ui,
                    p,
                    "Speed drift",
                    &mut s.drift,
                    0.0..=1.0,
                    0.01,
                    "How much the pace wanders over long passages",
                    pct,
                );
                ui.end_row();
                slider_row(
                    ui,
                    p,
                    "Slips",
                    &mut s.typo_rate,
                    0.0..=0.10,
                    0.001,
                    "Mistyped keys, always corrected with a backspace. The finished text is \
                 identical either way.",
                    |v| format!("{:.1}%", v * 100.0),
                );
                ui.end_row();
            });

        // -- safety and delivery -----------------------------------------
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            ui.label("Abort key");
            let current = PANIC_KEYS
                .iter()
                .find(|(_, k)| *k == s.panic_key)
                .map_or(PANIC_KEYS[0].0, |(l, _)| *l);
            egui::ComboBox::from_id_salt("panic")
                .selected_text(current)
                .show_ui(ui, |ui| {
                    for (label, key) in PANIC_KEYS {
                        if ui.selectable_label(s.panic_key == key, label).clicked() {
                            s.panic_key = key.to_string();
                            rehint = true;
                        }
                    }
                })
                .response
                .on_hover_text(
                    "Pressing this on a real keyboard stops typing immediately, whatever \
                     window has focus",
                );
            rehint |= ui
                .checkbox(&mut s.any_key_stops, "or any key at all")
                .on_hover_text("Stop the moment you touch the keyboard")
                .changed();

            ui.add_space(16.0);
            ui.label("Backend");
            let shown = if s.backend.is_empty() {
                "Automatic"
            } else {
                s.backend.as_str()
            };
            egui::ComboBox::from_id_salt("backend")
                .selected_text(shown)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(s.backend.is_empty(), "Automatic")
                        .clicked()
                    {
                        s.backend.clear();
                        rechip = true;
                    }
                    for (name, ok, reason) in &self.backends {
                        if *name == "null" {
                            continue;
                        }
                        let label = format!("{name}{}", if *ok { "" } else { "  (unavailable)" });
                        let r = ui.add_enabled(*ok, Button::selectable(s.backend == *name, label));
                        if r.on_hover_text(reason)
                            .on_disabled_hover_text(reason)
                            .clicked()
                        {
                            s.backend = (*name).to_string();
                            rechip = true;
                        }
                    }
                });
        });

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            recount |= ui
                .checkbox(
                    &mut s.fold_typography,
                    "Fold smart quotes and dashes to ASCII",
                )
                .on_hover_text(
                    "Curly quotes, em dashes and non-breaking spaces have no key on a \
                     keyboard. Folding them types what a person would have typed; unfolding \
                     sends them through the clipboard instead.",
                )
                .changed();
            ui.add_space(12.0);
            ui.label("Tabs as");
            let mut tabs = s.expand_tabs as u32;
            let r = ui
                .add(
                    egui::DragValue::new(&mut tabs)
                        .range(0..=16)
                        .custom_formatter(|n, _| {
                            if n == 0.0 {
                                "Tab key".to_string()
                            } else {
                                format!("{n:.0} spaces")
                            }
                        })
                        .custom_parser(|text| {
                            text.trim()
                                .trim_end_matches("spaces")
                                .trim()
                                .parse::<f64>()
                                .ok()
                        }),
                )
                .on_hover_text(
                    "Web forms usually treat Tab as 'next field'. Send spaces instead when \
                     that would jump out of the box you are typing into.",
                );
            if r.changed() {
                s.expand_tabs = tabs as usize;
                recount = true;
            }
        });

        if recount {
            self.refresh_meta();
        }
        if rehint {
            self.refresh_hint();
        }
        if rechip {
            self.refresh_backend_chip();
        }
    }

    fn footer(&mut self, ui: &mut egui::Ui, p: &Palette) {
        ui.add(
            ProgressBar::new(self.fraction)
                .desired_height(8.0)
                .corner_radius(CornerRadius::same(4))
                .fill(p.accent),
        );
        let running = self.running();
        ui.horizontal(|ui| {
            let status = RichText::new(&self.status).color(p.muted);
            ui.add(egui::Label::new(status).truncate());

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                let stop = Button::new(RichText::new("Stop").color(p.danger));
                if ui.add_enabled(running, stop).clicked() {
                    self.stop("stopped");
                }
                let label = if self.paused { "Resume" } else { "Pause" };
                if ui.add_enabled(running, Button::new(label)).clicked() {
                    self.toggle_pause();
                }
                let clicked = ui
                    .scope(|ui| {
                        // The one button that lights up its own colour on hover.
                        let w = &mut ui.visuals_mut().widgets;
                        w.hovered.weak_bg_fill = p.accent_hover;
                        w.hovered.bg_stroke = Stroke::new(1.0, p.accent_hover);
                        w.hovered.fg_stroke = Stroke::new(1.0, p.accent_text);
                        let start = Button::new(
                            RichText::new("Start typing").strong().color(p.accent_text),
                        )
                        .fill(p.accent)
                        .stroke(Stroke::new(1.0, p.accent))
                        .min_size(Vec2::new(130.0, 0.0));
                        ui.add_enabled(!running, start)
                            .on_hover_text("Ctrl+Enter")
                            .on_disabled_hover_text("Ctrl+Enter")
                            .clicked()
                    })
                    .inner;
                if clicked {
                    self.start(&ui.ctx().clone());
                }
            });
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let anchor = egui::Align2::CENTER_CENTER;
        let mut close_notice = false;
        if let Some(n) = &self.notice {
            egui::Window::new(&n.title)
                .collapsible(false)
                .resizable(false)
                .anchor(anchor, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.set_max_width(420.0);
                    ui.label(&n.body);
                    ui.add_space(6.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close_notice = ui.button("OK").clicked();
                    });
                });
        }
        if close_notice {
            self.notice = None;
        }

        let mut close_diagnostics = false;
        if let Some(text) = &self.diagnostics {
            egui::Window::new("Diagnostics")
                .collapsible(false)
                .resizable(false)
                .anchor(anchor, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label(RichText::new(text).monospace());
                    ui.add_space(6.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        close_diagnostics = ui.button("Close").clicked();
                    });
                });
        }
        if close_diagnostics {
            self.diagnostics = None;
        }

        if self.confirm_quit {
            let (mut yes, mut no) = (false, false);
            egui::Window::new("Still typing")
                .collapsible(false)
                .resizable(false)
                .anchor(anchor, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Typing is still in progress. Stop it and quit?");
                    ui.add_space(6.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        no = ui.button("No").clicked();
                        yes = ui.button("Yes").clicked();
                    });
                });
            if no {
                self.confirm_quit = false;
            }
            if yes {
                self.confirm_quit = false;
                self.quit(ctx);
            }
        }
    }

    /// Stop any run, wait for the keyboard to be released, and close.
    fn quit(&mut self, ctx: &egui::Context) {
        if let Some(run) = &mut self.run {
            if let Some(c) = &run.control {
                c.stop("window closed");
            }
            if let Some(worker) = run.worker.take() {
                join_for(worker, Duration::from_secs(2));
            }
        }
        self.run = None;
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// Join a thread, but give up after `limit` rather than hang the window.
fn join_for(handle: JoinHandle<()>, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !handle.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain(ctx);

        // Debounced recount after typing in the editor.
        if let Some(due) = self.meta_due {
            if Instant::now() >= due {
                self.refresh_meta();
            } else {
                ctx.request_repaint_after(due - Instant::now());
            }
        }

        // Closing the window mid-run asks first, rather than leaving the
        // keyboard to a thread nobody can see.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && self.running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_quit = true;
        }

        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Enter)) {
            self.start(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.load_file(ctx);
        }
        if self.running() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.stop("stopped from the app window");
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.draw(ui);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings.save();
    }
}

impl App {
    /// Lay out one frame. Separate from `eframe::App::ui` so it can be driven
    /// without a window.
    fn draw(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let p = palette(ctx.global_style().visuals.dark_mode);
        let side = |top: i8, bottom: i8| {
            Frame::new()
                .inner_margin(Margin {
                    left: 18,
                    right: 18,
                    top,
                    bottom,
                })
                .fill(p.bg)
        };

        egui::Panel::bottom("footer")
            .frame(side(6, 14))
            .show_separator_line(false)
            .show(ui, |ui| self.footer(ui, p));
        egui::Panel::bottom("controls")
            .frame(side(8, 8))
            .show_separator_line(false)
            .show(ui, |ui| self.controls(ui, p));
        egui::Panel::bottom("links")
            .frame(side(6, 0))
            .show_separator_line(false)
            .show(ui, |ui| self.links(ui, p, &ctx));
        egui::Panel::top("header")
            .frame(side(16, 4))
            .show_separator_line(false)
            .show(ui, |ui| self.header(ui, p));
        egui::CentralPanel::default()
            .frame(
                Frame::new()
                    .inner_margin(Margin {
                        left: 18,
                        right: 18,
                        top: 4,
                        bottom: 0,
                    })
                    .fill(p.bg),
            )
            .show(ui, |ui| self.editor(ui, p));

        self.dialogs(&ctx);
    }
}

/// A borderless, muted button that lights up on hover.
fn link(ui: &mut egui::Ui, text: &str, p: &Palette) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.fg_stroke = Stroke::new(1.0, p.muted);
        w.hovered.fg_stroke = Stroke::new(1.0, p.accent);
        w.active.fg_stroke = Stroke::new(1.0, p.accent);
        ui.add(Button::new(text).frame(false))
    })
    .inner
}

/// One `label  slider  value` triple of the rhythm grid.
#[allow(clippy::too_many_arguments)]
fn slider_row(
    ui: &mut egui::Ui,
    p: &Palette,
    label: &str,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    step: f64,
    tip: &str,
    fmt: impl Fn(f64) -> String,
) -> bool {
    ui.label(label);
    ui.spacing_mut().slider_width = 130.0;
    let moved = ui
        .add(Slider::new(value, range).step_by(step).show_value(false))
        .on_hover_text(tip)
        .changed();
    ui.add_sized(
        [52.0, 18.0],
        egui::Label::new(RichText::new(fmt(*value)).size(12.0).color(p.faint)),
    );
    moved
}

/// Digits grouped by thousands, as `{:,}` would write them.
fn group(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Upper-case the first letter and lower-case the rest, as Python's `capitalize`.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}

/// Open the window. Returns the process exit status.
pub fn run() -> ExitCode {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("OpenAutoTyper")
            // Matches the desktop file's name, which is how Wayland compositors
            // pair the window with its icon and launcher entry.
            .with_app_id("openautotyper")
            .with_inner_size([880.0, 760.0])
            .with_min_inner_size([640.0, 560.0]),
        ..Default::default()
    };
    match eframe::run_native(
        "OpenAutoTyper",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            report_failure(&e.to_string());
            ExitCode::FAILURE
        }
    }
}

fn failure_message(detail: &str) -> String {
    format!(
        "OpenAutoTyper's window could not open.\n\n{detail}\n\n\
         The command line works without it:  openautotyper --help"
    )
}

/// Say why the window did not open, somewhere the user will actually see it.
///
/// A desktop launcher runs with `Terminal=false`, so stderr goes nowhere: a
/// failure here is indistinguishable from the program being broken. Try the
/// session's own dialog tools first and fall back to stderr for anyone who did
/// start us from a shell.
fn report_failure(detail: &str) {
    let message = failure_message(detail);
    eprintln!("{message}");
    let dialogs: [(&str, Vec<String>); 4] = [
        (
            "kdialog",
            vec![
                "--title".into(),
                "OpenAutoTyper".into(),
                "--sorry".into(),
                message.clone(),
            ],
        ),
        (
            "zenity",
            vec![
                "--error".into(),
                "--title=OpenAutoTyper".into(),
                format!("--text={message}"),
            ],
        ),
        ("xmessage", vec!["-center".into(), message.clone()]),
        ("notify-send", vec!["OpenAutoTyper".into(), message.clone()]),
    ];
    for (program, args) in dialogs {
        if !clipboard::which(program) {
            continue;
        }
        if std::process::Command::new(program)
            .args(&args)
            .status()
            .is_ok()
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `frames` frames of the real layout in a bare egui context.
    fn render(app: &mut App, ctx: &egui::Context, frames: usize) {
        for _ in 0..frames {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(880.0, 760.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run_ui(input, |ui| app.draw(ui));
        }
    }

    fn headless() -> (App, egui::Context) {
        let ctx = egui::Context::default();
        install_theme(&ctx);
        (App::with_settings(Settings::default()), ctx)
    }

    #[test]
    fn the_window_lays_out_in_every_state_without_panicking() {
        let (mut app, ctx) = headless();
        render(&mut app, &ctx, 3);

        app.text = "Hello, world. ".repeat(500);
        app.refresh_meta();
        assert!(app.meta.contains("6,999 chars"), "{}", app.meta);
        render(&mut app, &ctx, 2);

        app.advanced = true;
        render(&mut app, &ctx, 2);

        app.warn("Nothing to type", "Put some text in the box first.");
        app.show_diagnostics();
        app.confirm_quit = true;
        render(&mut app, &ctx, 2);

        app.fraction = 0.5;
        app.paused = true;
        app.status = "Paused \u{2014} 3,500 of 7,000".into();
        render(&mut app, &ctx, 2);
    }

    #[test]
    fn it_renders_in_both_themes() {
        for theme in [Theme::Dark, Theme::Light] {
            let (mut app, ctx) = headless();
            ctx.set_theme(theme);
            app.advanced = true;
            render(&mut app, &ctx, 2);
            assert_eq!(ctx.global_style().visuals.dark_mode, theme == Theme::Dark);
        }
    }

    #[test]
    fn starting_with_no_text_asks_for_some() {
        let (mut app, ctx) = headless();
        app.start(&ctx);
        assert!(!app.running());
        assert_eq!(
            app.notice.as_ref().map(|n| n.title.as_str()),
            Some("Nothing to type")
        );
    }

    #[test]
    fn a_finished_run_is_reported_through_the_window_messages() {
        let (mut app, ctx) = headless();
        app.run = Some(Run {
            control: None,
            stop_pending: None,
            worker: None,
        });
        app.tx
            .send(Msg::Finished(RunResult {
                state: State::Done,
                typed: 1234,
                total: 1234,
                duration: 75.0,
                reason: String::new(),
                skipped: vec![(3, "\u{20ac}".into())],
                slip: 0.0,
                panic_active: true,
            }))
            .unwrap();
        app.drain(&ctx);
        assert!(!app.running());
        assert_eq!(app.fraction, 1.0);
        assert!(
            app.status
                .starts_with("Done \u{2014} 1,234 characters in 1m 15s"),
            "{}",
            app.status
        );
        assert!(app.status.contains('\u{20ac}'));
    }

    #[test]
    fn a_stop_pressed_before_the_session_exists_is_applied_when_it_does() {
        let (mut app, ctx) = headless();
        app.run = Some(Run {
            control: None,
            stop_pending: None,
            worker: None,
        });
        app.stop("stopped");
        assert!(app.run.as_ref().unwrap().stop_pending.is_some());

        let mut session = TypeSession::new(
            "abc",
            &Settings::default().cadence(),
            Box::new(backend::NullBackend::default()),
            SessionOptions {
                countdown: 0.0,
                panic_key: None,
                realtime: false,
                ..Default::default()
            },
        )
        .unwrap();
        app.tx.send(Msg::Started(session.control())).unwrap();
        app.drain(&ctx);
        assert!(app.run.as_ref().unwrap().stop_pending.is_none());
        assert_eq!(session.run().state, State::Stopped);
    }

    #[test]
    fn the_window_does_not_promise_an_abort_key_it_cannot_deliver() {
        // Typing needs /dev/uinput; stopping needs /dev/input/event*. Different
        // permissions, so the window can be able to type and unable to stop. It
        // used to say "Press Esc at any time to stop" either way.
        let (mut app, _ctx) = headless();
        app.abort_ok = (false, "no readable devices".into());
        app.refresh_hint();
        assert!(app.status.contains("No abort key") && app.status.contains("no readable devices"));
        assert!(!app.status.contains("Press Esc"));

        app.abort_ok = (true, "watching 3 device(s)".into());
        app.refresh_hint();
        assert!(app.status.to_lowercase().contains("stop"), "{}", app.status);
        assert!(app.status.contains("Press Esc"), "{}", app.status);

        app.settings.panic_key = "KEY_F12".into();
        app.refresh_hint();
        assert!(app.status.contains("Press F12"), "{}", app.status);

        app.settings.any_key_stops = true;
        app.refresh_hint();
        assert!(app.status.contains("touch the keyboard"), "{}", app.status);
    }

    #[test]
    fn typography_is_folded_before_counting() {
        let (mut app, _ctx) = headless();
        app.text = "a \u{2014} b".into(); // an em dash folds to two characters
        assert_eq!(app.normalized_text(), "a -- b");
        app.settings.fold_typography = false;
        assert_eq!(app.normalized_text(), "a \u{2014} b");
    }

    #[test]
    fn the_estimate_matches_the_engine() {
        let (mut app, _ctx) = headless();
        app.text = "x".repeat(500);
        app.settings.wpm = 100.0;
        app.refresh_meta();
        // 500 chars / 5 = 100 words at 100 wpm = 1 minute.
        assert!(
            app.meta.contains("1m 00s") && app.meta.contains("500 chars"),
            "{}",
            app.meta
        );
    }

    #[test]
    fn a_failure_to_open_the_window_is_explained_not_swallowed() {
        let message = failure_message("no display");
        assert!(message.contains("no display") && message.contains("openautotyper --help"));
    }

    #[test]
    fn settings_changes_reach_the_estimate() {
        let (mut app, _ctx) = headless();
        app.text = "word ".repeat(100);
        app.settings.wpm = 100.0;
        app.refresh_meta();
        let fast = app.meta.clone();
        app.settings.wpm = 25.0;
        app.refresh_meta();
        assert_ne!(fast, app.meta);
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1000), "1,000");
        assert_eq!(group(1234567), "1,234,567");
    }

    #[test]
    fn capitalize_matches_python() {
        assert_eq!(capitalize("aborted with KEY_ESC"), "Aborted with key_esc");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn each_preset_recognises_itself_and_only_itself() {
        for preset in &PRESETS {
            let mut s = Settings::default();
            preset.apply(&mut s);
            let matching: Vec<&str> = PRESETS
                .iter()
                .filter(|p| p.matches(&s))
                .map(|p| p.name)
                .collect();
            assert_eq!(matching, [preset.name]);
            assert!(s.cadence().validate().is_ok());
        }
    }

    #[test]
    fn every_abort_key_choice_is_a_real_key() {
        for (_, key) in PANIC_KEYS {
            assert!(crate::backend::codes::key_code(key).is_some(), "{key}");
        }
    }

    #[test]
    fn the_palettes_have_readable_text() {
        // WCAG-style contrast of body text against the window background.
        fn lum(c: Color32) -> f64 {
            let f = |v: u8| {
                let v = f64::from(v) / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
        }
        fn contrast(a: Color32, b: Color32) -> f64 {
            let (x, y) = (lum(a), lum(b));
            (x.max(y) + 0.05) / (x.min(y) + 0.05)
        }
        for p in [&DARK, &LIGHT] {
            assert!(contrast(p.text, p.bg) > 7.0);
            assert!(contrast(p.muted, p.bg) > 4.5);
            assert!(contrast(p.accent_text, p.accent) > 4.5);
            assert!(contrast(p.danger, p.bg) > 3.0);
            let _ = (p.accent_hover, p.surface_alt, p.border, p.faint, p.surface);
        }
    }
}
