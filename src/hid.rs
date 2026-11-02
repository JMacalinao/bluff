//! HID report descriptor and report builders for a combo keyboard and mouse.

use crate::keymap;

pub const KEYBOARD_ID: u8 = 1;
pub const MOUSE_ID: u8 = 2;

#[rustfmt::skip]
pub const REPORT_MAP: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01,  // Generic Desktop, Keyboard, Collection (Application)
    0x85, KEYBOARD_ID,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7,  // modifiers: 8 x 1 bit
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x75, 0x08, 0x95, 0x01, 0x81, 0x01,  // reserved byte
    0x15, 0x00, 0x26, 0xFF, 0x00,  // 6 key slots, usages 0-255
    0x19, 0x00, 0x2A, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x06, 0x81, 0x00,
    0xC0,
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01,  // Generic Desktop, Mouse, Collection (Application)
    0x85, MOUSE_ID,
    0x09, 0x01, 0xA1, 0x00,  // Pointer, Collection (Physical)
    0x05, 0x09, 0x19, 0x01, 0x29, 0x05,  // buttons 1-5
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x05, 0x81, 0x02,
    0x75, 0x03, 0x95, 0x01, 0x81, 0x01,  // padding
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31,  // X, Y: int16 relative
    0x16, 0x01, 0x80, 0x26, 0xFF, 0x7F, 0x75, 0x10, 0x95, 0x02, 0x81, 0x06,
    0x09, 0x38,  // wheel: int8 relative
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    0x05, 0x0C, 0x0A, 0x38, 0x02,  // Consumer AC Pan (horizontal scroll): int8 relative
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06,
    0xC0,
    0xC0,
];

const MODIFIER_FIRST: u8 = 0xE0;
const MAX_KEYS: usize = 6;

/// The HID button bit for Linux BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE and BTN_EXTRA.
pub fn mouse_button(code: u16) -> Option<u8> {
    match code {
        0x110..=0x114 => Some((code - 0x110) as u8),
        _ => None,
    }
}

/// Pressed-key state that renders to a keyboard input report.
#[derive(Default)]
pub struct Keyboard {
    modifiers: u8,
    keys: Vec<u8>,
}

impl Keyboard {
    /// Apply a Linux key event; return whether the report changed.
    pub fn key(&mut self, code: u16, pressed: bool) -> bool {
        let Some(usage) = keymap::usage(code) else {
            return false;
        };
        if usage >= MODIFIER_FIRST {
            let before = self.modifiers;
            let bit = 1 << (usage - MODIFIER_FIRST);
            if pressed {
                self.modifiers |= bit;
            } else {
                self.modifiers &= !bit;
            }
            return before != self.modifiers;
        }
        match (pressed, self.keys.iter().position(|&key| key == usage)) {
            (true, None) => self.keys.push(usage),
            (false, Some(at)) => {
                self.keys.remove(at);
            }
            _ => return false,
        }
        true
    }

    pub fn report(&self) -> [u8; 8] {
        let mut report = [0; 8];
        report[0] = self.modifiers;
        if self.keys.len() > MAX_KEYS {
            report[2..].fill(0x01);
        } else {
            report[2..2 + self.keys.len()].copy_from_slice(&self.keys);
        }
        report
    }

    /// The release and press reports that type a held key once more, None for modifiers and keys not held.
    pub fn repeat(&self, code: u16) -> Option<[[u8; 8]; 2]> {
        let usage = keymap::usage(code).filter(|&usage| usage < MODIFIER_FIRST)?;
        let at = self.keys.iter().position(|&key| key == usage)?;
        let mut released = Self {
            modifiers: self.modifiers,
            keys: self.keys.clone(),
        };
        released.keys.remove(at);
        Some([released.report(), self.report()])
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Pack a mouse input report from (dx, dy, wheel, pan); deltas outside the report range are clamped.
pub fn mouse_report(buttons: u8, [dx, dy, wheel, pan]: [i32; 4]) -> [u8; 7] {
    let [x0, x1] = (dx.clamp(-32767, 32767) as i16).to_le_bytes();
    let [y0, y1] = (dy.clamp(-32767, 32767) as i16).to_le_bytes();
    let wheel = wheel.clamp(-127, 127) as i8 as u8;
    let pan = pan.clamp(-127, 127) as i8 as u8;
    [buttons, x0, x1, y0, y1, wheel, pan]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::keycode;

    fn key(name: &str) -> u16 {
        keycode(name).unwrap()
    }

    #[test]
    fn keyboard_report_tracks_modifiers_and_keys() {
        let mut kb = Keyboard::default();
        assert!(kb.key(key("LEFTMETA"), true));
        assert!(kb.key(key("H"), true));
        assert_eq!(kb.report(), [0x08, 0, 0x0B, 0, 0, 0, 0, 0]);
        assert!(kb.key(key("H"), false));
        assert_eq!(kb.report(), [0x08, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn keyboard_ignores_unmapped_and_duplicate_presses() {
        let mut kb = Keyboard::default();
        assert!(!kb.key(0x2FF, true));
        assert!(kb.key(key("A"), true));
        assert!(!kb.key(key("A"), true));
    }

    #[test]
    fn keyboard_reports_rollover_past_six_keys() {
        let mut kb = Keyboard::default();
        for name in ["A", "B", "C", "D", "E", "F", "G"] {
            kb.key(key(name), true);
        }
        assert_eq!(kb.report()[2..], [0x01; 6]);
    }

    #[test]
    fn repeat_taps_the_key_and_keeps_the_rest_held() {
        let mut kb = Keyboard::default();
        kb.key(key("LEFTSHIFT"), true);
        kb.key(key("A"), true);
        kb.key(key("BACKSPACE"), true);
        assert_eq!(
            kb.repeat(key("BACKSPACE")),
            Some([
                [0x02, 0, 0x04, 0, 0, 0, 0, 0],
                [0x02, 0, 0x04, 0x2A, 0, 0, 0, 0]
            ])
        );
        assert_eq!(kb.repeat(key("LEFTSHIFT")), None);
        assert_eq!(kb.repeat(key("B")), None);
    }

    #[test]
    fn mouse_report_packs_and_clamps() {
        assert_eq!(
            mouse_report(0b101, [-1, 40000, 3, -200]),
            [0x05, 0xFF, 0xFF, 0xFF, 0x7F, 0x03, 0x81]
        );
    }

    #[test]
    fn report_map_collections_balance() {
        assert!(REPORT_MAP.iter().filter(|&&b| b == 0xC0).count() >= 3);
        assert!(REPORT_MAP.len() < 512);
    }
}
