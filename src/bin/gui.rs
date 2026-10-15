//! `openautotyper-gui`: the graphical interface, as its own launcher so the
//! desktop entry has something to point at.

fn main() -> std::process::ExitCode {
    openautotyper::gui::run()
}
