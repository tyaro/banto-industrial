"""Pure stdlib regression: no User32/GDI/SCM loading or visible windows.

From repository root:
  python -B -m unittest discover -s apps/chronogazer/installer/tests -p test_windows_gui_capability.py

The actual API.windows callback runs against fake User32, including its child-text
branch. Restoring the former `result = C.c_size_t()` / `byref(result)` pair in that
branch makes test_actual_parent_and_child_callbacks fail. Do not run the native
probe locally; the manual hosted workflow is the only native execution path.
"""

import struct
import unittest
import zlib

import windows_gui_capability as probe


class FakeUser32:
    def __init__(self):
        self.text_reads = []
        self.reply_calls = 0

    def GetWindowThreadProcessId(self, handle, pid):
        pid._obj.value = {101: 77, 102: 88, 201: 77, 202: 88}[handle]
        return 1

    def GetWindowTextW(self, handle, text, count):
        self.text_reads.append(handle)
        text.value = {101: "owned parent", 201: "owned child"}[handle]
        return len(text.value)

    def GetClassNameW(self, handle, text, count):
        text.value = "STATIC" if handle == 201 else "Fixture"
        return len(text.value)

    def IsWindowVisible(self, handle):
        return True

    def GetWindowRect(self, handle, rect):
        rect._obj.left, rect._obj.top = 10, 20
        rect._obj.right, rect._obj.bottom = 610, 320
        return True

    def SendMessageTimeoutW(self, handle, message, count, address, flags, timeout, reply_length):
        self.reply_calls += 1
        reply_length._obj.value = 11
        return 1

    def EnumWindows(self, callback, unused):
        callback(101, 0)
        callback(102, 0)
        return True

    def EnumChildWindows(self, parent, callback, unused):
        if parent != 101:
            raise AssertionError("unexpected parent HWND")
        callback(201, 0)
        callback(202, 0)
        return True


class CapabilityTests(unittest.TestCase):
    def test_actual_parent_and_child_callbacks(self):
        api = probe.API.__new__(probe.API)  # Deliberately bypass WinDLL initialization.
        api.u = FakeUser32()
        api.callback = lambda function: function
        parents = api.windows(77)
        children = api.windows(77, 101)
        self.assertEqual([(item["hwnd"], item["title"]) for item in parents], [(101, "owned parent")])
        self.assertEqual([(item["hwnd"], item["title"]) for item in children], [(201, "owned child")])
        self.assertEqual(api.u.text_reads, [101, 201])  # No other PID's text is queried.
        self.assertEqual(api.u.reply_calls, 1)

    def test_callback_exception_is_reported_instead_of_becoming_no_windows(self):
        class BrokenUser32(FakeUser32):
            def GetWindowTextW(self, handle, text, count):
                raise ValueError("fake text retrieval failure")
        api = probe.API.__new__(probe.API)
        api.u = BrokenUser32()
        api.callback = lambda function: function
        for parent in (None, 101):
            with self.subTest(parent=parent), self.assertRaisesRegex(RuntimeError, "window_callback_failed: ValueError: fake text retrieval failure"):
                api.windows(77, parent)

    def test_readiness_waits_for_child_label_after_parent_appears(self):
        window = {"pid": 77, "hwnd": 101, "title": "parent", "visible": True, "rect": [1, 2, 601, 302]}

        class ScriptedAPI:
            elapsed = 0

            def windows(self, pid, parent=None):
                if parent is None:
                    return [window]
                return [{"title": "label"}] if self.elapsed >= .1 else []

            def pause(self, seconds):
                self.elapsed += seconds
        api, observations = ScriptedAPI(), []
        actual, children = probe.wait_for_ready_window(api, 77, "parent", "label", observations,
                                                       lambda: True, clock=lambda: api.elapsed, pause=api.pause)
        self.assertEqual(actual, window)
        self.assertEqual(children, [{"title": "label"}])
        self.assertEqual(len(observations), 2)
        self.assertEqual(observations[0]["child_windows"], [])
        self.assertEqual(observations[1]["child_windows"], children)

    def test_readiness_deadline_preserves_partial_parent_observations(self):
        class ParentOnly:
            elapsed = 0

            def windows(self, pid, parent=None):
                return [] if parent else [{"pid": 77, "hwnd": 101, "title": "parent", "visible": True, "rect": [1, 2, 601, 302]}]

            def pause(self, seconds):
                self.elapsed += seconds
        api, observations = ParentOnly(), []
        with self.assertRaisesRegex(RuntimeError, "owned_window_and_label_not_ready_before_deadline"):
            probe.wait_for_ready_window(api, 77, "parent", "label", observations, lambda: True,
                                        timeout=.2, clock=lambda: api.elapsed, pause=api.pause)
        self.assertEqual(len(observations), 2)
        self.assertTrue(all(item["windows"] and not item["child_windows"] for item in observations))

    def test_environment_requires_hosted_explicit_opt_in(self):
        values = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted",
                  "RUNNER_OS": "Windows", "BANTO_973_GUI_TEST": probe.OPT_IN}
        probe.validate_environment(values)
        for key in values:
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                probe.validate_environment(dict(values, **{key: "disallowed"}))

    def test_session0_and_noninteractive_desktop_are_unsupported(self):
        environment = {"session": 1, "WSF_VISIBLE": True, "window_station": "WinSta0",
                       "virtual_screen_xywh": [0, 0, 1024, 768]}
        self.assertEqual(probe.capability_reasons(environment), [])
        for change in ({"session": 0}, {"WSF_VISIBLE": False}, {"window_station": "Service-0"},
                       {"virtual_screen_xywh": [0, 0, 0, 0]}):
            with self.subTest(change=change):
                self.assertTrue(probe.capability_reasons(dict(environment, **change)))

    def test_window_selection_refuses_ambiguity_and_foreign_pid(self):
        window = {"pid": 77, "title": "owned", "visible": True, "rect": [1, 2, 601, 302]}
        self.assertEqual(probe.select_window([window], 77, "owned"), window)
        self.assertIsNone(probe.select_window([dict(window, pid=88)], 77, "owned"))
        self.assertIsNone(probe.select_window([dict(window, visible=False)], 77, "owned"))
        with self.assertRaises(RuntimeError):
            probe.select_window([window, window], 77, "owned")

    def test_black_or_single_color_is_not_rendering_proof(self):
        self.assertFalse(probe.calibration_rendered(probe.pixel_counts(bytes(6))))
        self.assertFalse(probe.calibration_rendered(probe.pixel_counts(bytes(probe.RED) * 5000)))
        self.assertTrue(probe.calibration_rendered(probe.pixel_counts(bytes(probe.RED + probe.GREEN) * 5000)))

    def test_png_preserves_synthetic_pixels(self):
        rgb = bytes(probe.RED + probe.GREEN)
        png = probe.png_bytes(2, 1, rgb)
        self.assertEqual(png[:8], b'\x89PNG\r\n\x1a\n')
        offset, decoded = 8, None
        while offset < len(png):
            length = struct.unpack('>I', png[offset:offset + 4])[0]
            kind, data = png[offset + 4:offset + 8], png[offset + 8:offset + 8 + length]
            if kind == b'IDAT':
                decoded = zlib.decompress(data)
            offset += length + 12
        self.assertEqual(decoded, b'\0' + rgb)


if __name__ == "__main__":
    unittest.main()
