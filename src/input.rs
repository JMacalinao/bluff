//! Linux input event codes and the devices bluff reads them from.

use std::fmt;
use std::path::PathBuf;

pub const EV_SYN: u16 = 0;
pub const EV_KEY: u16 = 1;
pub const EV_REL: u16 = 2;
pub const EV_ABS: u16 = 3;
pub const EV_REP: u16 = 0x14;
pub const REL_X: u16 = 0;
pub const REL_Y: u16 = 1;
pub const REL_HWHEEL: u16 = 6;
pub const REL_WHEEL: u16 = 8;
pub const ABS_X: u16 = 0;
pub const ABS_Y: u16 = 1;
pub const KEY_A: u16 = 30;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_TOOL_FINGER: u16 = 0x145;
pub const BTN_TOUCH: u16 = 0x14A;
pub const BTN_TOOL_DOUBLETAP: u16 = 0x14D;
pub const BTN_TOOL_TRIPLETAP: u16 = 0x14E;
pub const BTN_TOOL_QUADTAP: u16 = 0x14F;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Keyboard,
    Mouse,
    Touchpad,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Keyboard => "keyboard",
            Kind::Mouse => "mouse",
            Kind::Touchpad => "touchpad",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub path: PathBuf,
    pub kind: Kind,
}

/// A source of Linux input events that can be kept from the laptop.
pub trait Input {
    fn info(&self) -> &DeviceInfo;
    fn grab(&mut self, on: bool);
}
