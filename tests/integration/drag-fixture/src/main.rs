//! The Linux counterpart of this fixture is `tests/integration/gtk-fixture.py`,
//! which records the same events for the same blue-to-green drag task.

#[cfg(windows)]
mod fixture;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    fixture::run()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("Use tests/integration/gtk-fixture.py on Linux");
    std::process::ExitCode::FAILURE
}
