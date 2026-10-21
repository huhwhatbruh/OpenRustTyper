//! What the virtual keyboard looks like to anything that inspects it.
//!
//! A uinput device is a real keyboard as far as the *input stack* is concerned
//! -- that is the whole reason v2 uses one. It is not automatically a real
//! keyboard as far as *userspace* is concerned, because
//! `/proc/bus/input/devices` is world-readable and says what the device called
//! itself. A freshly created uinput device announces its library in `Phys`,
//! carries a capability bitmap no keyboard has (no `MSC_SCAN`, no LEDs, no
//! autorepeat, plus a force-feedback bit), and is named after the program.
//!
//! So this module builds an identity from a keyboard the machine genuinely has.
//! The device then reads as a second keyboard of a model already present, which
//! is an ordinary thing for a computer to have.
//!
//! What this does not do, and cannot: the kernel puts every uinput device under
//! `/sys/devices/virtual/input/`, and nothing a userspace process passes to
//! `UI_DEV_SETUP` changes that. The aim here is to stop announcing it, not to
//! defeat an inspection.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

/// Used when no real keyboard can be read -- a generic USB HID descriptor, the
/// single most common shape of name on any machine with an external keyboard.
const GENERIC: (&str, u16, u16, u16) = ("USB Keyboard", 0x1A2C, 0x2124, 0x0110);

/// Bus 0x03 is BUS_USB. A keyboard on any other bus (0x11, the i8042 port, for
/// a laptop's built-in one) would need a matching `Phys`, and inventing a PS/2
/// port that is not there is worse than presenting as USB, which every machine
/// plausibly has.
pub const BUS_USB: u16 = 0x03;

/// A plausible port for a keyboard on a machine whose real ones cannot be read.
const FALLBACK_PHYS: &str = "usb-0000:00:14.0-2/input0";

/// The name and ids a virtual keyboard presents itself under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
    pub phys: String,
    /// Where this came from, for `--doctor`.
    pub source: String,
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

fn read_hex(path: String) -> Option<u32> {
    let text = read(path);
    if text.is_empty() {
        return Some(0);
    }
    u32::from_str_radix(&text, 16).ok()
}

/// Real USB keyboards this machine has, best to clone first.
///
/// Read from `/sys` rather than by opening `/dev/input/event*`: naming a device
/// needs no more privilege than listing it, and a machine where the abort key
/// cannot be watched should still get a decent identity.
fn candidates(root: &str) -> Vec<Identity> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            n.strip_prefix("input")
                .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    names.sort();

    // Another uinput device, possibly one of ours, is not a candidate -- and
    // every physical path the machine already reports is taken. The clone has
    // to pick a port none of them uses, or it recreates the very contradiction
    // it exists to avoid.
    let bases: Vec<String> = names
        .into_iter()
        .map(|entry| format!("{root}/{entry}"))
        .filter(|base| {
            !fs::canonicalize(base).is_ok_and(|p| p.to_string_lossy().contains("/virtual/"))
        })
        .collect();
    let mut taken: HashSet<String> = bases
        .iter()
        .map(|base| read(format!("{base}/phys")))
        .filter(|phys| !phys.is_empty())
        .collect();
    taken.insert(FALLBACK_PHYS.to_string());

    let mut ranked: Vec<((u8, usize), Identity)> = Vec::new();
    for base in bases {
        let name = read(format!("{base}/name"));
        if name.is_empty() {
            continue;
        }
        let (Some(vendor), Some(product), Some(version), Some(bustype)) = (
            read_hex(format!("{base}/id/vendor")),
            read_hex(format!("{base}/id/product")),
            read_hex(format!("{base}/id/version")),
            read_hex(format!("{base}/id/bustype")),
        ) else {
            continue;
        };
        // Only USB keyboards are worth cloning. An identity has to come with a
        // Phys matching the bus it claims, and a believable USB port path is
        // something that can be constructed; a second keyboard on the laptop's
        // single i8042 port is not a machine that exists.
        if bustype != u32::from(BUS_USB) || vendor == 0 || !is_a_keyboard(&base) {
            continue;
        }

        let phys = read(format!("{base}/phys"));
        // A gaming mouse's HID keyboard endpoint passes every capability test a
        // keyboard does, so the last word goes to the name: presenting as a
        // mouse would be its own kind of odd.
        let says_keyboard = u8::from(!name.to_lowercase().contains("keyboard"));
        let source = format!("modelled on this machine's {name:?}");
        ranked.push((
            (says_keyboard, ranked.len()),
            Identity {
                name,
                vendor: vendor as u16,
                product: product as u16,
                version: version as u16,
                phys: if phys.is_empty() {
                    FALLBACK_PHYS.into()
                } else {
                    sibling_port_in(&phys, &taken)
                },
                source,
            },
        ));
    }
    ranked.sort_by_key(|(rank, _)| *rank);
    ranked.into_iter().map(|(_, id)| id).collect()
}

/// Whether this input node is a keyboard, rather than some other button.
///
/// A mouse, a lid switch and a power button are all `EV_KEY` devices. Two
/// things separate a keyboard: it can produce letters, and it has the lock
/// lights. Every real keyboard reports `LED` capabilities even when the lights
/// are not physically fitted.
fn is_a_keyboard(base: &str) -> bool {
    if !bit(&read(format!("{base}/capabilities/led")), 0) {
        return false; // LED_NUML
    }
    let keys = read(format!("{base}/capabilities/key"));
    bit(&keys, 30) && bit(&keys, 44) // KEY_A and KEY_Z
}

/// Read one bit out of a sysfs capability bitmap.
///
/// The kernel prints these as space-separated hex words, most significant word
/// first, each covering 64 bits.
fn bit(bitmap: &str, index: usize) -> bool {
    let words: Vec<&str> = bitmap.split_whitespace().collect();
    let (word_from_end, within) = (index / 64, index % 64);
    if word_from_end >= words.len() {
        return false;
    }
    match u64::from_str_radix(words[words.len() - 1 - word_from_end], 16) {
        Ok(word) => word >> within & 1 == 1,
        Err(_) => false,
    }
}

/// A USB port path next to `phys`, avoiding every path in `taken`.
///
/// Two input nodes reporting the same name *and* the same physical path are a
/// contradiction: one keyboard cannot be plugged into one port twice. Moving
/// the port number makes the pair read as two keyboards of one model -- but it
/// must not land on the port of a *third* keyboard, which recreates the same
/// impossible pair against that one.
///
/// Matches `usb-<bus>-<port>[.<sub>...]/input<N>`.
fn sibling_port_in(phys: &str, taken: &HashSet<String>) -> String {
    let Some((bus, port, rest)) = parse_usb_phys(phys) else {
        return FALLBACK_PHYS.into();
    };
    for step in 1..=8 {
        let candidate = format!("usb-{bus}-{}{rest}", port % 8 + step);
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    FALLBACK_PHYS.into()
}

fn parse_usb_phys(phys: &str) -> Option<(&str, u32, &str)> {
    let rest = phys.strip_prefix("usb-")?;
    let (bus, tail) = rest.split_once('-')?;
    if bus.is_empty()
        || !bus
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
    {
        return None;
    }
    let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let (port, mut suffix) = tail.split_at(digits);
    // Any number of `.N` hub hops, then `/inputN`.
    while let Some(after) = suffix.strip_prefix('.') {
        let n = after.bytes().take_while(u8::is_ascii_digit).count();
        if n == 0 {
            return None;
        }
        suffix = &after[n..];
    }
    let n = suffix.strip_prefix("/input")?;
    if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((bus, port.parse().ok()?, suffix_from(tail, digits)))
}

fn suffix_from(tail: &str, digits: usize) -> &str {
    &tail[digits..]
}

/// Pick the identity the virtual keyboard should present. `override_name` names
/// the device outright, for anyone who would rather choose.
pub fn for_this_machine(override_name: Option<&str>) -> Identity {
    for_root("/sys/class/input", override_name)
}

fn for_root(root: &str, override_name: Option<&str>) -> Identity {
    if let Some(name) = override_name.filter(|n| !n.is_empty()) {
        return Identity {
            name: name.to_string(),
            vendor: GENERIC.1,
            product: GENERIC.2,
            version: GENERIC.3,
            phys: FALLBACK_PHYS.into(),
            source: "chosen by the user".into(),
        };
    }
    if let Some(first) = candidates(root).into_iter().next() {
        return first;
    }
    Identity {
        name: GENERIC.0.into(),
        vendor: GENERIC.1,
        product: GENERIC.2,
        version: GENERIC.3,
        phys: FALLBACK_PHYS.into(),
        source: "generic USB keyboard (no readable keyboard to model)".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_input(root: &Path, n: u32, name: &str, bus: &str, led: &str, keys: &str, phys: &str) {
        let base = root.join(format!("input{n}"));
        fs::create_dir_all(base.join("id")).unwrap();
        fs::create_dir_all(base.join("capabilities")).unwrap();
        fs::write(base.join("name"), format!("{name}\n")).unwrap();
        fs::write(base.join("phys"), format!("{phys}\n")).unwrap();
        fs::write(base.join("id/bustype"), format!("{bus}\n")).unwrap();
        fs::write(base.join("id/vendor"), "0d62\n").unwrap();
        fs::write(base.join("id/product"), "3740\n").unwrap();
        fs::write(base.join("id/version"), "0110\n").unwrap();
        fs::write(base.join("capabilities/led"), format!("{led}\n")).unwrap();
        fs::write(base.join("capabilities/key"), format!("{keys}\n")).unwrap();
    }

    // Bits 30 (A) and 44 (Z) set, as a real keyboard's key bitmap would have.
    const KEYS: &str = "100040000000";

    #[test]
    fn bitmaps_are_read_most_significant_word_first() {
        assert!(bit("7", 0) && bit("7", 2) && !bit("7", 3));
        assert!(bit("1 0", 64) && !bit("1 0", 0));
        assert!(!bit("", 0));
        assert!(!bit("ff", 200));
        assert!(bit(KEYS, 30) && bit(KEYS, 44));
    }

    #[test]
    fn a_sibling_port_differs_from_the_original() {
        assert_eq!(
            sibling_port_in("usb-0000:00:14.0-10/input1", &HashSet::new()),
            "usb-0000:00:14.0-3/input1"
        );
        assert_eq!(
            sibling_port_in("usb-0000:00:14.0-2.4/input0", &HashSet::new()),
            "usb-0000:00:14.0-3.4/input0"
        );
        assert_ne!(
            sibling_port_in("usb-0000:00:14.0-7/input0", &HashSet::new()),
            "usb-0000:00:14.0-7/input0"
        );
    }

    #[test]
    fn a_clone_does_not_take_a_port_another_keyboard_uses() {
        let dir = tempfile::tempdir().unwrap();
        fake_input(
            dir.path(),
            1,
            "Plain Keyboard",
            "3",
            "7",
            KEYS,
            "usb-0000:00:14.0-2/input0",
        );
        fake_input(
            dir.path(),
            2,
            "Other Keyboard",
            "3",
            "7",
            KEYS,
            "usb-0000:00:14.0-3/input0",
        );
        let id = for_root(dir.path().to_str().unwrap(), None);
        assert_eq!(id.name, "Plain Keyboard");
        // Port 3 is the other keyboard's; the clone moves past it.
        assert_eq!(id.phys, "usb-0000:00:14.0-4/input0");
    }

    #[test]
    fn a_phys_that_is_not_usb_gets_the_fallback() {
        assert_eq!(
            sibling_port_in("isa0060/serio0/input0", &HashSet::new()),
            FALLBACK_PHYS
        );
        assert_eq!(sibling_port_in("", &HashSet::new()), FALLBACK_PHYS);
        assert_eq!(
            sibling_port_in("usb-xyz!-2/input0", &HashSet::new()),
            FALLBACK_PHYS
        );
        assert_eq!(
            sibling_port_in("usb-0000:00:14.0-2/inputX", &HashSet::new()),
            FALLBACK_PHYS
        );
    }

    #[test]
    fn a_real_usb_keyboard_is_cloned() {
        let dir = tempfile::tempdir().unwrap();
        fake_input(
            dir.path(),
            4,
            "HID 0d62:3740 Keyboard",
            "3",
            "7",
            KEYS,
            "usb-0000:00:14.0-10/input1",
        );
        let id = for_root(dir.path().to_str().unwrap(), None);
        assert_eq!(id.name, "HID 0d62:3740 Keyboard");
        assert_eq!(
            (id.vendor, id.product, id.version),
            (0x0d62, 0x3740, 0x0110)
        );
        assert_eq!(id.phys, "usb-0000:00:14.0-3/input1");
        assert!(id.source.contains("modelled on"));
    }

    #[test]
    fn a_mouse_or_a_ps2_keyboard_is_not_cloned() {
        let dir = tempfile::tempdir().unwrap();
        fake_input(
            dir.path(),
            1,
            "Some Mouse",
            "3",
            "0",
            "0",
            "usb-0000:00:14.0-1/input0",
        );
        fake_input(
            dir.path(),
            2,
            "AT Keyboard",
            "11",
            "7",
            KEYS,
            "isa0060/serio0/input0",
        );
        let id = for_root(dir.path().to_str().unwrap(), None);
        assert_eq!(id.name, GENERIC.0);
    }

    #[test]
    fn a_device_named_keyboard_beats_one_that_merely_acts_like_one() {
        let dir = tempfile::tempdir().unwrap();
        fake_input(
            dir.path(),
            1,
            "Gaming Mouse",
            "3",
            "7",
            KEYS,
            "usb-0000:00:14.0-1/input0",
        );
        fake_input(
            dir.path(),
            2,
            "Plain Keyboard",
            "3",
            "7",
            KEYS,
            "usb-0000:00:14.0-2/input0",
        );
        let id = for_root(dir.path().to_str().unwrap(), None);
        assert_eq!(id.name, "Plain Keyboard");
    }

    #[test]
    fn the_device_never_names_itself_after_this_program() {
        // The failure this module exists for: the name was a signed confession.
        let id = for_this_machine(None);
        let lowered = format!("{} {}", id.name, id.phys).to_lowercase();
        for word in [
            "openautotyper",
            "autotyper",
            "evdev",
            "uinput",
            "virtual",
            "python",
            "rust",
        ] {
            assert!(!lowered.contains(word), "{word:?} in {id:?}");
        }
    }

    #[test]
    fn the_identity_is_a_usb_keyboard_in_every_field() {
        let id = for_this_machine(None);
        assert!(id.phys.starts_with("usb-"), "{id:?}");
        assert!(id.vendor > 0);
        assert!(!id.source.is_empty());
    }

    #[test]
    fn a_cloned_keyboard_is_not_plugged_into_its_own_port() {
        // One keyboard cannot occupy one USB port twice. The clone keeps the
        // model but moves the port, so the pair reads as two keyboards of one
        // model -- which is an ordinary thing to own.
        let real = "usb-0000:00:14.0-10/input1";
        let sibling = sibling_port_in(real, &HashSet::new());
        assert_ne!(sibling, real);
        assert!(sibling.starts_with("usb-0000:00:14.0-"));
        assert!(sibling.ends_with("/input1"));
    }

    #[test]
    fn the_chosen_model_is_a_keyboard_this_machine_has() {
        for id in candidates("/sys/class/input") {
            assert!(id.vendor > 0 && id.phys.starts_with("usb-"), "{id:?}");
        }
    }

    #[test]
    fn the_user_can_choose_the_name_outright() {
        let id = for_root("/nonexistent", Some("My Board"));
        assert_eq!(id.name, "My Board");
        assert_eq!(id.source, "chosen by the user");
    }

    #[test]
    fn nothing_to_model_gives_a_generic_keyboard() {
        let id = for_root("/nonexistent", None);
        assert_eq!(id.name, GENERIC.0);
        assert!(id.source.contains("generic"));
    }
}
