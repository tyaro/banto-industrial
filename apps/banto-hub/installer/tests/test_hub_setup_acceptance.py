"""hub_setup_acceptance.py の純粋部分のテスト（SCM・実 setup は一切触らない）。

実行: python -B -m unittest discover -s apps/banto-hub/installer/tests -p test_hub_setup_acceptance.py
"""

import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import hub_setup_acceptance as h  # noqa: E402

OK_ENV = {
    "GITHUB_ACTIONS": "true",
    "RUNNER_ENVIRONMENT": "github-hosted",
    h.GUARD_ENV: h.GUARD_VALUE,
}


class GuardTests(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(h.guard_problems(OK_ENV, "win32", True), [])

    def test_each_missing_condition_refuses(self):
        for key in OK_ENV:
            env = {k: v for k, v in OK_ENV.items() if k != key}
            self.assertTrue(h.guard_problems(env, "win32", True), key)
        self.assertTrue(h.guard_problems(OK_ENV, "linux", True))
        self.assertTrue(h.guard_problems(OK_ENV, "win32", False))
        self.assertTrue(h.guard_problems({**OK_ENV, "RUNNER_ENVIRONMENT": "self-hosted"}, "win32", True))
        self.assertTrue(h.guard_problems({**OK_ENV, h.GUARD_ENV: "1"}, "win32", True))


class ParseTests(unittest.TestCase):
    def test_sc_query(self):
        t = (
            "SERVICE_NAME: BantoHub\n"
            "        TYPE               : 10  WIN32_OWN_PROCESS\n"
            "        STATE              : 4  RUNNING\n"
            "                                (STOPPABLE, NOT_PAUSABLE, ACCEPTS_SHUTDOWN)\n"
            "        WIN32_EXIT_CODE    : 0  (0x0)\n"
            "        SERVICE_EXIT_CODE  : 0  (0x0)\n"
            "        CHECKPOINT         : 0x0\n"
            "        WAIT_HINT          : 0x0\n"
            "        PID                : 1234\n"
            "        FLAGS              :\n"
        )
        self.assertEqual(h.parse_sc_query(t), {"state": "RUNNING", "pid": 1234})
        self.assertEqual(
            h.parse_sc_query("[SC] EnumQueryServicesStatus:OpenService FAILED 1060"),
            {"state": None, "pid": None},
        )

    def test_sc_query_stopped_has_pid_zero(self):
        t = "SERVICE_NAME: X\n        STATE              : 1  STOPPED\n        PID                : 0\n        FLAGS              :\n"
        self.assertEqual(h.parse_sc_query(t), {"state": "STOPPED", "pid": 0})

    def test_root_marker_problem(self):
        self.assertIn("not set", h.root_marker_problem({}))
        with tempfile.TemporaryDirectory() as d:
            env = {h.ROOT_ENV: d}
            self.assertIn("marker", h.root_marker_problem(env))
            (Path(d) / ".banto-973-ci").write_text("")
            self.assertIsNone(h.root_marker_problem(env))

    def test_sc_qc(self):
        t = (
            "        START_TYPE         : 3   DEMAND_START\n"
            "        BINARY_PATH_NAME   : C:\\Program Files\\BantoHub\\banto-hub.exe service-run\n"
        )
        d = h.parse_sc_qc(t)
        self.assertEqual(d["start_type"], "DEMAND_START")
        self.assertTrue(h.image_path_inside(d["image_path"], h.INSTDIR))

    def test_image_path_forms(self):
        self.assertEqual(h.image_path_exe('"C:\\A B\\x.exe" arg'), "C:\\A B\\x.exe")
        self.assertEqual(h.image_path_exe("C:\\A B\\x.exe arg"), "C:\\A B\\x.exe")
        self.assertTrue(h.image_path_inside('"C:\\Program Files\\BantoHub\\a.exe"', h.INSTDIR))
        self.assertFalse(h.image_path_inside("C:\\Program Files\\BantoHubX\\a.exe", h.INSTDIR))
        self.assertFalse(h.image_path_inside("D:\\elsewhere\\a.exe", h.INSTDIR))

    def test_icacls_explicit_users(self):
        t = (
            "C:\\x BUILTIN\\Users:(M)\n  NT AUTHORITY\\SYSTEM:(F)\n"
            "  BUILTIN\\Users:(I)(RX)\n  BUILTIN\\Administrators:(F)\n"
        )
        self.assertEqual(len(h.icacls_explicit_users_aces(t)), 1)
        self.assertEqual(h.icacls_explicit_users_aces("  BUILTIN\\Users:(I)(RX)\n"), [])

    def test_status(self):
        self.assertTrue(h.status_initialized({"initialized": True, "grants": {}}))
        self.assertFalse(h.status_initialized({"initialized": False}))
        self.assertFalse(h.status_initialized({}))


class FileTests(unittest.TestCase):
    def test_find_setup(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "BantoHub_0.2.0-alpha.9_x64-setup.exe").write_bytes(b"x")
            _, v = h.find_setup(Path(d))
            self.assertEqual(v, "0.2.0-alpha.9")
            (Path(d) / "BantoHub_0.3.0_x64-setup.exe").write_bytes(b"y")
            with self.assertRaises(h.PhaseFailure):
                h.find_setup(Path(d))

    def test_tree_hashes_detects_change(self):
        with tempfile.TemporaryDirectory() as d:
            f = Path(d) / "a.txt"
            f.write_text("1")
            before = h.tree_hashes(Path(d))
            f.write_text("2")
            self.assertNotEqual(before, h.tree_hashes(Path(d)))


class RequestHeaderTests(unittest.TestCase):
    def test_get_and_post_carry_client_header_and_only_post_has_content_type(self):
        get = h.request_headers(False)
        post = h.request_headers(True)
        self.assertEqual(get, {"X-Banto-Client": "banto"})
        self.assertEqual(post, {"X-Banto-Client": "banto", "Content-Type": "application/json"})


class SecretTests(unittest.TestCase):
    def test_password_random_and_not_in_evidence(self):
        a, b = h.random_password(), h.random_password()
        self.assertNotEqual(a, b)
        self.assertGreaterEqual(len(a), 24)
        self.assertFalse(h.evidence_has_secret({"x": 1}, a))
        self.assertTrue(h.evidence_has_secret({"x": a}, a))

    def test_phases_in_order(self):
        names = [p.__name__ for p in h.PHASES]
        self.assertEqual(names[0], "phase_reject")
        self.assertEqual(names[-1], "phase_reinstall")
        self.assertEqual(len(names), 7)


if __name__ == "__main__":
    unittest.main()
