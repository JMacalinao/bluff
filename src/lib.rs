//! Be a Bluetooth LE keyboard and mouse for a tablet or phone.

/// The name and version the tray menu and the startup log show.
pub const VERSION: &str = concat!("bluff ", env!("CARGO_PKG_VERSION"));

pub mod bridge;
pub mod cli;
pub mod hid;
pub mod icons;
pub mod input;
pub mod keymap;
#[cfg(target_os = "linux")]
pub mod linux;
pub mod scancodes;
pub mod touchpad;
#[cfg(windows)]
pub mod windows;
