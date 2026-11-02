"""Precision touchpad contacts through Raw Input, and the touchpad's own scrolling setting."""
# pyright: reportAttributeAccessIssue=false
# (typeshed declares WinDLL for win32 only, and pyright checks stdlib stubs for the host platform)

import ctypes
import logging
from ctypes import wintypes
from dataclasses import dataclass

logger = logging.getLogger(__name__)

WM_INPUT = 0x00FF
RIDEV_INPUTSINK = 0x100
RID_INPUT = 0x10000003
RIDI_PREPARSEDDATA = 0x20000005
HIDP_INPUT = 0
HIDP_STATUS_SUCCESS = 0x110000
GENERIC_DESKTOP, DIGITIZER = 0x01, 0x0D
X, Y, TOUCHPAD, TIP_SWITCH, CONTACT_COUNT = 0x30, 0x31, 0x05, 0x42, 0x54
SPI_GETTOUCHPADPARAMETERS, SPI_SETTOUCHPADPARAMETERS = 0xAE, 0xAF
PAN_ENABLED = 1 << 7  # in the second flag word of TOUCHPAD_PARAMETERS
UNITS_PER_MM = 100
"""Contact positions go out in hundredths of a millimeter."""

_UNIT_MM = {0x11: 10.0, 0x13: 25.4}  # HID length units: centimeter, inch


class RAWINPUTDEVICE(ctypes.Structure):
    _fields_ = [
        ("usUsagePage", wintypes.USHORT),
        ("usUsage", wintypes.USHORT),
        ("dwFlags", wintypes.DWORD),
        ("hwndTarget", wintypes.HWND),
    ]


class RAWINPUTHEADER(ctypes.Structure):
    _fields_ = [
        ("dwType", wintypes.DWORD),
        ("dwSize", wintypes.DWORD),
        ("hDevice", wintypes.HANDLE),
        ("wParam", wintypes.WPARAM),
    ]


class RAWHID(ctypes.Structure):
    _fields_ = [("dwSizeHid", wintypes.DWORD), ("dwCount", wintypes.DWORD)]


class HIDP_CAPS(ctypes.Structure):
    _fields_ = [
        ("Usage", wintypes.USHORT),
        ("UsagePage", wintypes.USHORT),
        ("InputReportByteLength", wintypes.USHORT),
        ("OutputReportByteLength", wintypes.USHORT),
        ("FeatureReportByteLength", wintypes.USHORT),
        ("Reserved", wintypes.USHORT * 17),
        ("NumberLinkCollectionNodes", wintypes.USHORT),
        ("NumberInputButtonCaps", wintypes.USHORT),
        ("NumberInputValueCaps", wintypes.USHORT),
        ("NumberInputDataIndices", wintypes.USHORT),
        ("Rest", wintypes.USHORT * 6),
    ]


class HIDP_VALUE_CAPS(ctypes.Structure):
    _fields_ = [
        ("UsagePage", wintypes.USHORT),
        ("ReportID", ctypes.c_ubyte),
        ("IsAlias", ctypes.c_ubyte),
        ("BitField", wintypes.USHORT),
        ("LinkCollection", wintypes.USHORT),
        ("LinkUsage", wintypes.USHORT),
        ("LinkUsagePage", wintypes.USHORT),
        ("IsRange", ctypes.c_ubyte),
        ("IsStringRange", ctypes.c_ubyte),
        ("IsDesignatorRange", ctypes.c_ubyte),
        ("IsAbsolute", ctypes.c_ubyte),
        ("HasNull", ctypes.c_ubyte),
        ("Reserved", ctypes.c_ubyte),
        ("BitSize", wintypes.USHORT),
        ("ReportCount", wintypes.USHORT),
        ("Reserved2", wintypes.USHORT * 5),
        ("UnitsExp", wintypes.ULONG),
        ("Units", wintypes.ULONG),
        ("LogicalMin", wintypes.LONG),
        ("LogicalMax", wintypes.LONG),
        ("PhysicalMin", wintypes.LONG),
        ("PhysicalMax", wintypes.LONG),
        ("Usage", wintypes.USHORT),
        ("Rest", wintypes.USHORT * 7),
    ]


_user32 = ctypes.WinDLL("user32", use_last_error=True)
_user32.RegisterRawInputDevices.argtypes = [ctypes.POINTER(RAWINPUTDEVICE), wintypes.UINT, wintypes.UINT]
_user32.GetRawInputData.argtypes = [
    wintypes.HANDLE, wintypes.UINT, ctypes.c_void_p, ctypes.POINTER(wintypes.UINT), wintypes.UINT,
]  # fmt: skip
_user32.GetRawInputDeviceInfoW.argtypes = [
    wintypes.HANDLE,
    wintypes.UINT,
    ctypes.c_void_p,
    ctypes.POINTER(wintypes.UINT),
]
_user32.SystemParametersInfoW.argtypes = [wintypes.UINT, wintypes.UINT, ctypes.c_void_p, wintypes.UINT]
_hid = ctypes.WinDLL("hid")
_hid.HidP_GetCaps.argtypes = [ctypes.c_void_p, ctypes.POINTER(HIDP_CAPS)]
_hid.HidP_GetCaps.restype = wintypes.LONG
_hid.HidP_GetValueCaps.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.POINTER(wintypes.USHORT), ctypes.c_void_p]
_hid.HidP_GetValueCaps.restype = wintypes.LONG
_hid.HidP_GetUsageValue.argtypes = [
    ctypes.c_int, wintypes.USHORT, wintypes.USHORT, wintypes.USHORT,
    ctypes.POINTER(wintypes.ULONG), ctypes.c_void_p, ctypes.c_void_p, wintypes.ULONG,
]  # fmt: skip
_hid.HidP_GetUsageValue.restype = wintypes.LONG
_hid.HidP_GetUsages.argtypes = [
    ctypes.c_int, wintypes.USHORT, wintypes.USHORT, ctypes.POINTER(wintypes.USHORT),
    ctypes.POINTER(wintypes.ULONG), ctypes.c_void_p, ctypes.c_void_p, wintypes.ULONG,
]  # fmt: skip
_hid.HidP_GetUsages.restype = wintypes.LONG


def hundredths_per_unit(caps: HIDP_VALUE_CAPS) -> float:
    """Hundredths of a millimeter per logical unit of a position axis."""
    exponent = caps.UnitsExp - 16 if caps.UnitsExp > 7 else caps.UnitsExp  # a signed nibble
    physical = (caps.PhysicalMax - caps.PhysicalMin) * 10.0**exponent * _UNIT_MM.get(caps.Units, 0)
    logical = caps.LogicalMax - caps.LogicalMin
    if physical <= 0 or logical <= 0:
        return UNITS_PER_MM / 30  # shortcut: guess 30 units per mm, like the Linux touchpad does
    return physical * UNITS_PER_MM / logical


@dataclass
class _Device:
    preparsed: ctypes.Array[ctypes.c_char]
    fingers: list[int]
    """Link collections that hold one contact each."""
    scale: tuple[float, float]


class PrecisionTouchpads:
    """Reads contacts from every precision touchpad's WM_INPUT reports."""

    def __init__(self) -> None:
        self._devices: dict[int, _Device | None] = {}

    def register(self, window: int) -> None:
        device = RAWINPUTDEVICE(DIGITIZER, TOUCHPAD, RIDEV_INPUTSINK, window)
        if not _user32.RegisterRawInputDevices(ctypes.byref(device), 1, ctypes.sizeof(device)):
            logger.error("could not register for touchpad input: error %d", ctypes.get_last_error())

    def read(self, raw_input: int) -> tuple[int, int, int] | None:
        """Fingers touching and their center in hundredths of a millimeter, from a WM_INPUT lParam."""
        header_size = ctypes.sizeof(RAWINPUTHEADER)
        size = wintypes.UINT()
        _user32.GetRawInputData(raw_input, RID_INPUT, None, ctypes.byref(size), header_size)
        data = ctypes.create_string_buffer(size.value)
        if _user32.GetRawInputData(raw_input, RID_INPUT, data, ctypes.byref(size), header_size) != size.value:
            return None
        header = RAWINPUTHEADER.from_buffer(data)
        device = self._device(header.hDevice or 0)
        if device is None:
            return None
        hid = RAWHID.from_buffer(data, header_size)
        # shortcut: reads only the first report; touchpads that split a frame over reports need them merged.
        report = ctypes.addressof(data) + header_size + ctypes.sizeof(RAWHID)
        return self._contacts(device, report, hid.dwSizeHid)

    def _contacts(self, device: _Device, report: int, length: int) -> tuple[int, int, int]:
        value = wintypes.ULONG()
        get = _hid.HidP_GetUsageValue
        if get(HIDP_INPUT, DIGITIZER, 0, CONTACT_COUNT, ctypes.byref(value), device.preparsed, report, length) != (
            HIDP_STATUS_SUCCESS
        ):
            return 0, 0, 0
        touching = []
        for link in device.fingers[: value.value]:
            usages = (wintypes.USHORT * 8)()
            count = wintypes.ULONG(len(usages))
            _hid.HidP_GetUsages(
                HIDP_INPUT, DIGITIZER, link, usages, ctypes.byref(count), device.preparsed, report, length
            )
            if TIP_SWITCH not in usages[: count.value]:
                continue
            position = []
            for usage in (X, Y):
                get(HIDP_INPUT, GENERIC_DESKTOP, link, usage, ctypes.byref(value), device.preparsed, report, length)
                position.append(value.value)
            touching.append(position)
        if not touching:
            return 0, 0, 0
        x = sum(p[0] for p in touching) / len(touching) * device.scale[0]
        y = sum(p[1] for p in touching) / len(touching) * device.scale[1]
        return len(touching), round(x), round(y)

    def _device(self, handle: int) -> _Device | None:
        if handle not in self._devices:
            self._devices[handle] = self._open(handle)
        return self._devices[handle]

    def _open(self, handle: int) -> _Device | None:
        size = wintypes.UINT()
        _user32.GetRawInputDeviceInfoW(handle, RIDI_PREPARSEDDATA, None, ctypes.byref(size))
        preparsed = ctypes.create_string_buffer(size.value)
        _user32.GetRawInputDeviceInfoW(handle, RIDI_PREPARSEDDATA, preparsed, ctypes.byref(size))
        caps = HIDP_CAPS()
        if _hid.HidP_GetCaps(preparsed, ctypes.byref(caps)) != HIDP_STATUS_SUCCESS:
            return None
        count = wintypes.USHORT(caps.NumberInputValueCaps)
        values = (HIDP_VALUE_CAPS * count.value)()
        if _hid.HidP_GetValueCaps(HIDP_INPUT, values, ctypes.byref(count), preparsed) != HIDP_STATUS_SUCCESS:
            return None
        xs = {v.LinkCollection: v for v in values if (v.UsagePage, v.Usage) == (GENERIC_DESKTOP, X)}
        ys = {v.LinkCollection: v for v in values if (v.UsagePage, v.Usage) == (GENERIC_DESKTOP, Y)}
        fingers = sorted(xs.keys() & ys.keys())
        if not fingers:
            return None
        first = fingers[0]
        logger.info("reading touchpad contacts: %d slots", len(fingers))
        return _Device(preparsed, fingers, (hundredths_per_unit(xs[first]), hundredths_per_unit(ys[first])))


def set_pan(enabled: bool) -> bool:
    """Turn the touchpad's own two-finger scrolling on or off for this session; return whether it was on."""
    parameters = (wintypes.UINT * 11)(1)  # TOUCHPAD_PARAMETERS, version 1: 44 bytes
    size = ctypes.sizeof(parameters)
    if not _user32.SystemParametersInfoW(SPI_GETTOUCHPADPARAMETERS, size, parameters, 0):
        return False
    was = bool(parameters[4] & PAN_ENABLED)
    parameters[4] = parameters[4] | PAN_ENABLED if enabled else parameters[4] & ~PAN_ENABLED
    # No SPIF_UPDATEINIFILE: the change ends with the session if bluff never restores it.
    if not _user32.SystemParametersInfoW(SPI_SETTOUCHPADPARAMETERS, size, parameters, 0):
        logger.warning("could not change touchpad scrolling: error %d", ctypes.get_last_error())
    return was
