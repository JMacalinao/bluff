//! Low-level keyboard and mouse hooks that turn Windows input into Linux input events.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;

use log::error;
use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DispatchMessageW, GetCursorPos, GetMessageW, HWND_MESSAGE,
    KBDLLHOOKSTRUCT, LLKHF_INJECTED, LLMHF_INJECTED, MSG, MSLLHOOKSTRUCT, SetWindowsHookExW,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WINDOW_EX_STYLE, WINDOW_STYLE, WM_INPUT, WM_KEYDOWN,
    WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_SYSKEYDOWN, WM_XBUTTONDOWN, WM_XBUTTONUP,
};
use windows::core::w;

use crate::bridge::Event;
use crate::input::{
    ABS_X, ABS_Y, BTN_LEFT, BTN_TOOL_DOUBLETAP, DeviceInfo, EV_ABS, EV_KEY, EV_REL, EV_SYN, Input,
    Kind, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y,
};
use crate::scancodes::{keycode, wheel_ticks};
use crate::windows::touchpad::{PrecisionTouchpads, set_pan};

const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const SYN_REPORT: u16 = 0;

fn button(message: u32) -> Option<(u16, i32)> {
    Some(match message {
        0x201 => (BTN_LEFT, 1),
        0x202 => (BTN_LEFT, 0),
        0x204 => (BTN_RIGHT, 1),
        0x205 => (BTN_RIGHT, 0),
        0x207 => (BTN_MIDDLE, 1),
        0x208 => (BTN_MIDDLE, 0),
        _ => return None,
    })
}

/// Whether input is kept from Windows, shared by every side of the hooks.
#[derive(Default)]
pub struct Grab {
    grabbed: AtomicBool,
    two_fingers: AtomicBool,
    pan_was_on: AtomicBool,
}

impl Grab {
    fn set(&self, on: bool) {
        if self.grabbed.swap(on, Ordering::SeqCst) == on {
            return;
        }
        self.two_fingers.store(false, Ordering::SeqCst);
        if on {
            self.pan_was_on.store(set_pan(false), Ordering::SeqCst);
        } else if self.pan_was_on.load(Ordering::SeqCst) {
            set_pan(true);
        }
    }

    fn on(&self) -> bool {
        self.grabbed.load(Ordering::SeqCst)
    }
}

/// One side of the hooks as the bridge sees it; grabbing any side grabs all.
pub struct HookDevice {
    info: DeviceInfo,
    grab: Arc<Grab>,
}

impl Input for HookDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn grab(&mut self, on: bool) {
        self.grab.set(on);
    }
}

/// The keyboard, mouse and touchpad sides of the hooks, in that order.
pub fn devices(grab: &Arc<Grab>) -> [HookDevice; 3] {
    let device = |name: &str, path: &str, kind| HookDevice {
        info: DeviceInfo {
            name: name.into(),
            path: PathBuf::from(path),
            kind,
        },
        grab: grab.clone(),
    };
    [
        device("keyboard hook", "WH_KEYBOARD_LL", Kind::Keyboard),
        device("mouse hook", "WH_MOUSE_LL", Kind::Mouse),
        device("precision touchpad", "Raw Input", Kind::Touchpad),
    ]
}

struct Hooks {
    grab: Arc<Grab>,
    events: UnboundedSender<Event>,
    /// Bridge indices of the keyboard, mouse and touchpad sides.
    indices: [usize; 3],
    down: Mutex<HashSet<u16>>,
    wheel: Mutex<[i32; 2]>,
}

// Hook procedures get no context pointer, so they find the hooks here.
static HOOKS: OnceLock<Hooks> = OnceLock::new();

impl Hooks {
    fn emit(&self, side: usize, type_: u16, code: u16, value: i32) {
        let _ = self.events.send((self.indices[side], type_, code, value));
    }

    fn touch(&self, fingers: i32, x: i32, y: i32) {
        // Only two-finger scrolling: the mouse hook already moves the pointer for one finger.
        let two = fingers == 2;
        if self.grab.two_fingers.swap(two, Ordering::SeqCst) != two {
            self.emit(2, EV_KEY, BTN_TOOL_DOUBLETAP, i32::from(two));
        }
        if two {
            self.emit(2, EV_ABS, ABS_X, x);
            self.emit(2, EV_ABS, ABS_Y, y);
            self.emit(2, EV_SYN, SYN_REPORT, 0);
        }
    }
}

/// Install the hooks on their own thread, feeding Linux events for the sides at `indices` to `events`.
///
/// Keys always reach the bridge so the hotkey works; the mouse only while
/// grabbed. While grabbed, Windows sees no input at all except injected input
/// and Ctrl+Alt+Del. Low-level hooks never see input meant for an elevated
/// window unless bluff runs elevated too.
///
/// Precision touchpads scroll without going through the mouse hook, so two-finger
/// contacts come from Raw Input instead, and the touchpad's own scrolling is off
/// while grabbed.
pub fn start(grab: Arc<Grab>, indices: [usize; 3], events: UnboundedSender<Event>) {
    let hooks = Hooks {
        grab,
        events,
        indices,
        down: Mutex::default(),
        wheel: Mutex::default(),
    };
    if HOOKS.set(hooks).is_err() {
        error!("input hooks are already running");
        return;
    }
    thread::Builder::new()
        .name("input hooks".into())
        .spawn(run)
        .expect("spawn the hook thread");
}

fn run() {
    let hooks = HOOKS.get().expect("set before the thread starts");
    let mut touchpads = PrecisionTouchpads::default();
    // SAFETY: plain Win32 calls on this thread; the hook procedures live for the whole program.
    unsafe {
        for (kind, proc) in [(WH_KEYBOARD_LL, on_key as _), (WH_MOUSE_LL, on_mouse as _)] {
            if let Err(failure) = SetWindowsHookExW(kind, Some(proc), None, 0) {
                error!("could not install input hook {}: {failure}", kind.0);
            }
        }
        let none = (WINDOW_EX_STYLE(0), WINDOW_STYLE(0));
        match CreateWindowExW(
            none.0,
            w!("STATIC"),
            None,
            none.1,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            None,
            None,
        ) {
            Ok(window) => touchpads.register(window),
            Err(failure) => error!("could not create the touchpad window: {failure}"),
        }
        // Windows calls low-level hooks from this thread's message loop, and posts WM_INPUT to it.
        let mut message = MSG::default();
        while GetMessageW(&mut message, None::<HWND>, 0, 0).0 > 0 {
            if message.message == WM_INPUT && hooks.grab.on() {
                if let Some((fingers, x, y)) = touchpads.read(message.lParam) {
                    hooks.touch(fingers, x, y);
                }
            }
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let hooks = HOOKS.get().expect("set before the hooks");
    // SAFETY: for WH_KEYBOARD_LL, lparam points at a KBDLLHOOKSTRUCT.
    let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
    if code < 0 || event.flags.0 & LLKHF_INJECTED.0 != 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    if let Some(key) = keycode(event.scanCode, event.flags.0, event.vkCode) {
        let mut down = hooks.down.lock().unwrap_or_else(PoisonError::into_inner);
        let value = if matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN) {
            if down.insert(key) { 1 } else { 2 }
        } else {
            down.remove(&key);
            0
        };
        hooks.emit(0, EV_KEY, key, value);
    }
    if hooks.grab.on() {
        return LRESULT(1);
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

unsafe extern "system" fn on_mouse(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let hooks = HOOKS.get().expect("set before the hooks");
    // SAFETY: for WH_MOUSE_LL, lparam points at an MSLLHOOKSTRUCT.
    let event = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
    if code < 0 || !hooks.grab.on() || event.flags & LLMHF_INJECTED != 0 {
        return unsafe { CallNextHookEx(None, code, wparam, lparam) };
    }
    let message = wparam.0 as u32;
    let high = (event.mouseData >> 16) as u16;
    if message == WM_MOUSEMOVE {
        // The cursor stays put while grabbed, so the distance to it is the motion, ballistics included.
        let mut cursor = POINT::default();
        // SAFETY: writes one POINT.
        if unsafe { GetCursorPos(&mut cursor) }.is_ok() {
            for (axis, delta) in [
                (REL_X, event.pt.x - cursor.x),
                (REL_Y, event.pt.y - cursor.y),
            ] {
                if delta != 0 {
                    hooks.emit(1, EV_REL, axis, delta);
                }
            }
        }
    } else if let Some((code, value)) = button(message) {
        hooks.emit(1, EV_KEY, code, value);
    } else if matches!(message, WM_XBUTTONDOWN | WM_XBUTTONUP) {
        if let Some(code) = [None, Some(BTN_SIDE), Some(BTN_EXTRA)]
            .get(usize::from(high))
            .copied()
            .flatten()
        {
            hooks.emit(1, EV_KEY, code, i32::from(message == WM_XBUTTONDOWN));
        }
    } else if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
        // Precision touchpads send fractions of a notch; keep the rest for the next event.
        let axis = usize::from(message == WM_MOUSEHWHEEL);
        let mut wheel = hooks.wheel.lock().unwrap_or_else(PoisonError::into_inner);
        let (ticks, rest) = wheel_ticks(wheel[axis] + i32::from(high as i16));
        wheel[axis] = rest;
        if ticks != 0 {
            hooks.emit(1, EV_REL, [REL_WHEEL, REL_HWHEEL][axis], ticks);
        }
    }
    LRESULT(1)
}
