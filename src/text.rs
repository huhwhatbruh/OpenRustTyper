//! Text normalization.
//!
//! Pasted text carries characters that no keyboard can produce: smart quotes
//! from word processors, non-breaking spaces from HTML, zero-width joiners from
//! absolutely everywhere. Typing them requires a clipboard detour that is slow
//! and visible; folding them to their ASCII equivalents is both faster and
//! closer to what a person at a keyboard would have produced.

use std::collections::BTreeMap;

/// Characters that carry no visible width. They survive copy/paste and then
/// confuse everything downstream, so they are dropped rather than folded.
const INVISIBLE: &[char] = &[
    '\u{200B}', '\u{200C}', '\u{200D}', '\u{2060}', '\u{FEFF}', '\u{00AD}', '\u{180E}', '\u{034F}',
];

/// Typographic characters -> what a person typing on a keyboard would produce,
/// with the Unicode name for the summary line.
#[rustfmt::skip]
const FOLD: &[(char, &str, &str)] = &[
    ('\u{2018}', "'", "LEFT SINGLE QUOTATION MARK"),
    ('\u{2019}', "'", "RIGHT SINGLE QUOTATION MARK"),
    ('\u{201A}', "'", "SINGLE LOW-9 QUOTATION MARK"),
    ('\u{201B}', "'", "SINGLE HIGH-REVERSED-9 QUOTATION MARK"),
    ('\u{201C}', "\"", "LEFT DOUBLE QUOTATION MARK"),
    ('\u{201D}', "\"", "RIGHT DOUBLE QUOTATION MARK"),
    ('\u{201E}', "\"", "DOUBLE LOW-9 QUOTATION MARK"),
    ('\u{201F}', "\"", "DOUBLE HIGH-REVERSED-9 QUOTATION MARK"),
    ('\u{2032}', "'", "PRIME"),
    ('\u{2033}', "\"", "DOUBLE PRIME"),
    ('\u{2013}', "-", "EN DASH"),
    ('\u{2014}', "--", "EM DASH"),
    ('\u{2015}', "--", "HORIZONTAL BAR"),
    ('\u{2212}', "-", "MINUS SIGN"),
    ('\u{2010}', "-", "HYPHEN"),
    ('\u{2011}', "-", "NON-BREAKING HYPHEN"),
    ('\u{2026}', "...", "HORIZONTAL ELLIPSIS"),
    ('\u{00A0}', " ", "NO-BREAK SPACE"),
    ('\u{202F}', " ", "NARROW NO-BREAK SPACE"),
    ('\u{2007}', " ", "FIGURE SPACE"),
    ('\u{2009}', " ", "THIN SPACE"),
    ('\u{2002}', " ", "EN SPACE"),
    ('\u{2003}', " ", "EM SPACE"),
    ('\u{2004}', " ", "THREE-PER-EM SPACE"),
    ('\u{2005}', " ", "FOUR-PER-EM SPACE"),
    ('\u{2006}', " ", "SIX-PER-EM SPACE"),
    ('\u{2008}', " ", "PUNCTUATION SPACE"),
    ('\u{205F}', " ", "MEDIUM MATHEMATICAL SPACE"),
    ('\u{3000}', " ", "IDEOGRAPHIC SPACE"),
    ('\u{00AB}', "\"", "LEFT-POINTING DOUBLE ANGLE QUOTATION MARK"),
    ('\u{00BB}', "\"", "RIGHT-POINTING DOUBLE ANGLE QUOTATION MARK"),
    ('\u{2039}', "'", "SINGLE LEFT-POINTING ANGLE QUOTATION MARK"),
    ('\u{203A}', "'", "SINGLE RIGHT-POINTING ANGLE QUOTATION MARK"),
    ('\u{2022}', "-", "BULLET"),
    ('\u{00B7}', "-", "MIDDLE DOT"),
    ('\u{2043}', "-", "HYPHEN BULLET"),
    ('\u{00D7}', "x", "MULTIPLICATION SIGN"),
    ('\u{00F7}', "/", "DIVISION SIGN"),
    ('\u{2044}', "/", "FRACTION SLASH"),
    ('\u{00B4}', "'", "ACUTE ACCENT"),
    ('\u{2122}', "(tm)", "TRADE MARK SIGN"),
    ('\u{00AE}', "(r)", "REGISTERED SIGN"),
    ('\u{00A9}', "(c)", "COPYRIGHT SIGN"),
    ('\u{2190}', "<-", "LEFTWARDS ARROW"),
    ('\u{2192}', "->", "RIGHTWARDS ARROW"),
    ('\u{21D2}', "=>", "RIGHTWARDS DOUBLE ARROW"),
    ('\u{2264}', "<=", "LESS-THAN OR EQUAL TO"),
    ('\u{2265}', ">=", "GREATER-THAN OR EQUAL TO"),
    ('\u{2260}', "!=", "NOT EQUAL TO"),
];

fn fold_of(ch: char) -> Option<(&'static str, &'static str)> {
    FOLD.iter()
        .find(|(c, _, _)| *c == ch)
        .map(|(_, to, name)| (*to, *name))
}

/// Normalized text plus a record of what had to change to get there.
#[derive(Debug, Clone, Default)]
pub struct Normalized {
    pub text: String,
    /// Source character -> its replacement, in code point order.
    pub folded: BTreeMap<char, &'static str>,
    pub dropped: BTreeMap<char, usize>,
}

impl Normalized {
    pub fn changed(&self) -> bool {
        !self.folded.is_empty() || !self.dropped.is_empty()
    }

    /// Human-readable account of the substitutions, for the UI/CLI.
    pub fn summary(&self) -> String {
        let mut bits: Vec<String> = Vec::new();
        if !self.folded.is_empty() {
            let shown: Vec<String> = self
                .folded
                .iter()
                .take(6)
                .map(|(src, dst)| format!("{} -> {:?}", name_of(*src), dst))
                .collect();
            let more = self.folded.len().saturating_sub(6);
            let tail = if more > 0 {
                format!(" (+{more} more)")
            } else {
                String::new()
            };
            bits.push(format!("folded {}{}", shown.join(", "), tail));
        }
        if !self.dropped.is_empty() {
            let total: usize = self.dropped.values().sum();
            bits.push(format!("dropped {total} zero-width character(s)"));
        }
        bits.join("; ")
    }
}

fn name_of(ch: char) -> String {
    match fold_of(ch) {
        Some((_, name)) => name.to_string(),
        None => format!("U+{:04X}", ch as u32),
    }
}

/// Options for [`normalize`].
#[derive(Debug, Clone, Copy)]
pub struct NormalizeOptions {
    pub fold_typography: bool,
    /// 0 keeps tab characters (typed as the Tab key). Set it to a width when
    /// the target treats Tab as "move to next field" rather than "insert
    /// whitespace", which is the usual reason typing into a web form goes
    /// sideways.
    pub expand_tabs: usize,
    pub strip_trailing_ws: bool,
}

impl Default for NormalizeOptions {
    fn default() -> Self {
        Self {
            fold_typography: true,
            expand_tabs: 0,
            strip_trailing_ws: true,
        }
    }
}

/// Prepare text for typing.
///
/// Line endings are always normalized to `\n`: a literal carriage return is a
/// keystroke no one wants and CRLF would type Enter twice.
pub fn normalize(raw: &str, opts: NormalizeOptions) -> Normalized {
    let mut folded = BTreeMap::new();
    let mut dropped = BTreeMap::new();

    let text = raw.replace("\r\n", "\n").replace('\r', "\n");

    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if INVISIBLE.contains(&ch) {
            *dropped.entry(ch).or_insert(0) += 1;
            continue;
        }
        if opts.fold_typography {
            if let Some((sub, _)) = fold_of(ch) {
                folded.insert(ch, sub);
                out.push_str(sub);
                continue;
            }
        }
        out.push(ch);
    }

    let mut text = out;
    if opts.expand_tabs > 0 {
        text = expand_tabs(&text, opts.expand_tabs);
    }
    if opts.strip_trailing_ws {
        text = text
            .split('\n')
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n");
    }
    Normalized {
        text,
        folded,
        dropped,
    }
}

/// Column-aware tab expansion, the way Python's `str.expandtabs` does it.
fn expand_tabs(text: &str, width: usize) -> String {
    let mut out = String::with_capacity(text.len());
    let mut column = 0usize;
    for ch in text.chars() {
        match ch {
            '\t' => {
                let pad = width - column % width;
                out.extend(std::iter::repeat_n(' ', pad));
                column += pad;
            }
            '\n' | '\r' => {
                out.push(ch);
                column = 0;
            }
            _ => {
                out.push(ch);
                column += 1;
            }
        }
    }
    out
}

/// `(characters, words, lines)` for display.
pub fn stats(text: &str) -> (usize, usize, usize) {
    if text.is_empty() {
        return (0, 0, 0);
    }
    let lines = text.matches('\n').count() + 1;
    (text.chars().count(), text.split_whitespace().count(), lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(s: &str) -> Normalized {
        normalize(s, NormalizeOptions::default())
    }

    #[test]
    fn line_endings_are_always_unix() {
        assert_eq!(norm("a\r\nb\rc").text, "a\nb\nc");
    }

    #[test]
    fn smart_typography_folds_to_ascii() {
        let n = norm("\u{201C}hi\u{201D} \u{2014} it\u{2019}s\u{2026}");
        assert_eq!(n.text, "\"hi\" -- it's...");
        assert!(n.changed());
    }

    #[test]
    fn zero_width_characters_are_dropped() {
        let n = norm("a\u{200B}b\u{FEFF}c");
        assert_eq!(n.text, "abc");
        assert_eq!(n.dropped.values().sum::<usize>(), 2);
    }

    #[test]
    fn non_breaking_space_becomes_a_real_space() {
        assert_eq!(norm("a\u{00A0}b").text, "a b");
    }

    #[test]
    fn folding_can_be_switched_off() {
        let opts = NormalizeOptions {
            fold_typography: false,
            ..Default::default()
        };
        assert_eq!(
            normalize("\u{201C}x\u{201D}", opts).text,
            "\u{201C}x\u{201D}"
        );
    }

    #[test]
    fn tabs_survive_by_default_and_expand_on_request() {
        assert_eq!(norm("a\tb").text, "a\tb");
        let opts = NormalizeOptions {
            expand_tabs: 4,
            ..Default::default()
        };
        assert_eq!(normalize("a\tb", opts).text, "a   b");
        assert_eq!(normalize("abcd\te", opts).text, "abcd    e");
    }

    #[test]
    fn trailing_whitespace_is_stripped_per_line() {
        assert_eq!(norm("a  \nb\t\n").text, "a\nb\n");
    }

    #[test]
    fn clean_text_reports_no_changes() {
        let n = norm("plain text");
        assert!(!n.changed());
        assert_eq!(n.summary(), "");
    }

    #[test]
    fn the_summary_names_what_it_changed() {
        let n = norm("\u{2014}");
        assert!(n.summary().contains("EM DASH"), "{}", n.summary());
    }

    #[test]
    fn stats_count_characters_words_and_lines() {
        assert_eq!(stats(""), (0, 0, 0));
        assert_eq!(stats("one two\nthree"), (13, 3, 2));
    }
}
