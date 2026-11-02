"""Low-level keyboard and mouse hooks that turn Windows input into Linux input events."""
# pyright: reportAttributeAccessIssue=false
# (typeshed declares WinDLL and WINFUNCTYPE for win32 only, and pyright checks stdlib stubs for the host platform)

import asyncio
import ctypes
import logging
import threading
from collections.abc import Callable
from ctypes import wintypes
from pathlib import Path

from bluff.evdev import BTN_LEFT, EV_KEY, EV_REL, REL_HWHEEL, REL_WHEEL, REL_X, REL_Y, DeviceInfo, Kind
from bluff.windows.scancodes import keycode, wheel_ticks

logger = logging.getLogger(__name__)

WH_KEYBOARD_LL, WH_MOUSE_LL = 13, 14
WM_KEYDOWN, WM_SYSKEYDOWN = 0x100, 0x104
WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_MOUSEHWHEEL = 0x200, 0x20A, 0x20E
WM_XBUTTONDOWN, WM_XBUTTONUP = 0x20B, 0x20C
LLKHF_INJECTED, LLMHF_INJECTED = 0x10, 0x01

BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA = 0x111, 0x112, 0x113, 0x114
_BUTTONS = {
    0x201: (BTN_LEFT, 1), 0x202: (BTN_LEFT, 0),
    0x204: (BTN_RIGHT, 1), 0x205: (BTN_RIGHT, 0),
    0x207: (BTN_MIDDLE, 1), 0x208: (BTN_MIDDLE, 0),
}  # fmt: skip
_XBUTTONS = {1: BTN_SIDE, 2: BTN_EXTRA}


class KBDLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [
        ("vkCode", wintypes.DWORD),
        ("scanCode", wintypes.DWORD),
        ("flags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


class MSLLHOOKSTRUCT(ctypes.Structure):
    _fields_ = [
        ("pt", wintypes.POINT),
        ("mouseData", wintypes.DWORD),
        ("flags", wintypes.DWORD),
        ("time", wintypes.DWORD),
        ("dwExtraInfo", ctypes.c_size_t),
    ]


LRESULT = ctypes.c_ssize_t
HOOKPROC = ctypes.WINFUNCTYPE(LRESULT, ctypes.c_int, wintypes.WPARAM, wintypes.LPARAM)
_user32 = ctypes.WinDLL("user32", use_last_error=True)
_user32.SetWindowsHookExW.argtypes = [ctypes.c_int, HOOKPROC, wintypes.HINSTANCE, wintypes.DWORD]
_user32.SetWindowsHookExW.restype = wintypes.HHOOK
_user32.CallNextHookEx.argtypes = [wintypes.HHOOK, ctypes.c_int, wintypes.WPARAM, wintypes.LPARAM]
_user32.CallNextHookEx.restype = LRESULT
_user32.GetMessageW.argtypes = [ctypes.POINTER(wintypes.MSG), wintypes.HWND, wintypes.UINT, wintypes.UINT]
_user32.GetCursorPos.argtypes = [ctypes.POINTER(wintypes.POINT)]

type Handler = Callable[[HookDevice, int, int, int], None]


class HookDevice:
    """One side of `Hooks` as the bridge sees it; grabbing either side grabs both."""

    def __init__(self, info: DeviceInfo, hooks: Hooks) -> None:
        self.info = info
        self._hooks = hooks

    def grab(self, on: bool) -> None:
        self._hooks.grabbed = on


class Hooks:
    """Keyboard and mouse hooks on their own thread, feeding Linux events to `handler` on the event loop.

    Keys always reach the handler so the hotkey works; the mouse only while
    grabbed. While grabbed, Windows sees no input at all except injected input
    and Ctrl+Alt+Del. Low-level hooks never see input meant for an elevated
    window unless bluff runs elevated too.
    """

    def __init__(self, handler: Handler) -> None:
        self.keyboard = HookDevice(DeviceInfo("keyboard hook", Path("WH_KEYBOARD_LL"), Kind.KEYBOARD), self)
        self.mouse = HookDevice(DeviceInfo("mouse hook", Path("WH_MOUSE_LL"), Kind.MOUSE), self)
        self.grabbed = False
        self._handler = handler
        self._loop = asyncio.get_running_loop()
        self._down: set[int] = set()
        self._wheel = [0, 0]
        # ctypes must keep the callbacks alive for as long as the hooks are installed.
        self._procs = (HOOKPROC(self._on_key), HOOKPROC(self._on_mouse))
        threading.Thread(target=self._run, name="input hooks", daemon=True).start()

    def _run(self) -> None:
        for kind, proc in zip((WH_KEYBOARD_LL, WH_MOUSE_LL), self._procs, strict=True):
            if not _user32.SetWindowsHookExW(kind, proc, None, 0):
                logger.error("could not install input hook %d: error %d", kind, ctypes.get_last_error())
        # Windows calls low-level hooks from this thread's message loop.
        message = wintypes.MSG()
        while _user32.GetMessageW(ctypes.byref(message), None, 0, 0) > 0:
            pass

    def _emit(self, device: HookDevice, type_: int, code: int, value: int) -> None:
        self._loop.call_soon_threadsafe(self._handler, device, type_, code, value)

    def _on_key(self, n_code: int, w_param: int, l_param: int) -> int:
        event = KBDLLHOOKSTRUCT.from_address(l_param)
        if n_code < 0 or event.flags & LLKHF_INJECTED:
            return _user32.CallNextHookEx(None, n_code, w_param, l_param)
        code = keycode(event.scanCode, event.flags, event.vkCode)
        if code is not None:
            if w_param in (WM_KEYDOWN, WM_SYSKEYDOWN):
                value = 2 if code in self._down else 1
                self._down.add(code)
            else:
                value = 0
                self._down.discard(code)
            self._emit(self.keyboard, EV_KEY, code, value)
        if self.grabbed:
            return 1
        return _user32.CallNextHookEx(None, n_code, w_param, l_param)

    def _on_mouse(self, n_code: int, w_param: int, l_param: int) -> int:
        event = MSLLHOOKSTRUCT.from_address(l_param)
        if n_code < 0 or not self.grabbed or event.flags & LLMHF_INJECTED:
            return _user32.CallNextHookEx(None, n_code, w_param, l_param)
        if w_param == WM_MOUSEMOVE:
            # The cursor stays put while grabbed, so the distance to it is the motion, ballistics included.
            cursor = wintypes.POINT()
            _user32.GetCursorPos(ctypes.byref(cursor))
            for axis, delta in ((REL_X, event.pt.x - cursor.x), (REL_Y, event.pt.y - cursor.y)):
                if delta:
                    self._emit(self.mouse, EV_REL, axis, delta)
        elif w_param in _BUTTONS:
            self._emit(self.mouse, EV_KEY, *_BUTTONS[w_param])
        elif w_param in (WM_XBUTTONDOWN, WM_XBUTTONUP) and (button := _XBUTTONS.get(event.mouseData >> 16)):
            self._emit(self.mouse, EV_KEY, button, int(w_param == WM_XBUTTONDOWN))
        elif w_param in (WM_MOUSEWHEEL, WM_MOUSEHWHEEL):
            # Precision touchpads send fractions of a notch; keep the rest for the next event.
            axis = int(w_param == WM_MOUSEHWHEEL)
            delta = ctypes.c_short(event.mouseData >> 16).value
            ticks, self._wheel[axis] = wheel_ticks(self._wheel[axis] + delta)
            if ticks:
                self._emit(self.mouse, EV_REL, (REL_WHEEL, REL_HWHEEL)[axis], ticks)
        return 1
