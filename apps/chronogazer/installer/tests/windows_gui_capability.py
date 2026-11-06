"""Disposable GitHub-hosted Windows GUI capability gate, NOT a ChronoGazer UI test.

Do not execute this on a developer PC. Only the temporary workflow supplies the
opt-in and fresh RUNNER_TEMP marker. Import/pure helpers do not open Win32 APIs.
The owned child paints a two-color calibration window. Only its desktop crop is
saved, and pixels must prove actual rendering. No service/SCM/registry/NSIS/UAC.
"""

import argparse
import binascii
import ctypes as C
from ctypes import wintypes as W
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import time
import zlib

OPT_IN = "github-hosted-gui-capability-only-v1"
RED = (220, 40, 60)
GREEN = (20, 190, 90)


class UnsupportedGUI(RuntimeError):
    pass


def require(condition, reason):
    if not condition:
        raise RuntimeError(reason)


def validate_environment(values):
    for key, value in (("GITHUB_ACTIONS", "true"), ("RUNNER_ENVIRONMENT", "github-hosted"),
                       ("RUNNER_OS", "Windows"), ("BANTO_973_GUI_TEST", OPT_IN)):
        require(values.get(key) == value, "refused_environment_" + key)


def select_window(windows, pid, title):
    matches = [window for window in windows if window["pid"] == pid and window["title"] == title
               and window["visible"] and window["rect"][2] > window["rect"][0]
               and window["rect"][3] > window["rect"][1]]
    require(len(matches) <= 1, "ambiguous_owned_window")
    return matches[0] if matches else None


def capability_reasons(environment):
    reasons = []
    if environment["session"] == 0:
        reasons.append("Session0")
    if not environment["WSF_VISIBLE"] or environment["window_station"].casefold() != "winsta0":
        reasons.append("noninteractive_window_station")
    if environment["virtual_screen_xywh"][2] < 640 or environment["virtual_screen_xywh"][3] < 480:
        reasons.append("screen_bounds_unavailable")
    return reasons


def pixel_counts(rgb):
    counts = {"red": 0, "green": 0, "non_black": 0}
    for index in range(0, len(rgb), 3):
        pixel = rgb[index:index + 3]
        if max(pixel) > 12:
            counts["non_black"] += 1
        for name, expected in (("red", RED), ("green", GREEN)):
            if all(abs(value - wanted) <= 5 for value, wanted in zip(pixel, expected)):
                counts[name] += 1
    return counts


def guard():
    validate_environment(os.environ)
    require(os.name == "nt", "Windows_required")
    shell = C.WinDLL("shell32", use_last_error=True)
    shell.IsUserAnAdmin.argtypes, shell.IsUserAnAdmin.restype = [], W.BOOL
    require(bool(shell.IsUserAnAdmin()), "administrator_token_required_no_UAC")
    temp = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    raw = Path(os.environ["BANTO_973_GUI_ROOT"])
    root = raw.resolve(strict=True)
    require(root != temp and root.is_relative_to(temp) and root.name.startswith("Banto973Gui-"), "invalid_fresh_root")
    for ancestor in (raw, *raw.parents):
        require(not ancestor.is_symlink() and not ancestor.is_junction(), "reparse_root")
        if ancestor.resolve() == temp:
            break
    marker = os.environ["GITHUB_RUN_ID"] + '\n' + os.environ["BANTO_973_EXPECTED_SHA"] + '\n'
    require((root / ".banto-973-gui").read_text(encoding="utf-8") == marker, "marker_mismatch")
    return root


def bind(dll, name, args, result):
    function = getattr(dll, name)
    function.argtypes, function.restype = args, result
    return function


class API:
    def __init__(self):
        self.u = C.WinDLL("user32", use_last_error=True)
        self.k = C.WinDLL("kernel32", use_last_error=True)
        self.g = C.WinDLL("gdi32", use_last_error=True)
        self.callback = C.WINFUNCTYPE(W.BOOL, W.HWND, C.c_ssize_t)
        for name, args, result in (
            ("GetProcessWindowStation", [], W.HANDLE), ("GetThreadDesktop", [W.DWORD], W.HANDLE),
            ("GetUserObjectInformationW", [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.POINTER(W.DWORD)], W.BOOL),
            ("GetSystemMetrics", [C.c_int], C.c_int), ("GetWindowThreadProcessId", [W.HWND, C.POINTER(W.DWORD)], W.DWORD),
            ("GetWindowTextW", [W.HWND, W.LPWSTR, C.c_int], C.c_int),
            ("GetClassNameW", [W.HWND, W.LPWSTR, C.c_int], C.c_int),
            ("IsWindowVisible", [W.HWND], W.BOOL), ("GetWindowRect", [W.HWND, C.POINTER(W.RECT)], W.BOOL),
            ("EnumWindows", [self.callback, C.c_ssize_t], W.BOOL),
            ("EnumChildWindows", [W.HWND, self.callback, C.c_ssize_t], W.BOOL),
            ("PostMessageW", [W.HWND, W.UINT, C.c_size_t, C.c_ssize_t], W.BOOL),
            ("SendMessageTimeoutW", [W.HWND, W.UINT, C.c_size_t, C.c_ssize_t, W.UINT, W.UINT, C.POINTER(C.c_size_t)], C.c_ssize_t),
            ("GetDC", [W.HWND], W.HDC), ("ReleaseDC", [W.HWND, W.HDC], C.c_int),
        ):
            bind(self.u, name, args, result)
        bind(self.k, "GetCurrentThreadId", [], W.DWORD)
        bind(self.k, "ProcessIdToSessionId", [W.DWORD, C.POINTER(W.DWORD)], W.BOOL)
        bind(self.k, "WTSGetActiveConsoleSessionId", [], W.DWORD)
        bind(self.u, "SetProcessDpiAwarenessContext", [W.HANDLE], W.BOOL)
        self.dpi_set = bool(self.u.SetProcessDpiAwarenessContext(W.HANDLE(-4)))

    def object_name(self, handle):
        buffer = C.create_unicode_buffer(512)
        needed = W.DWORD()
        require(bool(self.u.GetUserObjectInformationW(handle, 2, buffer, C.sizeof(buffer), C.byref(needed))), "user_object_name_failed")
        return buffer.value

    def environment(self):
        class Flags(C.Structure):
            _fields_ = [("inherit", W.BOOL), ("reserved", W.BOOL), ("flags", W.DWORD)]
        session = W.DWORD()
        require(bool(self.k.ProcessIdToSessionId(os.getpid(), C.byref(session))), "session_query_failed")
        station = self.u.GetProcessWindowStation()
        desktop = self.u.GetThreadDesktop(self.k.GetCurrentThreadId())
        require(station and desktop, "station_or_desktop_missing")
        flags, needed = Flags(), W.DWORD()
        require(bool(self.u.GetUserObjectInformationW(station, 1, C.byref(flags), C.sizeof(flags), C.byref(needed))), "station_flags_failed")
        bounds = [self.u.GetSystemMetrics(index) for index in (76, 77, 78, 79)]
        return {"pid": os.getpid(), "session": session.value,
                "active_console_session": self.k.WTSGetActiveConsoleSessionId(),
                "window_station": self.object_name(station), "station_flags": flags.flags,
                "WSF_VISIBLE": bool(flags.flags & 1), "desktop": self.object_name(desktop),
                "virtual_screen_xywh": bounds, "dpi_awareness_set": self.dpi_set}

    def windows(self, pid, parent=None):
        result = []

        @self.callback
        def visit(handle, unused):
            actual = W.DWORD()
            self.u.GetWindowThreadProcessId(handle, C.byref(actual))
            if actual.value == pid:  # Never read another process's window text.
                text, class_name, rect = C.create_unicode_buffer(2048), C.create_unicode_buffer(256), W.RECT()
                self.u.GetWindowTextW(handle, text, len(text))
                if parent is not None:
                    reply_length = C.c_size_t()
                    self.u.SendMessageTimeoutW(handle, 0x000D, len(text), C.addressof(text), 3, 1000, C.byref(reply_length))
                self.u.GetClassNameW(handle, class_name, len(class_name))
                if self.u.GetWindowRect(handle, C.byref(rect)):
                    result.append({"hwnd": int(handle), "pid": actual.value, "title": text.value,
                                   "class": class_name.value, "visible": bool(self.u.IsWindowVisible(handle)),
                                   "rect": [rect.left, rect.top, rect.right, rect.bottom]})
            return True
        if parent is None:
            require(bool(self.u.EnumWindows(visit, 0)), "EnumWindows_failed")
        else:
            self.u.EnumChildWindows(parent, visit, 0)
        return result

    def capture(self, rect):
        left, top, right, bottom = rect
        width, height = right - left, bottom - top
        require(0 < width <= 2048 and 0 < height <= 2048, "invalid_crop_size")
        g = self.g
        for name, args, result in (
            ("CreateCompatibleDC", [W.HDC], W.HDC),
            ("CreateCompatibleBitmap", [W.HDC, C.c_int, C.c_int], W.HANDLE),
            ("SelectObject", [W.HDC, W.HANDLE], W.HANDLE),
            ("BitBlt", [W.HDC, C.c_int, C.c_int, C.c_int, C.c_int, W.HDC, C.c_int, C.c_int, W.DWORD], W.BOOL),
            ("GetDIBits", [W.HDC, W.HANDLE, W.UINT, W.UINT, C.c_void_p, C.c_void_p, W.UINT], C.c_int),
            ("DeleteObject", [W.HANDLE], W.BOOL), ("DeleteDC", [W.HDC], W.BOOL),
        ):
            bind(g, name, args, result)

        class Header(C.Structure):
            _fields_ = [("size", W.DWORD), ("width", W.LONG), ("height", W.LONG),
                        ("planes", W.WORD), ("bits", W.WORD), ("compression", W.DWORD),
                        ("image_size", W.DWORD), ("xppm", W.LONG), ("yppm", W.LONG),
                        ("colors", W.DWORD), ("important", W.DWORD)]
        screen = self.u.GetDC(None)
        require(bool(screen), "screen_DC_failed")
        memory = bitmap = old = None
        try:
            memory = g.CreateCompatibleDC(screen)
            bitmap = g.CreateCompatibleBitmap(screen, width, height)
            require(memory and bitmap, "capture_allocation_failed")
            old = g.SelectObject(memory, bitmap)
            require(bool(old), "bitmap_selection_failed")
            require(bool(g.BitBlt(memory, 0, 0, width, height, screen, left, top, 0x40CC0020)), "desktop_BitBlt_failed")
            g.SelectObject(memory, old)
            old = None  # GetDIBits requires an unselected bitmap.
            header = Header(C.sizeof(Header), width, -height, 1, 32, 0, width * height * 4, 0, 0, 0, 0)
            pixels = C.create_string_buffer(width * height * 4)
            require(g.GetDIBits(memory, bitmap, 0, height, pixels, C.byref(header), 0) == height, "GetDIBits_failed")
            bgra = pixels.raw
            rgb = bytearray(width * height * 3)
            rgb[0::3], rgb[1::3], rgb[2::3] = bgra[2::4], bgra[1::4], bgra[0::4]
            return width, height, bytes(rgb)
        finally:
            if old:
                g.SelectObject(memory, old)
            if bitmap:
                g.DeleteObject(bitmap)
            if memory:
                g.DeleteDC(memory)
            self.u.ReleaseDC(None, screen)


def png_bytes(width, height, rgb):
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', binascii.crc32(kind + data) & 0xffffffff)
    rows = b''.join(b'\0' + rgb[start:start + width * 3] for start in range(0, len(rgb), width * 3))
    return (b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', width, height, 8, 2, 0, 0, 0))
            + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b''))


def fixture(api, root):
    u, g, k = api.u, api.g, api.k
    wndproc_type = C.WINFUNCTYPE(C.c_ssize_t, W.HWND, W.UINT, C.c_size_t, C.c_ssize_t)

    class Paint(C.Structure):
        _fields_ = [("dc", W.HDC), ("erase", W.BOOL), ("rect", W.RECT),
                    ("restore", W.BOOL), ("update", W.BOOL), ("reserved", W.BYTE * 32)]

    class WindowClass(C.Structure):
        _fields_ = [("style", W.UINT), ("proc", wndproc_type), ("class_extra", C.c_int),
                    ("window_extra", C.c_int), ("instance", W.HINSTANCE), ("icon", W.HANDLE),
                    ("cursor", W.HANDLE), ("background", W.HANDLE), ("menu", W.LPCWSTR), ("name", W.LPCWSTR)]
    bind(k, "GetModuleHandleW", [W.LPCWSTR], W.HMODULE)
    for name, args, result in (
        ("DefWindowProcW", [W.HWND, W.UINT, C.c_size_t, C.c_ssize_t], C.c_ssize_t),
        ("RegisterClassW", [C.POINTER(WindowClass)], W.WORD),
        ("CreateWindowExW", [W.DWORD, W.LPCWSTR, W.LPCWSTR, W.DWORD, C.c_int, C.c_int, C.c_int, C.c_int, W.HWND, W.HMENU, W.HINSTANCE, C.c_void_p], W.HWND),
        ("BeginPaint", [W.HWND, C.POINTER(Paint)], W.HDC), ("EndPaint", [W.HWND, C.POINTER(Paint)], W.BOOL),
        ("FillRect", [W.HDC, C.POINTER(W.RECT), W.HANDLE], C.c_int),
        ("DestroyWindow", [W.HWND], W.BOOL), ("PostQuitMessage", [C.c_int], None),
        ("ShowWindow", [W.HWND, C.c_int], W.BOOL), ("UpdateWindow", [W.HWND], W.BOOL),
        ("SetWindowPos", [W.HWND, W.HWND, C.c_int, C.c_int, C.c_int, C.c_int, W.UINT], W.BOOL),
        ("SetTimer", [W.HWND, C.c_size_t, W.UINT, C.c_void_p], C.c_size_t),
        ("GetMessageW", [C.POINTER(W.MSG), W.HWND, W.UINT, W.UINT], C.c_int),
        ("TranslateMessage", [C.POINTER(W.MSG)], W.BOOL), ("DispatchMessageW", [C.POINTER(W.MSG)], C.c_ssize_t),
    ):
        bind(u, name, args, result)
    bind(g, "CreateSolidBrush", [W.DWORD], W.HANDLE)
    bind(g, "DeleteObject", [W.HANDLE], W.BOOL)
    bind(g, "GetStockObject", [C.c_int], W.HANDLE)

    @wndproc_type
    def callback(handle, message, wparam, lparam):
        if message == 0x000F:  # WM_PAINT: known colors in the actual desktop image.
            paint = Paint()
            dc = u.BeginPaint(handle, C.byref(paint))
            if dc:
                for rect, color in ((W.RECT(30, 70, 250, 180), RED), (W.RECT(300, 70, 520, 180), GREEN)):
                    brush = g.CreateSolidBrush(color[0] | color[1] << 8 | color[2] << 16)
                    if brush:
                        u.FillRect(dc, C.byref(rect), brush)
                        g.DeleteObject(brush)
                u.EndPaint(handle, C.byref(paint))
            return 0
        if message in (0x0010, 0x0113):  # WM_CLOSE / watchdog WM_TIMER.
            u.DestroyWindow(handle)
            return 0
        if message == 0x0002:
            u.PostQuitMessage(0)
            return 0
        return u.DefWindowProcW(handle, message, wparam, lparam)

    title = "Banto973 GUI capability " + root.name
    instance = k.GetModuleHandleW(None)
    window_class = WindowClass(0, callback, 0, 0, instance, None, None, g.GetStockObject(0), None, "Banto973Calibration")
    require(bool(u.RegisterClassW(C.byref(window_class))), "RegisterClass_failed")
    bounds = api.environment()["virtual_screen_xywh"]
    window = u.CreateWindowExW(0, window_class.name, title, 0x10CF0000, bounds[0] + 40, bounds[1] + 40, 600, 300, None, None, instance, None)
    require(bool(window), "CreateWindow_failed")
    label = "Banto973 visible calibration " + root.name[-8:]
    require(bool(u.CreateWindowExW(0, "STATIC", label, 0x50000000, 20, 20, 560, 30, window, None, instance, None)), "CreateLabel_failed")
    u.SetWindowPos(window, W.HWND(-1), 0, 0, 0, 0, 0x0043)
    u.ShowWindow(window, 1)
    u.UpdateWindow(window)
    require(bool(u.SetTimer(window, 1, 20000, None)), "watchdog_timer_failed")
    message = W.MSG()
    while True:
        status = u.GetMessageW(C.byref(message), None, 0, 0)
        require(status != -1, "GetMessage_failed")
        if status == 0:
            break
        u.TranslateMessage(C.byref(message))
        u.DispatchMessageW(C.byref(message))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", action="store_true")
    args = parser.parse_args()
    root = guard()
    if args.fixture:
        fixture(API(), root)
        return
    proof = root / "proof"
    report = {"status": "error", "capability_supported": False,
              "ChronoGazer_UI_test": "not_executed", "service_operations": 0,
              "head_sha": os.environ["BANTO_973_EXPECTED_SHA"], "event_sha": os.environ["GITHUB_SHA"],
              "forced_termination": False}
    child = None
    try:
        api = API()
        environment = api.environment()
        report["environment"] = environment
        x, y, width, height = environment["virtual_screen_xywh"]
        reasons = capability_reasons(environment)
        if reasons:
            report.update(status="unsupported", reasons=reasons)
            raise UnsupportedGUI()
        with (proof / "child.stdout.log").open('w') as stdout, (proof / "child.stderr.log").open('w') as stderr:
            child = subprocess.Popen([sys.executable, "-B", str(Path(__file__).resolve()), "--fixture"],
                                     stdout=stdout, stderr=stderr, creationflags=subprocess.CREATE_NO_WINDOW)
        report["fixture_pid"] = child.pid
        title = "Banto973 GUI capability " + root.name
        deadline = time.monotonic() + 10
        window = None
        while time.monotonic() < deadline and child.poll() is None:
            windows = api.windows(child.pid)
            window = select_window(windows, child.pid, title)
            if window:
                break
            time.sleep(.1)
        require(window is not None, "owned_visible_window_not_found")
        report["window"] = window
        report["child_windows"] = api.windows(child.pid, window["hwnd"])
        expected_label = "Banto973 visible calibration " + root.name[-8:]
        require(any(item["title"] == expected_label for item in report["child_windows"]), "owned_label_text_missing")
        left, top, right, bottom = window["rect"]
        require(x <= left < right <= x + width and y <= top < bottom <= y + height, "window_not_fully_onscreen")
        time.sleep(.5)
        crop_width, crop_height, rgb = api.capture(window["rect"])
        png = png_bytes(crop_width, crop_height, rgb)
        (proof / "calibration-window.png").write_bytes(png)
        counts = pixel_counts(rgb)
        report["pixels"] = counts
        report["png_sha256"] = hashlib.sha256(png).hexdigest()
        if counts["red"] < 5000 or counts["green"] < 5000:
            report.update(status="unsupported", reasons=["calibration_not_rendered_or_black_capture"])
        else:
            report.update(status="supported", capability_supported=True)
    except UnsupportedGUI:
        pass
    except Exception as error:
        report.update(status="error", failure=str(error), capability_supported=False)
    finally:
        if child:
            try:
                candidates = api.windows(child.pid)
                owned = select_window(candidates, child.pid, "Banto973 GUI capability " + root.name)
                if owned:
                    api.u.PostMessageW(owned["hwnd"], 0x0010, 0, 0)
                report["child_exit"] = child.wait(timeout=5)
                require(report["child_exit"] == 0, "fixture_exit_nonzero")
            except Exception as error:
                if child.poll() is None:
                    report["forced_termination"] = True
                    child.terminate()  # Only the process created by this probe.
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        report["child_still_running"] = True
                report.update(status="error", cleanup_failure=str(error), capability_supported=False)
        report["source_sha256"] = hashlib.sha256(Path(__file__).read_bytes()).hexdigest()
        (proof / "gui-capability.json").write_text(json.dumps(report, indent=2) + '\n', encoding="utf-8")
    if not report["capability_supported"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
