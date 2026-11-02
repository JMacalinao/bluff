//! Translate absolute touchpad events into relative mouse motion and scrolling.

use crate::input::{
    ABS_X, ABS_Y, BTN_TOOL_DOUBLETAP, BTN_TOOL_FINGER, BTN_TOOL_QUADTAP, BTN_TOOL_TRIPLETAP,
    BTN_TOUCH, EV_ABS, EV_KEY,
};

fn fingers(code: u16) -> Option<u8> {
    match code {
        BTN_TOOL_FINGER => Some(1),
        BTN_TOOL_DOUBLETAP => Some(2),
        BTN_TOOL_TRIPLETAP => Some(3),
        BTN_TOOL_QUADTAP => Some(4),
        _ => None,
    }
}

/// One finger moves the pointer, two fingers scroll, a two-finger click is a right click.
pub struct Touchpad {
    units_per_mm: f64,
    pixels_per_mm: f64,
    mm_per_tick: f64,
    pub fingers: u8,
    pos: [i32; 2],
    last: Option<[i32; 2]>,
    carry: [f64; 2],
}

impl Touchpad {
    /// `resolution` is device units per millimeter from the ABS_X axis info, `pixels_per_mm` the
    /// pointer counts sent per millimeter of finger travel, `mm_per_tick` the finger travel per wheel tick.
    pub fn new(resolution: i32, pixels_per_mm: f64, mm_per_tick: f64) -> Self {
        Self {
            // shortcut: guess when the device reports none
            units_per_mm: if resolution == 0 {
                30.0
            } else {
                f64::from(resolution)
            },
            pixels_per_mm,
            mm_per_tick,
            fingers: 0,
            pos: [0, 0],
            last: None,
            carry: [0.0, 0.0],
        }
    }

    pub fn event(&mut self, type_: u16, code: u16, value: i32) {
        match (type_, code) {
            (EV_ABS, ABS_X | ABS_Y) => self.pos[usize::from(code)] = value,
            (EV_KEY, BTN_TOUCH) => self.restart(),
            (EV_KEY, _) => {
                if let Some(count) = fingers(code) {
                    if value != 0 {
                        self.fingers = count;
                    } else if self.fingers == count {
                        self.fingers = 0;
                    }
                    self.restart();
                }
            }
            _ => {}
        }
    }

    /// HID button bit for a physical click with the current finger count.
    pub fn click_button(&self) -> u8 {
        u8::from(self.fingers == 2)
    }

    /// Close an event frame; return (dx, dy, wheel, pan).
    pub fn sync(&mut self) -> [i32; 4] {
        let Some(last) = self.last.replace(self.pos) else {
            return [0; 4];
        };
        if !matches!(self.fingers, 1 | 2) {
            return [0; 4];
        }
        let mm = [0, 1].map(|i| f64::from(self.pos[i] - last[i]) / self.units_per_mm);
        if self.fingers == 1 {
            let [dx, dy] = self.take(mm.map(|m| m * self.pixels_per_mm));
            return [dx, dy, 0, 0];
        }
        let [pan, down] = self.take(mm.map(|m| m / self.mm_per_tick));
        [0, 0, -down, pan]
    }

    /// Emit whole counts and keep the fractions for the next frame.
    fn take(&mut self, amounts: [f64; 2]) -> [i32; 2] {
        [0, 1].map(|i| {
            let total = self.carry[i] + amounts[i];
            self.carry[i] = total.fract();
            total.trunc() as i32
        })
    }

    fn restart(&mut self) {
        self.last = None;
        self.carry = [0.0, 0.0];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(pad: &mut Touchpad, x: i32, y: i32) -> [i32; 4] {
        pad.event(EV_ABS, ABS_X, x);
        pad.event(EV_ABS, ABS_Y, y);
        pad.sync()
    }

    #[test]
    fn one_finger_moves_without_jump_on_touch() {
        let mut pad = Touchpad::new(10, 10.0, 2.0);
        pad.event(EV_KEY, BTN_TOUCH, 1);
        pad.event(EV_KEY, BTN_TOOL_FINGER, 1);
        assert_eq!(frame(&mut pad, 500, 500), [0, 0, 0, 0]);
        assert_eq!(frame(&mut pad, 510, 495), [10, -5, 0, 0]);
    }

    #[test]
    fn two_fingers_scroll_and_right_click() {
        let mut pad = Touchpad::new(10, 10.0, 2.0);
        pad.event(EV_KEY, BTN_TOOL_DOUBLETAP, 1);
        assert_eq!(pad.click_button(), 1);
        frame(&mut pad, 100, 100);
        assert_eq!(frame(&mut pad, 100, 60), [0, 0, 2, 0]);
    }

    #[test]
    fn fractions_carry_over() {
        let mut pad = Touchpad::new(10, 1.0, 2.0);
        pad.event(EV_KEY, BTN_TOOL_FINGER, 1);
        frame(&mut pad, 0, 0);
        assert_eq!(frame(&mut pad, 6, 0)[0], 0);
        assert_eq!(frame(&mut pad, 12, 0)[0], 1);
    }
}
