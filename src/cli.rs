//! Command-line options and logging setup.

use std::collections::HashSet;

use clap::Parser;

use crate::keymap::keycode;

pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// Be a Bluetooth keyboard and mouse for a tablet or phone.
#[derive(Parser)]
#[command(name = "bluff")]
pub struct Args {
    /// name the device shows when pairing
    #[arg(long, default_value = "bluff")]
    pub name: String,
    #[arg(long, default_value = "hci0")]
    pub adapter: String,
    /// be discoverable and confirm pairing on this terminal
    #[arg(long)]
    pub pair: bool,
    /// toggle chord
    #[arg(long, value_parser = parse_hotkey, default_value = "LEFTCTRL+LEFTALT+LEFTMETA")]
    pub hotkey: HashSet<u16>,
    /// touchpad pointer speed
    #[arg(long, default_value_t = 10.0)]
    pub pixels_per_mm: f64,
    /// touchpad finger travel per scroll step
    #[arg(long, default_value_t = 2.0)]
    pub mm_per_tick: f64,
}

/// Turn `LEFTCTRL+LEFTALT+LEFTMETA` into Linux keycodes.
pub fn parse_hotkey(spec: &str) -> Result<HashSet<u16>, String> {
    spec.split('+')
        .map(|name| {
            let name = name.trim().to_uppercase();
            keycode(&name).ok_or(format!(
                "unknown key {name}; use names like LEFTCTRL, A, F12"
            ))
        })
        .collect()
}

pub fn init_logging() {
    let config = serde_yaml::from_str(include_str!("logging.yaml")).expect("logging.yaml is valid");
    log4rs::config::init_raw_config(config).expect("logging is set up once");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_names_become_keycodes() {
        assert_eq!(parse_hotkey("leftctrl+ A"), Ok(HashSet::from([29, 30])));
        assert!(parse_hotkey("LEFTCTRL+NOPE").unwrap_err().contains("NOPE"));
    }

    #[test]
    fn logging_config_parses() {
        serde_yaml::from_str::<log4rs::config::RawConfig>(include_str!("logging.yaml")).unwrap();
    }
}
