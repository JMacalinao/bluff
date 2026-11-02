//! Route laptop input either to the local compositor or to the paired device.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use log::{info, warn};
use tokio::sync::{mpsc::UnboundedReceiver, watch};
use tokio::time::sleep_until;

use crate::hid::{self, Keyboard, mouse_report};
use crate::input::{
    BTN_LEFT, EV_KEY, EV_REL, EV_SYN, Input, Kind, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y,
};
use crate::touchpad::Touchpad;

/// One input event: device index, type, code and value.
pub type Event = (usize, u16, u16, i32);

// BLE notifies at the connection interval anyway; coalescing keeps the queue short.
pub const MOUSE_INTERVAL: Duration = Duration::from_millis(8);
const KEY_REPEAT: i32 = 2;

fn rel_slot(code: u16) -> Option<usize> {
    match code {
        REL_X => Some(0),
        REL_Y => Some(1),
        REL_WHEEL => Some(2),
        REL_HWHEEL => Some(3),
        _ => None,
    }
}

/// The HID peripheral input goes to: BlueZ on Linux, WinRT on Windows.
pub trait Peripheral {
    fn notify_keyboard(&mut self, report: &[u8]);
    fn notify_mouse(&mut self, report: &[u8]);
    fn connected(&self) -> bool;
    /// Whether a new host may pair now.
    fn pairing(&self) -> bool {
        false
    }
}

/// Reads every input device; while active, grabs them and forwards to the peripheral.
///
/// The owner feeds events to `event`, calls `flush` once `flush_at` passes, and calls
/// `subscription_changed` whenever a host subscribes or unsubscribes.
pub struct Bridge<P: Peripheral> {
    pub peripheral: P,
    /// Linux keycodes that toggle the target when held together and released.
    pub hotkey: HashSet<u16>,
    pub active: bool,
    pub on_change: Box<dyn FnMut() + Send>,
    pixels_per_mm: f64,
    mm_per_tick: f64,
    devices: Vec<Box<dyn Input + Send>>,
    touchpads: HashMap<usize, Touchpad>,
    clicks: HashMap<usize, u8>,
    held: HashSet<u16>,
    toggle_pending: bool,
    keyboard: Keyboard,
    buttons: u8,
    motion: [i32; 4],
    flush_at: Option<Instant>,
    last_flush: Option<Instant>,
}

impl<P: Peripheral> Bridge<P> {
    /// `pixels_per_mm` and `mm_per_tick` scale touchpads, see `Touchpad`.
    pub fn new(peripheral: P, hotkey: HashSet<u16>, pixels_per_mm: f64, mm_per_tick: f64) -> Self {
        Self {
            peripheral,
            hotkey,
            active: false,
            on_change: Box::new(|| {}),
            pixels_per_mm,
            mm_per_tick,
            devices: Vec::new(),
            touchpads: HashMap::new(),
            clicks: HashMap::new(),
            held: HashSet::new(),
            toggle_pending: false,
            keyboard: Keyboard::default(),
            buttons: 0,
            motion: [0; 4],
            flush_at: None,
            last_flush: None,
        }
    }

    /// Take input from `device`; `resolution` is a touchpad's units per mm.
    /// Returns the index to pass to `event` with the device's events.
    pub fn add(&mut self, device: Box<dyn Input + Send>, resolution: i32) -> usize {
        let index = self.devices.len();
        let info = device.info();
        if info.kind == Kind::Touchpad {
            let touchpad = Touchpad::new(resolution, self.pixels_per_mm, self.mm_per_tick);
            self.touchpads.insert(index, touchpad);
        }
        info!(
            "using {} {:?} ({})",
            info.kind,
            info.name,
            info.path.display()
        );
        self.devices.push(device);
        index
    }

    pub fn event(&mut self, device: usize, type_: u16, code: u16, value: i32) {
        let pressed = value != 0;
        if self.devices[device].info().kind == Kind::Keyboard {
            if type_ == EV_KEY && value == KEY_REPEAT {
                self.repeat(code);
            } else if type_ == EV_KEY {
                self.key(code, pressed);
            }
            return;
        }
        if let Some(touchpad) = self.touchpads.get_mut(&device) {
            touchpad.event(type_, code, value);
            if type_ == EV_KEY && code == BTN_LEFT {
                let bit = if pressed {
                    let bit = touchpad.click_button();
                    self.clicks.insert(device, bit);
                    bit
                } else {
                    self.clicks.remove(&device).unwrap_or(0)
                };
                return self.button(bit, pressed);
            }
            if type_ == EV_SYN {
                for (slot, amount) in self.motion.iter_mut().zip(touchpad.sync()) {
                    *slot += amount;
                }
                return self.schedule_flush();
            }
        }
        match type_ {
            EV_KEY => {
                if let Some(bit) = hid::mouse_button(code) {
                    self.button(bit, pressed);
                }
            }
            EV_REL => {
                if let Some(slot) = rel_slot(code) {
                    self.motion[slot] += value;
                    self.schedule_flush();
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, code: u16, pressed: bool) {
        if pressed {
            self.held.insert(code);
            self.toggle_pending |= self.hotkey.is_subset(&self.held);
        } else {
            self.held.remove(&code);
        }
        if self.active && self.keyboard.key(code, pressed) {
            self.peripheral.notify_keyboard(&self.keyboard.report());
        }
        // Switching only once everything is released keeps the compositor from seeing a stuck key.
        if self.toggle_pending && self.held.is_empty() {
            self.toggle_pending = false;
            self.set_active(!self.active);
        }
    }

    /// Type a held key again on each of the laptop's repeat events.
    fn repeat(&mut self, code: u16) {
        // iPadOS does not repeat keys held on bluff, not even Delete, so the laptop's repeat drives it.
        if let Some(reports) = self.active.then(|| self.keyboard.repeat(code)).flatten() {
            for report in reports {
                self.peripheral.notify_keyboard(&report);
            }
        }
    }

    fn button(&mut self, bit: u8, pressed: bool) {
        if pressed {
            self.buttons |= 1 << bit;
        } else {
            self.buttons &= !(1 << bit);
        }
        self.flush();
    }

    fn schedule_flush(&mut self) {
        if self.flush_at.is_none() && self.active && self.motion.iter().any(|&m| m != 0) {
            let now = Instant::now();
            self.flush_at = Some(
                self.last_flush
                    .map_or(now, |last| now.max(last + MOUSE_INTERVAL)),
            );
        }
    }

    /// When the owner must call `flush` to send the motion gathered since the last report.
    pub fn flush_at(&self) -> Option<Instant> {
        self.flush_at
    }

    pub fn flush(&mut self) {
        self.flush_at = None;
        self.last_flush = Some(Instant::now());
        let motion = std::mem::take(&mut self.motion);
        if self.active {
            self.peripheral
                .notify_mouse(&mouse_report(self.buttons, motion));
        }
    }

    /// Grab input for the device, or release it back to the laptop.
    pub fn set_active(&mut self, active: bool) {
        if active && !self.peripheral.connected() {
            warn!("no device subscribed yet; staying on the laptop");
            return;
        }
        if !active {
            self.keyboard.clear();
            self.buttons = 0;
            self.peripheral.notify_keyboard(&self.keyboard.report());
            self.flush();
        }
        for device in &mut self.devices {
            device.grab(active);
        }
        self.active = active;
        info!(
            "input now goes to {}",
            if active { "the device" } else { "the laptop" }
        );
        (self.on_change)();
    }

    pub fn subscription_changed(&mut self) {
        if self.active && !self.peripheral.connected() {
            warn!("device went away; returning input to the laptop");
            self.set_active(false);
        }
        (self.on_change)();
    }
}

/// Lock the bridge; a panic elsewhere must not take input down with it.
pub fn lock<P: Peripheral>(bridge: &Mutex<Bridge<P>>) -> MutexGuard<'_, Bridge<P>> {
    bridge.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Feed input events to the bridge and send motion on time, until `quit` turns true or Ctrl+C.
pub async fn pump<P: Peripheral>(
    bridge: &Mutex<Bridge<P>>,
    mut events: UnboundedReceiver<Event>,
    mut quit: watch::Receiver<bool>,
) {
    loop {
        let flush_at = lock(bridge).flush_at();
        let deadline =
            flush_at.map_or_else(tokio::time::Instant::now, tokio::time::Instant::from_std);
        tokio::select! {
            Some((device, type_, code, value)) = events.recv() => lock(bridge).event(device, type_, code, value),
            () = sleep_until(deadline), if flush_at.is_some() => lock(bridge).flush(),
            _ = quit.changed() => return,
            _ = tokio::signal::ctrl_c() => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::input::DeviceInfo;
    use crate::keymap::keycode;

    #[derive(Default)]
    struct FakePeripheral {
        connected: bool,
        keyboard: Vec<Vec<u8>>,
    }

    impl Peripheral for FakePeripheral {
        fn notify_keyboard(&mut self, report: &[u8]) {
            self.keyboard.push(report.to_vec());
        }
        fn notify_mouse(&mut self, _: &[u8]) {}
        fn connected(&self) -> bool {
            self.connected
        }
    }

    struct FakeDevice {
        info: DeviceInfo,
        grabbed: Arc<AtomicBool>,
    }

    impl Input for FakeDevice {
        fn info(&self) -> &DeviceInfo {
            &self.info
        }
        fn grab(&mut self, on: bool) {
            self.grabbed.store(on, Ordering::Relaxed);
        }
    }

    fn press(bridge: &mut Bridge<FakePeripheral>, device: usize, names: &[&str]) {
        for name in names {
            bridge.event(device, EV_KEY, keycode(name).unwrap(), 1);
        }
        for name in names {
            bridge.event(device, EV_KEY, keycode(name).unwrap(), 0);
        }
    }

    #[test]
    fn hotkey_toggles_after_release_only_when_subscribed() {
        let hotkey = HashSet::from([keycode("LEFTCTRL").unwrap(), keycode("LEFTALT").unwrap()]);
        let mut bridge = Bridge::new(FakePeripheral::default(), hotkey, 10.0, 2.0);
        let grabbed = Arc::new(AtomicBool::new(false));
        let info = DeviceInfo {
            name: "kbd".into(),
            path: PathBuf::from("/dev/null"),
            kind: Kind::Keyboard,
        };
        let device = bridge.add(
            Box::new(FakeDevice {
                info,
                grabbed: grabbed.clone(),
            }),
            0,
        );

        press(&mut bridge, device, &["LEFTCTRL", "LEFTALT"]);
        assert!(!bridge.active);

        bridge.peripheral.connected = true;
        press(&mut bridge, device, &["LEFTCTRL", "LEFTALT"]);
        assert!(bridge.active && grabbed.load(Ordering::Relaxed));

        bridge.peripheral.connected = false;
        bridge.subscription_changed();
        assert!(!bridge.active && !grabbed.load(Ordering::Relaxed));
    }

    #[test]
    fn held_keys_repeat_on_the_device_only_while_active() {
        let mut bridge = Bridge::new(FakePeripheral::default(), HashSet::from([1]), 10.0, 2.0);
        let info = DeviceInfo {
            name: "kbd".into(),
            path: PathBuf::from("/dev/null"),
            kind: Kind::Keyboard,
        };
        let grabbed = Arc::new(AtomicBool::new(false));
        let device = bridge.add(Box::new(FakeDevice { info, grabbed }), 0);
        let backspace = keycode("BACKSPACE").unwrap();
        bridge.event(device, EV_KEY, backspace, 1);
        bridge.event(device, EV_KEY, backspace, 2);
        assert!(bridge.peripheral.keyboard.is_empty());

        bridge.event(device, EV_KEY, backspace, 0);

        bridge.active = true;
        bridge.event(device, EV_KEY, backspace, 1);
        bridge.event(device, EV_KEY, backspace, 2);
        let held = vec![0, 0, 0x2A, 0, 0, 0, 0, 0];
        assert_eq!(bridge.peripheral.keyboard, [held.clone(), vec![0; 8], held]);
    }
}
