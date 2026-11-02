//! Minimal evdev access: device discovery, nonblocking reads and exclusive grab.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Arc;

use log::warn;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc::UnboundedSender;

use crate::bridge::Event;
use crate::input::{
    ABS_X, ABS_Y, BTN_LEFT, BTN_TOOL_FINGER, DeviceInfo, EV_ABS, EV_KEY, EV_REL, EV_REP, Input,
    KEY_A, Kind, REL_X, REL_Y,
};

const EVENT_SIZE: usize = 24; // struct input_event on 64-bit: timeval, u16 type, u16 code, i32 value
const EVIOCGRAB: libc::c_ulong = 0x40044590;
const EVIOCGABS_BASE: libc::c_ulong = 0x80184540; // _IOR('E', 0x40 + axis, struct input_absinfo)

/// A /proc bitmap: 64-bit hex words, most significant first.
fn has(words: Option<&str>, bits: &[u16]) -> bool {
    let words: Vec<u64> = words
        .unwrap_or("0")
        .split_whitespace()
        .rev()
        .map(|word| u64::from_str_radix(word, 16).unwrap_or(0))
        .collect();
    bits.iter().all(|&bit| {
        let word = words.get(usize::from(bit / 64)).copied().unwrap_or(0);
        word >> (bit % 64) & 1 == 1
    })
}

fn classify(fields: &HashMap<&str, &str>) -> Option<Kind> {
    let field = |key| fields.get(key).copied();
    let (ev, key) = (field("EV"), field("KEY"));
    if has(ev, &[EV_REL]) && has(field("REL"), &[REL_X, REL_Y]) {
        return Some(Kind::Mouse);
    }
    if has(ev, &[EV_ABS]) && has(field("ABS"), &[ABS_X, ABS_Y]) {
        return has(key, &[BTN_TOOL_FINGER, BTN_LEFT]).then_some(Kind::Touchpad);
    }
    (has(ev, &[EV_KEY, EV_REP]) && has(key, &[KEY_A])).then_some(Kind::Keyboard)
}

/// Classify the input devices listed in /proc/bus/input/devices.
pub fn parse_devices(text: &str) -> Vec<DeviceInfo> {
    let mut devices = Vec::new();
    for block in text.trim().split("\n\n") {
        let mut fields = HashMap::new();
        let mut name = "";
        for line in block.lines() {
            let (tag, rest) = line.split_once(": ").unwrap_or((line, ""));
            match tag {
                "N" => name = rest.trim_start_matches("Name=\"").trim_end_matches('"'),
                "H" | "B" => {
                    let (key, value) = rest.split_once('=').unwrap_or((rest, ""));
                    fields.insert(key, value);
                }
                _ => {}
            }
        }
        let event = fields
            .get("Handlers")
            .and_then(|h| h.split_whitespace().find(|h| h.starts_with("event")));
        if let (Some(event), Some(kind)) = (event, classify(&fields)) {
            let path = Path::new("/dev/input").join(event);
            devices.push(DeviceInfo {
                name: name.to_string(),
                path,
                kind,
            });
        }
    }
    devices
}

pub fn discover() -> io::Result<Vec<DeviceInfo>> {
    Ok(parse_devices(&fs::read_to_string(
        "/proc/bus/input/devices",
    )?))
}

/// An open evdev node; `start` forwards its events to a channel.
pub struct Device {
    info: DeviceInfo,
    file: Arc<File>,
}

impl Device {
    pub fn open(info: DeviceInfo) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&info.path)?;
        Ok(Self {
            info,
            file: Arc::new(file),
        })
    }

    /// Units per millimeter of an absolute axis, 0 if the device does not say.
    pub fn resolution(&self, axis: u16) -> i32 {
        let mut absinfo = [0i32; 6];
        // SAFETY: EVIOCGABS writes one struct input_absinfo, six i32s, into the buffer.
        let result = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                EVIOCGABS_BASE + libc::c_ulong::from(axis),
                absinfo.as_mut_ptr(),
            )
        };
        if result < 0 { 0 } else { absinfo[5] }
    }

    /// A reader that, once given the device's index, sends its events to `events` in the background.
    pub fn reader(&self) -> io::Result<impl FnOnce(usize, UnboundedSender<Event>) + use<>> {
        // SAFETY: the Arc keeps the fd open for as long as the AsyncFd holds it.
        let fd = unsafe { AsyncFd::register(self.file.clone()) }
            .map_err(|failed| failed.into_parts().1)?;
        let name = self.info.name.clone();
        Ok(move |index, events: UnboundedSender<Event>| {
            tokio::spawn(read(fd, name, index, events));
        })
    }
}

async fn read(fd: AsyncFd<Arc<File>>, name: String, index: usize, events: UnboundedSender<Event>) {
    {
        {
            let mut buf = [0u8; EVENT_SIZE * 64];
            loop {
                let Ok(mut ready) = fd.readable().await else {
                    return;
                };
                match ready.try_io(|fd| fd.get_ref().as_ref().read(&mut buf)) {
                    Ok(Ok(size)) => {
                        for event in buf[..size].chunks_exact(EVENT_SIZE) {
                            let type_ = u16::from_ne_bytes([event[16], event[17]]);
                            let code = u16::from_ne_bytes([event[18], event[19]]);
                            let value = i32::from_ne_bytes(event[20..24].try_into().unwrap());
                            if events.send((index, type_, code, value)).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(Err(_)) => {
                        warn!("{name} went away");
                        return;
                    }
                    Err(_would_block) => {}
                }
            }
        }
    }
}

impl Input for Device {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn grab(&mut self, on: bool) {
        // SAFETY: EVIOCGRAB takes an int by value.
        if unsafe { libc::ioctl(self.file.as_raw_fd(), EVIOCGRAB, libc::c_int::from(on)) } < 0 {
            warn!(
                "could not {} {}: {}",
                if on { "grab" } else { "release" },
                self.info.name,
                io::Error::last_os_error()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const PROC: &str = r#"I: Bus=0019 Vendor=0000 Product=0001 Version=0000
N: Name="Power Button"
H: Handlers=kbd event0
B: EV=3
B: KEY=8000 10000000000000 0

I: Bus=0003 Vendor=0b05 Product=19b6 Version=0110
N: Name="Asus Keyboard"
H: Handlers=sysrq kbd leds event3
B: EV=120013
B: KEY=1000000000007 ff9f207ac14057ff febeffdfffefffff fffffffffffffffe

I: Bus=0018 Vendor=04f3 Product=3282 Version=0100
N: Name="ASUP1208:00 093A:3011 Touchpad"
H: Handlers=mouse1 event7
B: EV=1b
B: KEY=e520 10000 0 0 0 0
B: ABS=2e0800000000003

I: Bus=0003 Vendor=046d Product=c52b Version=0111
N: Name="Logitech USB Receiver"
H: Handlers=mouse2 event9
B: EV=17
B: KEY=ffff0000 0 0 0 0
B: REL=1943
"#;

    #[test]
    fn parse_devices_classifies_by_capability() {
        let found: Vec<(String, PathBuf, Kind)> = parse_devices(PROC)
            .into_iter()
            .map(|d| (d.name, d.path, d.kind))
            .collect();
        assert_eq!(
            found,
            [
                (
                    "Asus Keyboard".into(),
                    "/dev/input/event3".into(),
                    Kind::Keyboard
                ),
                (
                    "ASUP1208:00 093A:3011 Touchpad".into(),
                    "/dev/input/event7".into(),
                    Kind::Touchpad
                ),
                (
                    "Logitech USB Receiver".into(),
                    "/dev/input/event9".into(),
                    Kind::Mouse
                ),
            ]
        );
    }
}
