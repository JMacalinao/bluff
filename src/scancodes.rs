//! Windows keyboard and wheel input in the units Linux input events use.

pub const LLKHF_EXTENDED: u32 = 0x01;
pub const VK_PAUSE: u32 = 0x13;
pub const VK_NUMLOCK: u32 = 0x90;
pub const WHEEL_DELTA: i32 = 120;

/// Linux keycode for a scan code with the E0 prefix.
fn extended(scan: u32) -> Option<u16> {
    Some(match scan {
        0x1C => 96,  // KPENTER
        0x1D => 97,  // RIGHTCTRL
        0x20 => 113, // MUTE
        0x2E => 114, // VOLUMEDOWN
        0x30 => 115, // VOLUMEUP
        0x35 => 98,  // KPSLASH
        0x37 => 99,  // SYSRQ
        0x38 => 100, // RIGHTALT
        0x47 => 102, // HOME
        0x48 => 103, // UP
        0x49 => 104, // PAGEUP
        0x4B => 105, // LEFT
        0x4D => 106, // RIGHT
        0x4F => 107, // END
        0x50 => 108, // DOWN
        0x51 => 109, // PAGEDOWN
        0x52 => 110, // INSERT
        0x53 => 111, // DELETE
        0x5B => 125, // LEFTMETA
        0x5C => 126, // RIGHTMETA
        0x5D => 127, // COMPOSE
        _ => return None,
    })
}

/// The Linux keycode for a key the keyboard hook saw, None for keys bluff does not send.
pub fn keycode(scan: u32, flags: u32, vk: u32) -> Option<u16> {
    // The hook swaps these two: NumLock arrives as E0 45, Pause as a plain 45.
    match vk {
        VK_NUMLOCK => Some(69),
        VK_PAUSE => Some(119),
        _ if flags & LLKHF_EXTENDED != 0 => extended(scan),
        // Plain set-1 scan codes 1 to 88 already equal Linux keycodes.
        _ => (1..=88).contains(&scan).then_some(scan as u16),
    }
}

/// Split a wheel delta into whole notches and the remainder, both keeping the sign.
pub fn wheel_ticks(pending: i32) -> (i32, i32) {
    (pending / WHEEL_DELTA, pending % WHEEL_DELTA)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::keycode as named;

    #[test]
    fn plain_scan_codes_are_linux_keycodes() {
        assert_eq!(keycode(0x1E, 0, 0x41), named("A"));
        assert_eq!(keycode(0x58, 0, 0x7B), named("F12"));
        assert_eq!(keycode(0x70, 0, 0), None);
    }

    #[test]
    fn extended_scan_codes() {
        assert_eq!(keycode(0x5B, LLKHF_EXTENDED, 0x5B), named("LEFTMETA"));
        assert_eq!(keycode(0x48, LLKHF_EXTENDED, 0x26), named("UP"));
        assert_eq!(keycode(0x48, 0, 0x68), named("KP8"));
        // fake shift around NumLock-off navigation keys
        assert_eq!(keycode(0x2A, LLKHF_EXTENDED, 0x10), None);
    }

    #[test]
    fn numlock_and_pause_swap_back() {
        assert_eq!(keycode(0x45, LLKHF_EXTENDED, VK_NUMLOCK), named("NUMLOCK"));
        assert_eq!(keycode(0x45, 0, VK_PAUSE), named("PAUSE"));
    }

    #[test]
    fn wheel_ticks_keep_the_remainder() {
        assert_eq!(wheel_ticks(120), (1, 0));
        assert_eq!(wheel_ticks(50), (0, 50));
        assert_eq!(wheel_ticks(250), (2, 10));
        assert_eq!(wheel_ticks(-130), (-1, -10));
    }
}
