use std::process::ExitCode;
use std::sync::Arc;

use dcu_core::{Backend, Error};

const VERSION: &str = "0.1.0";
const USAGE: &str = "desktop-computer-use-native --serve --endpoint PATH --token-file PATH";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let arguments: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    {
        dcu_windows::enable_per_monitor_dpi_awareness();
        if arguments.len() == 4 && arguments[1] == "--input-watchdog" {
            let parent_pid = arguments[2]
                .parse::<u32>()
                .map_err(|_| Error::invalid_argument("Invalid watchdog parent process id"))?;
            std::process::exit(dcu_windows::run_input_watchdog(parent_pid, &arguments[3]));
        }
    }

    let mut serve = false;
    let mut endpoint = String::new();
    let mut token_file = String::new();
    let mut remaining = arguments.iter().skip(1);
    while let Some(argument) = remaining.next() {
        match argument.as_str() {
            "--version" => {
                println!("{VERSION}");
                return Ok(());
            }
            "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            "--serve" => serve = true,
            "--endpoint" if remaining.len() > 0 => {
                endpoint = remaining.next().cloned().unwrap_or_default()
            }
            "--token-file" if remaining.len() > 0 => {
                token_file = remaining.next().cloned().unwrap_or_default()
            }
            _ => {
                return Err(Error::invalid_argument(format!(
                    "Unknown or incomplete native argument: {argument}"
                )));
            }
        }
    }
    if !serve || endpoint.is_empty() || token_file.is_empty() {
        return Err(Error::invalid_argument("Use --help for daemon arguments"));
    }
    let token = read_token(&token_file)?;

    // WGC/UIA objects outlive individual connection worker threads; keep their
    // shared multithreaded apartment alive until the backend is dropped.
    #[cfg(windows)]
    let _apartment = dcu_windows::RuntimeApartment::initialize()?;
    dcu_core::serve(&endpoint, &token, make_backend()?)
}

fn read_token(path: &str) -> Result<String, Error> {
    let invalid = || {
        Error::new(
            "invalid_environment",
            "Missing or invalid private token file",
        )
    };
    let contents = std::fs::read_to_string(path).map_err(|_| invalid())?;
    let first_line = contents.split('\n').next().unwrap_or_default();
    let token = first_line.strip_suffix('\r').unwrap_or(first_line);
    if !(32..=256).contains(&token.len()) {
        return Err(invalid());
    }
    Ok(token.to_owned())
}

#[cfg(windows)]
fn make_backend() -> Result<Arc<dyn Backend>, Error> {
    dcu_windows::make_backend()
}

#[cfg(target_os = "linux")]
fn make_backend() -> Result<Arc<dyn Backend>, Error> {
    dcu_linux::make_backend()
}
