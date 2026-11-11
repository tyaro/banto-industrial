"""chronogazer_setup_acceptance.py の純粋部分のテスト（SCM・実 setup は一切触らない）。

実行: python -B -m unittest discover -s apps/chronogazer/installer/tests -p test_chronogazer_setup_acceptance.py
"""

import json
import sys
import tempfile
import unittest
from unittest import mock
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


class ServiceLeftRunningTests(unittest.TestCase):
    def test_table(self):
        self.assertFalse(c.service_left_running({"exit": 0, "state": "RUNNING", "pid": 1}))
        self.assertTrue(c.service_left_running({"exit": 0, "state": "STOPPED", "pid": 0}))
        self.assertTrue(c.service_left_running({"exit": 0, "state": "STOP_PENDING", "pid": 5}))

    def test_unusable_sc_result_raises(self):
        bad = ({"exit": 1060, "state": None}, {"exit": 1, "state": "RUNNING"}, {"exit": 0, "state": None}, {"exit": 0, "state": ""}, {})
        for st in bad:
            with self.assertRaises(RuntimeError, msg=st):
                c.service_left_running(st)


class WaitUninstallDoneTests(unittest.TestCase):
    def _run(self, **kw):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "uninstall.exe").write_text("x")
            with (
                mock.patch.object(c, "INSTDIR", Path(d)),
                mock.patch.object(c, "service_key_exists", return_value=False),
                mock.patch.object(c, "sc_state", return_value={"state": None}),
                mock.patch.object(c, "uninstall_registration", return_value=[]),
                mock.patch.object(c, "wait_until", side_effect=lambda fn, *a, **k: fn()),
                mock.patch.object(c, "obs", lambda *a, **k: None),
            ):
                c._wait_uninstall_done("p", **kw)

    def test_in_place_run_only_requires_exes_gone(self):
        self._run(files=c.EXES)

    def test_default_files_require_uninstall_exe_gone(self):
        with self.assertRaises(c.PhaseFailure):
            self._run()


class ThreadsToSuspendTests(unittest.TestCase):
    def test_excludes_the_oldest_as_main(self):
        self.assertEqual(c.threads_to_suspend([(10, 300), (11, 100), (12, 200)], set()), [10, 12])

    def test_skips_done_but_still_picks_main_from_all(self):
        self.assertEqual(c.threads_to_suspend([(10, 300), (11, 100), (12, 200)], {12}), [10])
        self.assertEqual(c.threads_to_suspend([(10, 300), (11, 100), (12, 200)], {10, 12}), [])

    def test_main_only_means_nothing_to_suspend(self):
        self.assertEqual(c.threads_to_suspend([(11, 100)], set()), [])

    def test_new_thread_is_picked_up_in_a_later_pass(self):
        self.assertEqual(c.threads_to_suspend([(11, 100), (10, 300), (13, 400)], {10}), [13])

    def test_empty_raises(self):
        with self.assertRaises(RuntimeError):
            c.threads_to_suspend([], set())

    def test_tied_oldest_takes_the_first_enumerated_as_main(self):
        self.assertEqual(c.threads_to_suspend([(10, 100), (11, 100), (12, 200)], set()), [11, 12])
        self.assertEqual(c.threads_to_suspend([(11, 100), (10, 100), (12, 200)], set()), [10, 12])

    def test_tie_not_at_the_oldest_is_fine(self):
        self.assertEqual(c.threads_to_suspend([(10, 100), (11, 200), (12, 200)], set()), [11, 12])


class ImportSafetyTests(unittest.TestCase):
    def test_uninstall_in_place_command_mode_flag(self):
        self.assertEqual(
            c.uninstall_in_place_command(c.INSTDIR, "/P"),
            '"C:\\Program Files\\ChronoGazer\\uninstall.exe" /P _?=C:\\Program Files\\ChronoGazer',
        )

    def test_import_has_no_side_effects(self):
        self.assertEqual(c.EVIDENCE, {"phases": {}})
        self.assertIsNone(c.PROOF_DIR)
        self.assertEqual(len(c.PHASES), 26)

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
                "phase_visible_recheck_cancel",
                "phase_visible_retry_continues",
                "phase_visible_uninstall_recheck_cancel",
                "phase_visible_recheck_retry_continues",
                "phase_visible_uninstall_recheck_retry_continues",
                "phase_visible_locked_exe_abort",
                "phase_visible_locked_exe_restart_fails",
                "phase_visible_acl_refused_abort",
                "phase_visible_acl_refused_restart_fails",
                "phase_visible_stop_timeout_abort",
                "phase_uninstall_delete_pending",
            ],
        )


if __name__ == "__main__":
    unittest.main()
