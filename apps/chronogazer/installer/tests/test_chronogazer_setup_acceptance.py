"""chronogazer_setup_acceptance.py の純粋部分のテスト（SCM・実 setup は一切触らない）。

実行: python -B -m unittest discover -s apps/chronogazer/installer/tests -p test_chronogazer_setup_acceptance.py
"""

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chronogazer_setup_acceptance as c  # noqa: E402

OK_ENV = {
    "GITHUB_ACTIONS": "true",
    "RUNNER_ENVIRONMENT": "github-hosted",
    c.GUARD_ENV: c.GUARD_VALUE,
}
SERVER = Path(r"C:\Program Files\ChronoGazer\chronogazer-server.exe")


class GuardTests(unittest.TestCase):
    def test_ok(self):
        self.assertEqual(c.guard_problems(OK_ENV, "win32", True), [])

    def test_each_missing_condition_refuses(self):
        for key in OK_ENV:
            env = {k: v for k, v in OK_ENV.items() if k != key}
            self.assertTrue(c.guard_problems(env, "win32", True), key)
        self.assertTrue(c.guard_problems(OK_ENV, "linux", True))
        self.assertTrue(c.guard_problems(OK_ENV, "win32", False))
        self.assertTrue(c.guard_problems({**OK_ENV, "RUNNER_ENVIRONMENT": "self-hosted"}, "win32", True))
        self.assertTrue(c.guard_problems({**OK_ENV, c.GUARD_ENV: "1"}, "win32", True))

    def test_root_marker_problem(self):
        self.assertIn("not set", c.root_marker_problem({}))
        with tempfile.TemporaryDirectory() as d:
            self.assertIn("marker", c.root_marker_problem({c.ROOT_ENV: d}))
            (Path(d) / ".banto-973-ci").write_text("x", encoding="utf-8")
            self.assertIsNone(c.root_marker_problem({c.ROOT_ENV: d}))


class ParseTests(unittest.TestCase):
    def test_sc_query(self):
        t = (
            "SERVICE_NAME: ChronoGazer\n"
            "        STATE              : 4  RUNNING\n"
            "        PID                : 1234\n"
            "        FLAGS              :\n"
        )
        self.assertEqual(c.parse_sc_query(t), {"state": "RUNNING", "pid": 1234})
        self.assertEqual(
            c.parse_sc_query("[SC] EnumQueryServicesStatus:OpenService FAILED 1060"),
            {"state": None, "pid": None},
        )
        self.assertEqual(c.parse_sc_query(""), {"state": None, "pid": None})

    def test_sc_qc(self):
        t = (
            "        START_TYPE         : 3   DEMAND_START\n"
            "        BINARY_PATH_NAME   : C:\\Program Files\\ChronoGazer\\chronogazer-server.exe run-service\n"
        )
        d = c.parse_sc_qc(t)
        self.assertEqual(d["start_type"], "DEMAND_START")
        self.assertTrue(c.image_path_is(d["image_path"], SERVER))
        self.assertEqual(c.parse_sc_qc("")["start_type"], None)

    def test_image_path_exe_quoted_and_unquoted(self):
        exe = str(SERVER)
        self.assertEqual(c.image_path_exe(f'"{exe}" run-service'), exe)
        self.assertEqual(c.image_path_exe(f"{exe} run-service"), exe)
        self.assertEqual(c.image_path_exe(exe), exe)

    def test_image_path_is_exact_ignoring_case_only(self):
        exe = str(SERVER)
        self.assertTrue(c.image_path_is(f'"{exe.upper()}" run-service', SERVER))
        self.assertTrue(c.image_path_is(exe.lower(), SERVER))
        self.assertFalse(c.image_path_is(r"C:\Program Files\ChronoGazer\chronogazer-elev.exe", SERVER))
        self.assertFalse(c.image_path_is(r"C:\Program Files\ChronoGazer\sub\chronogazer-server.exe", SERVER))
        self.assertFalse(c.image_path_is(r"C:\ChronoGazerElsewhere\chronogazer-server.exe", SERVER))
        self.assertFalse(c.image_path_is("", SERVER))

    def test_icacls_explicit_users_aces(self):
        t = (
            "C:\\x BUILTIN\\Administrators:(F)\n"
            "    BUILTIN\\Users:(I)(RX)\n"
            "    BUILTIN\\Users:(M)\n"
        )
        self.assertEqual(c.icacls_explicit_users_aces(t), ["BUILTIN\\Users:(M)"])
        self.assertEqual(c.icacls_explicit_users_aces(""), [])

    def test_empty_observations_raise(self):
        with self.assertRaises(RuntimeError):
            c.parse_owner_output(0, "  \n", "")
        with self.assertRaises(RuntimeError):
            c.parse_owner_output(1, "BUILTIN\\Administrators", "boom")
        self.assertEqual(c.parse_owner_output(0, "BUILTIN\\Administrators\n", ""), "BUILTIN\\Administrators")
        with self.assertRaises(RuntimeError):
            c.parse_icacls_output(0, "", "")
        with self.assertRaises(RuntimeError):
            c.parse_icacls_output(5, "x", "denied")
        self.assertEqual(c.parse_icacls_output(0, "x", ""), "x")

    def test_find_setup_and_dist_names(self):
        with tempfile.TemporaryDirectory() as d:
            dist = Path(d)
            with self.assertRaises(c.PhaseFailure):
                c.find_setup(dist)
            (dist / "ChronoGazer_0.3.1_x64-setup.exe").write_bytes(b"x")
            setup, version = c.find_setup(dist)
            self.assertEqual((setup.name, version), ("ChronoGazer_0.3.1_x64-setup.exe", "0.3.1"))
            (dist / "ChronoGazer_0.3.2_x64-setup.exe").write_bytes(b"x")
            with self.assertRaises(c.PhaseFailure):
                c.find_setup(dist)
        self.assertEqual(
            c.dist_exe_name("chronogazer-server", "0.3.1"), "chronogazer-server-v0.3.1-windows-x86_64.exe"
        )

    def test_tree_hashes(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(c.tree_hashes(Path(d)), {})
            (Path(d) / "a.txt").write_text("a", encoding="utf-8")
            self.assertEqual(list(c.tree_hashes(Path(d))), ["a.txt"])


class HealthTests(unittest.TestCase):
    EP = {"schema": 1, "port": 8721, "pid": 4321, "scope": "0123456789abcdef", "version": "0.1.0", "hostKind": "service"}

    def test_parse_endpoint(self):
        d = c.parse_endpoint(json.dumps(self.EP))
        self.assertEqual((d["port"], d["scope"], d["pid"], d["host_kind"]), (8721, "0123456789abcdef", 4321, "service"))

    def test_parse_endpoint_rejects_unusable(self):
        for bad in (
            "",
            "{}",
            json.dumps({**self.EP, "port": 0}),
            json.dumps({**self.EP, "port": 70000}),
            json.dumps({**self.EP, "port": True}),
            json.dumps({**self.EP, "port": "8721"}),
            json.dumps({**self.EP, "scope": ""}),
            json.dumps({k: v for k, v in self.EP.items() if k != "scope"}),
        ):
            with self.assertRaises((RuntimeError, ValueError), msg=bad):
                c.parse_endpoint(bad)

    def test_host_healthy_only_when_all_match(self):
        body = {"product": "chronogazer", "hostKind": "service", "scope": "s", "version": "1"}
        self.assertTrue(c.host_healthy(200, body, "s"))
        self.assertFalse(c.host_healthy(500, body, "s"))
        self.assertFalse(c.host_healthy(200, body, "other"))
        self.assertFalse(c.host_healthy(200, {**body, "product": "banto-hub"}, "s"))
        self.assertFalse(c.host_healthy(200, {**body, "hostKind": "desktop"}, "s"))
        self.assertFalse(c.host_healthy(200, {k: v for k, v in body.items() if k != "scope"}, "s"))
        self.assertFalse(c.host_healthy(200, {}, "s"))
        self.assertFalse(c.host_healthy(200, [], "s"))

    def test_endpoint_pid_matches(self):
        self.assertTrue(c.endpoint_pid_matches(10, 10))
        for a, b in ((10, 11), (None, 10), (10, None), (0, 0), (-1, -1), (True, True), ("10", "10")):
            self.assertFalse(c.endpoint_pid_matches(a, b), msg=(a, b))

    def test_request_headers_carry_csrf_header(self):
        self.assertEqual(c.host_request_headers()["X-Banto-Client"], "banto")
        self.assertEqual(c.HOST_PATH, "/api/host")


class DbSentinelTests(unittest.TestCase):
    def test_write_then_read_only(self):
        with tempfile.TemporaryDirectory() as d:
            db = Path(d) / "chronogazer.sqlite3"
            import sqlite3

            con = sqlite3.connect(str(db))
            con.execute("CREATE TABLE other(x)")
            con.commit()
            con.close()
            c.write_db_sentinel(db, "abc")
            self.assertEqual(c.read_db_sentinel(db), ["abc"])

    def test_read_is_read_only(self):
        with tempfile.TemporaryDirectory() as d:
            db = Path(d) / "x.sqlite3"
            import sqlite3

            con = sqlite3.connect(str(db))
            con.execute(f"CREATE TABLE {c.SENTINEL_TABLE}(v TEXT)")
            con.commit()
            con.close()
            # 行が無い（空の観測）は例外
            with self.assertRaises(RuntimeError):
                c.read_db_sentinel(db)

    def test_missing_table_and_missing_file_raise(self):
        with tempfile.TemporaryDirectory() as d:
            db = Path(d) / "x.sqlite3"
            with self.assertRaises(Exception):
                c.read_db_sentinel(db)  # mode=ro は作らない
            self.assertFalse(db.exists())
            import sqlite3

            sqlite3.connect(str(db)).close()
            with self.assertRaises(Exception):
                c.read_db_sentinel(db)


class FaultHelperTests(unittest.TestCase):
    def test_sdshow(self):
        out = "\r\nD:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)\r\n"
        self.assertTrue(c.parse_sc_sdshow(0, out, "").startswith("D:("))
        for rc, o in ((1, out), (0, ""), (0, "[SC] OpenService FAILED 5")):
            with self.assertRaises(RuntimeError):
                c.parse_sc_sdshow(rc, o, "err")

    def test_deny_inserted_at_dacl_head(self):
        self.assertEqual(c.sddl_with_deny("D:(A;;GA;;;SY)", "SD"), "D:(D;;SD;;;BA)(A;;GA;;;SY)")
        self.assertEqual(c.sddl_with_deny("D:(A;;GA;;;SY)", "RC"), "D:(D;;RC;;;BA)(A;;GA;;;SY)")
        self.assertEqual(c.sddl_with_deny("D:PAI(A;;GA;;;SY)", "SD"), "D:PAI(D;;SD;;;BA)(A;;GA;;;SY)")
        self.assertEqual(
            c.sddl_with_deny("O:BAG:SYD:P(A;;GA;;;SY)S:(AU;FA;GA;;;WD)", "RC"),
            "O:BAG:SYD:P(D;;RC;;;BA)(A;;GA;;;SY)S:(AU;FA;GA;;;WD)",
        )
        self.assertEqual(
            c.sddl_with_deny("O:S-1-5-32-544G:DUD:(A;;GA;;;SY)", "SD"),
            "O:S-1-5-32-544G:DUD:(D;;SD;;;BA)(A;;GA;;;SY)",
        )

    def test_deny_without_dacl_raises(self):
        for s in ("", "O:BAG:SY", "S:(AU;FA;GA;;;WD)"):
            for right in ("SD", "RC"):
                with self.assertRaises(RuntimeError):
                    c.sddl_with_deny(s, right)

    def test_uninstall_in_place_command(self):
        self.assertEqual(
            c.uninstall_in_place_command(c.INSTDIR),
            '"C:\\Program Files\\ChronoGazer\\uninstall.exe" /S _?=C:\\Program Files\\ChronoGazer',
        )

    def test_count_service_start_failures(self):
        def ev(i, svc):
            return (
                f"Event[{i}]:\n  Log Name: System\n  Source: Service Control Manager\n  Event ID: 7000\n"
                f"  Description:\n  The {svc} service failed to start due to the following error:\n"
                "  %%193 is not a valid Win32 application.\n\n"
            )

        self.assertEqual(c.count_service_start_failures(0, "", ""), 0)
        out = ev(0, "ChronoGazer") + ev(1, "OtherSvc") + ev(2, "CHRONOGAZER")
        self.assertEqual(c.count_service_start_failures(0, out, ""), 2)
        self.assertEqual(c.count_service_start_failures(0, ev(0, "OtherSvc"), ""), 0)
        with self.assertRaises(RuntimeError):
            c.count_service_start_failures(1, out, "denied")

    def test_tree_snapshot_empty_raises(self):
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(RuntimeError):
                c.tree_snapshot(d)
            (Path(d) / "a").write_text("1")
            self.assertEqual(len(c.tree_snapshot(d)), 1)


class ImportSafetyTests(unittest.TestCase):
    def test_import_has_no_side_effects(self):
        self.assertEqual(c.EVIDENCE, {"phases": {}})
        self.assertIsNone(c.PROOF_DIR)
        self.assertEqual(len(c.PHASES), 16)

    def test_fault_phases_follow_reinstall_in_order(self):
        names = [ph.__name__ for ph in c.PHASES]
        i = names.index("phase_reinstall")
        self.assertEqual(names[:i], [
            "phase_reject", "phase_fresh", "phase_start_seed", "phase_acl_overwrite",
            "phase_stopped_overwrite", "phase_uninstall_running",
        ])
        self.assertEqual(
            names[i + 1 :],
            [
                "phase_install_locked_exe",
                "phase_install_acl_refused",
                "phase_install_preflight_refused",
                "phase_uninstall_locked_exe",
                "phase_uninstall_delete_fail",
                "phase_install_service_install_fails_running",
                "phase_restart_fails_corrupt_exe",
                "phase_visible_shell_quit_cancel",
                "phase_uninstall_delete_pending",
            ],
        )


if __name__ == "__main__":
    unittest.main()
