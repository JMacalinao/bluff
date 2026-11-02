// A GUI-subsystem exe opens no console window on Windows; the tray icon is its UI.
#![cfg_attr(windows, windows_subsystem = "windows")]

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
    // Run from a terminal, log there. Started any other way there is no console to attach.
    #[cfg(windows)]
    let attached = unsafe {
        windows::Win32::System::Console::AttachConsole(
            windows::Win32::System::Console::ATTACH_PARENT_PROCESS,
        )
    }
    .is_ok();
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
            #[cfg(windows)]
            if !attached {
                show_error(&failure.to_string());
            }
            ExitCode::FAILURE
        }
    }
}

/// Show an error where a user without a console sees it.
#[cfg(windows)]
fn show_error(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::{HSTRING, w};

    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("bluff"),
            MB_OK | MB_ICONERROR,
        )
    };
}
