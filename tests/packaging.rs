//! The desktop entry and the package that ships it.
//!
//! A launcher is the one part of the program that gets no error channel: it runs
//! with `Terminal=false`, so anything it prints is discarded and a failure looks
//! identical to the program being broken. These tests check the ways that
//! happened -- a menu entry pointing at a program the package did not install,
//! or one whose libraries the package did not depend on.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

fn packaging() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging")
}

fn desktop_text() -> String {
    std::fs::read_to_string(packaging().join("openautotyper.desktop")).unwrap()
}

fn entries() -> HashMap<String, String> {
    desktop_text()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('['))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn pkgbuild() -> String {
    std::fs::read_to_string(packaging().join("PKGBUILD")).unwrap()
}

#[test]
fn the_desktop_file_starts_with_its_group_header() {
    // A stray shebang before [Desktop Entry] is not a comment, it is line 1.
    assert_eq!(desktop_text().lines().next(), Some("[Desktop Entry]"));
}

#[test]
fn the_desktop_file_validates() {
    let Ok(done) = Command::new("desktop-file-validate")
        .arg(packaging().join("openautotyper.desktop"))
        .output()
    else {
        eprintln!("SKIPPED: desktop-file-utils not installed");
        return;
    };
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
    assert!(done.status.success(), "{said}");
}

#[test]
fn the_launcher_hides_itself_when_the_program_is_absent() {
    // TryExec is the only thing between a missing GUI and a dead menu entry.
    let e = entries();
    assert_eq!(e["TryExec"], e["Exec"].split_whitespace().next().unwrap());
}

#[test]
fn the_launcher_starts_the_program_the_crate_builds() {
    // Exec names a binary; Cargo.toml has to actually produce it.
    let manifest =
        std::fs::read_to_string(packaging().parent().unwrap().join("Cargo.toml")).unwrap();
    let exec = entries()["Exec"]
        .split_whitespace()
        .next()
        .unwrap()
        .to_string();
    assert!(
        manifest.contains(&format!("name = \"{exec}\"")),
        "no [[bin]] called {exec}"
    );
}

#[test]
fn the_icon_the_entry_names_is_one_the_package_ships() {
    let icon = &entries()["Icon"];
    assert!(packaging().join(format!("{icon}.svg")).is_file(), "{icon}");
}

#[test]
fn the_window_title_matches_the_wayland_app_id_the_entry_expects() {
    // The compositor pairs a window with its launcher entry by app id, and the
    // entry's file name is what it looks for.
    let gui = std::fs::read_to_string(packaging().parent().unwrap().join("src/gui.rs")).unwrap();
    assert!(gui.contains(".with_app_id(\"openautotyper\")"));
    assert!(packaging().join("openautotyper.desktop").is_file());
}

#[test]
fn the_arch_package_installs_everything_the_entry_references() {
    // A desktop entry is a promise that clicking it starts something.
    let pkgbuild = pkgbuild();
    let depends = pkgbuild
        .split("depends=(")
        .nth(1)
        .unwrap()
        .split(')')
        .next()
        .unwrap();
    // The GUI is what the launcher launches, so the libraries egui opens at
    // start-up cannot be optional in a package that ships the launcher.
    for lib in ["libxkbcommon", "wayland", "libx11", "libglvnd"] {
        assert!(
            depends.contains(lib),
            "the window needs {lib}; it must be a depend"
        );
    }
    for shipped in [
        "openautotyper.desktop",
        "openautotyper.svg",
        "99-openautotyper-uinput.rules",
        "target/release/openautotyper ",
        "target/release/openautotyper-gui",
    ] {
        assert!(pkgbuild.contains(shipped), "{shipped}");
    }
    assert!(pkgbuild.contains("makedepends=('cargo')"));
}

#[test]
fn the_package_builds_what_it_was_locked_to() {
    // --locked/--frozen only mean something with a lockfile to hold to.
    assert!(
        packaging().parent().unwrap().join("Cargo.lock").is_file(),
        "commit Cargo.lock"
    );
    let pkgbuild = pkgbuild();
    assert!(pkgbuild.contains("cargo fetch --locked"));
    assert!(pkgbuild.contains("cargo build --frozen --release"));
}

#[test]
fn the_package_version_matches_the_crate() {
    let manifest =
        std::fs::read_to_string(packaging().parent().unwrap().join("Cargo.toml")).unwrap();
    let version = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = \""))
        .and_then(|v| v.strip_suffix('"'))
        .unwrap();
    assert!(
        pkgbuild().contains(&format!("pkgver={version}\n")),
        "PKGBUILD pkgver != {version}"
    );
}
