use super::*;

#[test]
fn every_backend_probe_returns_a_reason() {
    for (name, _ok, reason) in report() {
        assert!(!reason.is_empty(), "{name} gave no reason");
    }
}

#[test]
fn null_is_never_chosen_automatically() {
    // Typing into the void is indistinguishable from the bug being fixed. A
    // machine with no usable backend at all is a legitimate outcome.
    if let Ok(backend) = select(None) {
        assert_ne!(backend.name(), "null");
    }
}

#[test]
fn an_unknown_backend_name_is_rejected_clearly() {
    let err = select(Some("telepathy")).err().expect("must be refused");
    let msg = err.to_string();
    assert!(msg.contains("unknown backend 'telepathy'"), "{msg}");
    assert!(msg.contains("available: uinput, x11, null"), "{msg}");
}

#[test]
fn every_backend_is_listed_in_order_with_null_last() {
    let names: Vec<&str> = report().iter().map(|(n, _, _)| *n).collect();
    assert_eq!(names, ["uinput", "x11", "null"]);
}

#[test]
fn asking_for_an_unusable_backend_by_name_says_why() {
    for (name, ok, reason) in report() {
        if !ok {
            let msg = select(Some(name))
                .err()
                .expect("must be refused")
                .to_string();
            assert_eq!(msg, format!("backend '{name}' is unusable here: {reason}"));
        }
    }
}

#[test]
fn with_nothing_usable_every_reason_is_listed() {
    if report().iter().any(|(n, ok, _)| *ok && *n != "null") {
        return; // this machine can type; nothing to refuse
    }
    let msg = select(None).err().unwrap().to_string();
    assert!(
        msg.starts_with("no usable keyboard backend on this system:"),
        "{msg}"
    );
    assert!(
        msg.contains("  uinput: ") && msg.contains("  x11: "),
        "{msg}"
    );
}

#[test]
fn null_can_still_be_requested_by_name() {
    assert_eq!(select(Some("null")).unwrap().name(), "null");
}

#[test]
fn an_empty_name_means_automatic() {
    // The GUI stores "no preference" as an empty string.
    if let Ok(backend) = select(Some("")) {
        assert_ne!(backend.name(), "null");
    }
}

#[test]
fn null_backend_records_and_backspaces() {
    let mut backend = NullBackend::default();
    backend.open().unwrap();
    assert!(backend.type_char("a").unwrap());
    assert!(backend.type_char("enter").unwrap());
    assert!(backend.type_char("b").unwrap());
    assert!(backend.type_char("backspace").unwrap());
    assert_eq!(backend.text(), "a\n");
    assert!(!backend.type_char("nonsense").unwrap());
    backend.release_all();
    backend.close();
    assert!(backend.released == 1 && backend.closed);
}

#[test]
fn null_backend_honours_a_restricted_layout() {
    let mut backend = NullBackend::reaching("ab");
    backend.open().unwrap();
    assert!(backend.type_char("a").unwrap());
    assert!(!backend.type_char("z").unwrap());
}
