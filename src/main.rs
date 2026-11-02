use std::process::ExitCode;

use clap::Parser;
use log::error;

use bluff::cli::{Args, init_logging};

/// Run bluff until interrupted.
///
/// Registers the HID peripheral, opens every keyboard, mouse and touchpad, and toggles between
/// laptop and device on the hotkey. Exiting releases all grabs because the OS drops them with
/// the process.
#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    init_logging();
    let args = Args::parse();
    #[cfg(target_os = "linux")]
    let result = bluff::linux::run(args).await;
    #[cfg(windows)]
    let result = bluff::windows::run(args).await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            error!("{failure}");
            ExitCode::FAILURE
        }
    }
}
