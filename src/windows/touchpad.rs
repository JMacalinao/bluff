//! Precision touchpad contacts through Raw Input, and the touchpad's own scrolling setting.

use std::collections::HashMap;
use std::mem::size_of;

use log::{error, info, warn};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidP_GetCaps, HidP_GetUsageValue,
    HidP_GetUsages, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWHID, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIDI_PREPARSEDDATA, RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SYSTEM_PARAMETERS_INFO_ACTION, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
};

const GENERIC_DESKTOP: u16 = 0x01;
const DIGITIZER: u16 = 0x0D;
const X: u16 = 0x30;
const Y: u16 = 0x31;
const TOUCHPAD: u16 = 0x05;
const TIP_SWITCH: u16 = 0x42;
const CONTACT_COUNT: u16 = 0x54;
const SPI_GETTOUCHPADPARAMETERS: SYSTEM_PARAMETERS_INFO_ACTION =
    SYSTEM_PARAMETERS_INFO_ACTION(0xAE);
const SPI_SETTOUCHPADPARAMETERS: SYSTEM_PARAMETERS_INFO_ACTION =
    SYSTEM_PARAMETERS_INFO_ACTION(0xAF);
const PAN_ENABLED: u32 = 1 << 7; // in the second flag word of TOUCHPAD_PARAMETERS
/// Contact positions go out in hundredths of a millimeter.
pub const UNITS_PER_MM: i32 = 100;

/// Hundredths of a millimeter per logical unit of a position axis.
fn hundredths_per_unit(caps: &HIDP_VALUE_CAPS) -> f64 {
    let exponent = if caps.UnitsExp > 7 {
        caps.UnitsExp as i32 - 16
    } else {
        caps.UnitsExp as i32
    }; // a signed nibble
    let unit_mm = match caps.Units {
        0x11 => 10.0, // centimeter
        0x13 => 25.4, // inch
        _ => 0.0,
    };
    let physical = f64::from(caps.PhysicalMax - caps.PhysicalMin) * 10f64.powi(exponent) * unit_mm;
    let logical = f64::from(caps.LogicalMax - caps.LogicalMin);
    if physical <= 0.0 || logical <= 0.0 {
        // shortcut: guess 30 units per mm, like the Linux touchpad does
        return f64::from(UNITS_PER_MM) / 30.0;
    }
    physical * f64::from(UNITS_PER_MM) / logical
}

struct Device {
    preparsed: Vec<u8>,
    /// Link collections that hold one contact each.
    fingers: Vec<u16>,
    scale: (f64, f64),
}

impl Device {
    fn data(&self) -> PHIDP_PREPARSED_DATA {
        PHIDP_PREPARSED_DATA(self.preparsed.as_ptr() as isize)
    }
}

/// Reads contacts from every precision touchpad's WM_INPUT reports.
#[derive(Default)]
pub struct PrecisionTouchpads {
    devices: HashMap<isize, Option<Device>>,
}

impl PrecisionTouchpads {
    pub fn register(&self, window: HWND) {
        let device = RAWINPUTDEVICE {
            usUsagePage: DIGITIZER,
            usUsage: TOUCHPAD,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        };
        // SAFETY: registers one valid RAWINPUTDEVICE.
        if let Err(failure) =
            unsafe { RegisterRawInputDevices(&[device], size_of::<RAWINPUTDEVICE>() as u32) }
        {
            error!("could not register for touchpad input: {failure}");
        }
    }

    /// Fingers touching and their center in hundredths of a millimeter, from a WM_INPUT lParam.
    pub fn read(&mut self, raw_input: LPARAM) -> Option<(i32, i32, i32)> {
        let handle = HRAWINPUT(raw_input.0 as _);
        let header_size = size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        // SAFETY: the first call only asks for the size; the second fills a buffer of that size.
        let mut data = unsafe {
            GetRawInputData(handle, RID_INPUT, None, &mut size, header_size);
            let mut data = vec![0u8; size as usize];
            if GetRawInputData(
                handle,
                RID_INPUT,
                Some(data.as_mut_ptr().cast()),
                &mut size,
                header_size,
            ) != size
            {
                return None;
            }
            data
        };
        // SAFETY: the buffer starts with a RAWINPUTHEADER and holds a RAWHID after it.
        let (header, hid) = unsafe {
            let header = data.as_ptr().cast::<RAWINPUTHEADER>().read_unaligned();
            let hid = data
                .as_ptr()
                .add(header_size as usize)
                .cast::<RAWHID>()
                .read_unaligned();
            (header, hid)
        };
        let device = self
            .devices
            .entry(header.hDevice.0 as isize)
            .or_insert_with(|| open(header.hDevice))
            .as_ref()?;
        // shortcut: reads only the first report; touchpads that split a frame over reports need them merged.
        let start = header_size as usize + 2 * size_of::<u32>();
        let report = data.get_mut(start..start + hid.dwSizeHid as usize)?;
        Some(contacts(device, report))
    }
}

fn contacts(device: &Device, report: &mut [u8]) -> (i32, i32, i32) {
    let mut count = 0u32;
    // SAFETY: the preparsed data and report buffers stay alive for every HidP call below.
    unsafe {
        if HidP_GetUsageValue(
            HidP_Input,
            DIGITIZER,
            None,
            CONTACT_COUNT,
            &mut count,
            device.data(),
            report,
        ) != HIDP_STATUS_SUCCESS
        {
            return (0, 0, 0);
        }
        let mut touching = Vec::new();
        for &link in device.fingers.iter().take(count as usize) {
            let mut usages = [0u16; 8];
            let mut length = usages.len() as u32;
            let _ = HidP_GetUsages(
                HidP_Input,
                DIGITIZER,
                Some(link),
                usages.as_mut_ptr(),
                &mut length,
                device.data(),
                report,
            );
            if !usages[..length as usize].contains(&TIP_SWITCH) {
                continue;
            }
            let mut position = [0u32; 2];
            for (axis, usage) in position.iter_mut().zip([X, Y]) {
                let _ = HidP_GetUsageValue(
                    HidP_Input,
                    GENERIC_DESKTOP,
                    Some(link),
                    usage,
                    axis,
                    device.data(),
                    report,
                );
            }
            touching.push(position);
        }
        if touching.is_empty() {
            return (0, 0, 0);
        }
        let n = touching.len() as f64;
        let x = touching.iter().map(|p| f64::from(p[0])).sum::<f64>() / n * device.scale.0;
        let y = touching.iter().map(|p| f64::from(p[1])).sum::<f64>() / n * device.scale.1;
        (touching.len() as i32, x.round() as i32, y.round() as i32)
    }
}

fn open(handle: HANDLE) -> Option<Device> {
    let mut size = 0u32;
    // SAFETY: the first call only asks for the size; the second fills a buffer of that size,
    // and the HidP calls read the preparsed data it holds.
    unsafe {
        GetRawInputDeviceInfoW(Some(handle), RIDI_PREPARSEDDATA, None, &mut size);
        let mut preparsed = vec![0u8; size as usize];
        GetRawInputDeviceInfoW(
            Some(handle),
            RIDI_PREPARSEDDATA,
            Some(preparsed.as_mut_ptr().cast()),
            &mut size,
        );
        let data = PHIDP_PREPARSED_DATA(preparsed.as_ptr() as isize);
        let mut caps = HIDP_CAPS::default();
        if HidP_GetCaps(data, &mut caps) != HIDP_STATUS_SUCCESS {
            return None;
        }
        let mut count = caps.NumberInputValueCaps;
        let mut values = vec![HIDP_VALUE_CAPS::default(); usize::from(count)];
        if HidP_GetValueCaps(HidP_Input, values.as_mut_ptr(), &mut count, data)
            != HIDP_STATUS_SUCCESS
        {
            return None;
        }
        let axis = |usage: u16| -> HashMap<u16, &HIDP_VALUE_CAPS> {
            let matching = values
                .iter()
                .filter(|v| v.UsagePage == GENERIC_DESKTOP && v.Anonymous.NotRange.Usage == usage);
            matching.map(|v| (v.LinkCollection, v)).collect()
        };
        let (xs, ys) = (axis(X), axis(Y));
        let mut fingers: Vec<u16> = xs
            .keys()
            .filter(|link| ys.contains_key(link))
            .copied()
            .collect();
        fingers.sort_unstable();
        let first = *fingers.first()?;
        info!("reading touchpad contacts: {} slots", fingers.len());
        let scale = (
            hundredths_per_unit(xs[&first]),
            hundredths_per_unit(ys[&first]),
        );
        Some(Device {
            preparsed,
            fingers,
            scale,
        })
    }
}

/// Turn the touchpad's own two-finger scrolling on or off for this session; return whether it was on.
pub fn set_pan(enabled: bool) -> bool {
    let mut parameters = [0u32; 11]; // TOUCHPAD_PARAMETERS, version 1: 44 bytes
    parameters[0] = 1;
    let size = size_of_val(&parameters) as u32;
    let none = SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0);
    // SAFETY: both calls get a buffer of the size they are told.
    unsafe {
        if SystemParametersInfoW(
            SPI_GETTOUCHPADPARAMETERS,
            size,
            Some(parameters.as_mut_ptr().cast()),
            none,
        )
        .is_err()
        {
            return false;
        }
        let was = parameters[4] & PAN_ENABLED != 0;
        if enabled {
            parameters[4] |= PAN_ENABLED;
        } else {
            parameters[4] &= !PAN_ENABLED;
        }
        // No SPIF_UPDATEINIFILE: the change ends with the session if bluff never restores it.
        if let Err(failure) = SystemParametersInfoW(
            SPI_SETTOUCHPADPARAMETERS,
            size,
            Some(parameters.as_mut_ptr().cast()),
            none,
        ) {
            warn!("could not change touchpad scrolling: {failure}");
        }
        was
    }
}
