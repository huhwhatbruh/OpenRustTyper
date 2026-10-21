//! Settings, remembered between runs.
//!
//! Stored as TOML under the XDG config directory so it can be read, edited and
//! version-controlled by hand. Unknown keys are preserved on write -- the GUI
//! saves on every window close, so a setting from a newer release, or one added
//! by hand, must survive being read by a build that does not understand it. Bad
//! values are ignored rather than fatal: a corrupt config should cost you your
//! preferences, not the ability to start the program.

use std::io::Write;
use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::timing::Cadence;

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("openautotyper")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

/// Everything the UI remembers.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    // cadence
    pub wpm: f64,
    pub variance: f64,
    pub rhythm: f64,
    pub hesitation: f64,
    pub sentence_pause: f64,
    pub max_pause: f64,
    pub typo_rate: f64,
    pub drift: f64,
    // run
    pub countdown: f64,
    /// Empty means auto-select.
    pub backend: String,
    pub panic_key: String,
    pub any_key_stops: bool,
    // text handling
    pub fold_typography: bool,
    pub expand_tabs: usize,
    pub strip_trailing_ws: bool,
    /// Keys found in the file that this build does not recognise, carried
    /// through to the next write. Without this the GUI, which saves on every
    /// window close, would silently delete a setting added by hand or by a
    /// newer release.
    pub unknown: Table,
}

impl Default for Settings {
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
            countdown: 5.0,
            backend: String::new(),
            panic_key: "KEY_ESC".into(),
            any_key_stops: false,
            fold_typography: true,
            expand_tabs: 0,
            strip_trailing_ws: true,
            unknown: Table::new(),
        }
    }
}

/// Every key the settings themselves own, in file order.
const KNOWN: &[&str] = &[
    "wpm",
    "variance",
    "rhythm",
    "hesitation",
    "sentence_pause",
    "max_pause",
    "typo_rate",
    "drift",
    "countdown",
    "backend",
    "panic_key",
    "any_key_stops",
    "fold_typography",
    "expand_tabs",
    "strip_trailing_ws",
];

fn as_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Integer(i) => Some(*i as f64),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_bool(v: &Value) -> Option<bool> {
    match v {
        Value::Boolean(b) => Some(*b),
        Value::Integer(i) => Some(*i != 0),
        _ => None,
    }
}

fn as_usize(v: &Value) -> Option<usize> {
    match v {
        Value::Integer(i) => usize::try_from(*i).ok(),
        Value::Float(f) if *f >= 0.0 => Some(*f as usize),
        _ => None,
    }
}

fn as_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Table(_) | Value::Array(_) => None,
        other => Some(other.to_string()),
    }
}

impl Settings {
    pub fn cadence(&self) -> Cadence {
        Cadence {
            wpm: self.wpm,
            variance: self.variance,
            rhythm: self.rhythm,
            hesitation: self.hesitation,
            sentence_pause: self.sentence_pause,
            max_pause: self.max_pause,
            typo_rate: self.typo_rate,
            drift: self.drift,
            ..Cadence::default()
        }
    }

    /// Load from the default location.
    pub fn load() -> Self {
        Self::load_from(&config_path())
    }

    pub fn load_from(path: &Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(table) = raw.parse::<Table>() else {
            return Self::default();
        };

        let mut s = Self::default();
        // One bad key must not discard the rest, so each is read on its own.
        macro_rules! take {
            ($key:literal, $field:ident, $conv:expr) => {
                if let Some(v) = table.get($key).and_then($conv) {
                    s.$field = v;
                }
            };
        }
        take!("wpm", wpm, as_f64);
        take!("variance", variance, as_f64);
        take!("rhythm", rhythm, as_f64);
        take!("hesitation", hesitation, as_f64);
        take!("sentence_pause", sentence_pause, as_f64);
        take!("max_pause", max_pause, as_f64);
        take!("typo_rate", typo_rate, as_f64);
        take!("drift", drift, as_f64);
        take!("countdown", countdown, as_f64);
        take!("backend", backend, as_string);
        take!("panic_key", panic_key, as_string);
        take!("any_key_stops", any_key_stops, as_bool);
        take!("fold_typography", fold_typography, as_bool);
        take!("expand_tabs", expand_tabs, as_usize);
        take!("strip_trailing_ws", strip_trailing_ws, as_bool);

        // Anything this build does not know about is somebody else's setting:
        // a newer release's, or one the user added by hand. Carry it.
        s.unknown = table
            .into_iter()
            .filter(|(k, _)| !KNOWN.contains(&k.as_str()))
            .collect();

        // Out-of-range values: fall back to defaults wholesale.
        if s.cadence().validate().is_err() {
            return Self::default();
        }
        s
    }

    /// Save to the default location.
    pub fn save(&self) -> bool {
        self.save_to(&config_path())
    }

    pub fn save_to(&self, path: &Path) -> bool {
        self.try_save(path).is_ok()
    }

    fn try_save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut body = Table::new();
        let mut put = |k: &str, v: Value| {
            body.insert(k.to_string(), v);
        };
        put("wpm", Value::Float(self.wpm));
        put("variance", Value::Float(self.variance));
        put("rhythm", Value::Float(self.rhythm));
        put("hesitation", Value::Float(self.hesitation));
        put("sentence_pause", Value::Float(self.sentence_pause));
        put("max_pause", Value::Float(self.max_pause));
        put("typo_rate", Value::Float(self.typo_rate));
        put("drift", Value::Float(self.drift));
        put("countdown", Value::Float(self.countdown));
        put("backend", Value::String(self.backend.clone()));
        put("panic_key", Value::String(self.panic_key.clone()));
        put("any_key_stops", Value::Boolean(self.any_key_stops));
        put("fold_typography", Value::Boolean(self.fold_typography));
        put("expand_tabs", Value::Integer(self.expand_tabs as i64));
        put("strip_trailing_ws", Value::Boolean(self.strip_trailing_ws));

        let mut out = String::from("# OpenAutoTyper settings. Delete this file to reset.\n\n");
        out.push_str(&toml::to_string(&body).map_err(std::io::Error::other)?);
        if !self.unknown.is_empty() {
            out.push_str(
                "\n# Not settings this version understands \u{2014} from a newer\n\
                 # release, or added by hand. Kept so saving cannot eat them.\n",
            );
            out.push_str(&toml::to_string(&self.unknown).map_err(std::io::Error::other)?);
        }

        // Atomic replace: a crash mid-write must not leave a truncated file.
        let tmp = path.with_extension("toml.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        // Flush to disk before the rename. The rename is atomic, but without
        // this a power loss can leave the new name pointing at blocks that were
        // never written -- an empty config.
        let written = file
            .write_all(out.as_bytes())
            .and_then(|()| file.sync_all());
        drop(file);
        let replaced = written.and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = replaced {
            // Do not leave a half-written .tmp beside a good config.
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn round_trip() {
        let d = dir();
        let p = d.path().join("c.toml");
        let s = Settings {
            wpm: 93.0,
            typo_rate: 0.02,
            backend: "uinput".into(),
            ..Default::default()
        };
        assert!(s.save_to(&p));
        assert_eq!(Settings::load_from(&p), s);
    }

    #[test]
    fn a_missing_file_gives_defaults() {
        assert_eq!(
            Settings::load_from(&dir().path().join("nope.toml")),
            Settings::default()
        );
    }

    #[test]
    fn unparseable_file_gives_defaults() {
        let d = dir();
        let p = d.path().join("c.toml");
        std::fs::write(&p, "wpm = = [[").unwrap();
        assert_eq!(Settings::load_from(&p), Settings::default());
    }

    #[test]
    fn one_bad_key_does_not_discard_the_others() {
        let d = dir();
        let p = d.path().join("c.toml");
        std::fs::write(&p, "wpm = \"fast\"\ncountdown = 9.0\n").unwrap();
        let s = Settings::load_from(&p);
        assert_eq!(s.wpm, 70.0);
        assert_eq!(s.countdown, 9.0);
    }

    #[test]
    fn out_of_range_values_fall_back_to_defaults() {
        let d = dir();
        let p = d.path().join("c.toml");
        std::fs::write(&p, "wpm = 99999.0\ncountdown = 9.0\n").unwrap();
        assert_eq!(Settings::load_from(&p), Settings::default());
    }

    #[test]
    fn settings_produce_a_valid_cadence() {
        assert!(Settings::default().cadence().validate().is_ok());
    }

    #[test]
    fn saving_creates_missing_directories() {
        let d = dir();
        let p = d.path().join("a/b/c.toml");
        assert!(Settings::default().save_to(&p));
        assert!(p.exists());
    }

    #[test]
    fn a_key_this_build_does_not_know_survives_a_save() {
        let d = dir();
        let p = d.path().join("c.toml");
        std::fs::write(
            &p,
            "wpm = 80.0\nfuture_flag = true\nnote = \"hi\"\n[table]\nx = 1\n",
        )
        .unwrap();
        let s = Settings::load_from(&p);
        assert!(s.save_to(&p));
        let after: Table = std::fs::read_to_string(&p).unwrap().parse().unwrap();
        assert_eq!(after["future_flag"], Value::Boolean(true));
        assert_eq!(after["note"], Value::String("hi".into()));
        assert_eq!(after["table"]["x"], Value::Integer(1));
        assert_eq!(after["wpm"], Value::Float(80.0));
    }

    #[test]
    fn preserved_keys_are_not_mistaken_for_settings() {
        let d = dir();
        let p = d.path().join("c.toml");
        std::fs::write(&p, "unknown = 5\nwpm = 80.0\n").unwrap();
        let s = Settings::load_from(&p);
        assert_eq!(s.wpm, 80.0);
        assert!(s.unknown.contains_key("unknown"));
        assert_eq!(s.cadence().wpm, 80.0);
    }

    #[test]
    fn a_config_with_no_surprises_writes_no_extra_section() {
        let d = dir();
        let p = d.path().join("c.toml");
        Settings::default().save_to(&p);
        assert!(!std::fs::read_to_string(&p)
            .unwrap()
            .contains("Not settings"));
    }

    #[test]
    fn a_string_with_quotes_survives_the_round_trip() {
        let d = dir();
        let p = d.path().join("c.toml");
        let s = Settings {
            backend: "we\"ird \\ name".into(),
            ..Default::default()
        };
        s.save_to(&p);
        assert_eq!(Settings::load_from(&p).backend, "we\"ird \\ name");
    }
}
