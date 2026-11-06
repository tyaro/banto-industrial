"""Pure stdlib regression for nsis_lock_abort_scm: no SCM/User32/NSIS, no windows opened.

From repository root:
  python -B -m unittest discover -s apps/chronogazer/installer/tests -p test_nsis_lock_abort_scm.py

Dialog selection, PPO audit/instrumentation, events order, timeline and heartbeat
binding are judged on fake data. The native orchestrator runs only in the
disposable hosted workflow (installer-lock-abort.yml).
"""

import json
import unittest

import nsis_lock_abort_scm as lock
from nsis_precopy import ROOT

PID_A, PID_B, RUN, SHA = 100, 200, "42", "a" * 40
DUMMY = r"C:\a\_temp\Banto973-x\dummy\running_dummy.exe"
PROBE = r"C:\a\_temp\Banto973-x\probe.exe"
EXPECTED = {"run_id": RUN, "sha": SHA, "root": r"C:\a\_temp\Banto973-x", "exe_sha256": "f" * 64,
            "service_name": "ChronoGazer", "image_path": f'"{DUMMY}" run-service --run-id {RUN} --sha {SHA}'}


def start(pid, wall_ms, **override):
    record = {"event": "start", "pid": pid, "run_id": RUN, "sha": SHA, "root": EXPECTED["root"],
              "exe_sha256": EXPECTED["exe_sha256"], "args": ["ChronoGazer"], "wall_ms": wall_ms}
    record.update(override)
    return record


def beat(seq, pid, **override):
    record = {"seq": seq, "pid": pid, "run_id": RUN, "sha": SHA, "wall_ms": 1000 + seq}
    record.update(override)
    return record


def window(hwnd, owner, title="Banto973 lock-abort 12345678", pid=7, cls="#32770", visible=True):
    return {"hwnd": hwnd, "pid": pid, "title": title, "class": cls, "visible": visible,
            "rect": [0, 0, 100, 100], "owner": owner}


MAIN_TEXT = "使用中、または書き込めない実行ファイルがあります（chronogazer-server.exe）。\r\n\r\n上書きはまだ始めていません。"


class Environment(unittest.TestCase):
    def test_requires_both_opt_ins(self):
        values = {"GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted", "RUNNER_OS": "Windows",
                  "BANTO_973_SCM_TEST": "github-hosted-disposable-only-v1"}
        with self.assertRaisesRegex(RuntimeError, "BANTO_973_GUI_TEST"):
            lock.validate_environment(values)
        lock.validate_environment(dict(values, BANTO_973_GUI_TEST="github-hosted-gui-capability-only-v1"))
        with self.assertRaisesRegex(RuntimeError, "BANTO_973_SCM_TEST"):
            lock.validate_environment(dict(values, BANTO_973_SCM_TEST="x", BANTO_973_GUI_TEST="github-hosted-gui-capability-only-v1"))


class Ownership(unittest.TestCase):
    def test_command_accepts_dummy_and_probe_only(self):
        self.assertTrue(lock.command_matches(EXPECTED["image_path"], DUMMY, PROBE, RUN, SHA))
        self.assertTrue(lock.command_matches('"\\\\?\\' + DUMMY + f'" run-service --run-id {RUN} --sha {SHA}', DUMMY, PROBE, RUN, SHA))
        self.assertTrue(lock.command_matches(f'"{PROBE}" run-service', DUMMY, PROBE, RUN, SHA))
        self.assertFalse(lock.command_matches(f'"{DUMMY}" run-service --run-id {RUN} --sha {"b" * 40}', DUMMY, PROBE, RUN, SHA))
        self.assertFalse(lock.command_matches(f'"{DUMMY}" run-service', DUMMY, PROBE, RUN, SHA))

    def test_binding_exact_keys_and_values(self):
        self.assertEqual(lock.binding_problems(dict(EXPECTED), EXPECTED), [])
        self.assertEqual(lock.binding_problems(dict(EXPECTED, root=EXPECTED["root"].lower()), EXPECTED), [])
        self.assertEqual(lock.binding_problems(dict(EXPECTED, extra="1"), EXPECTED), ["binding_keys"])
        self.assertEqual(lock.binding_problems(dict(EXPECTED, sha="b" * 40), EXPECTED), ["binding_sha"])
        self.assertEqual(lock.binding_problems(dict(EXPECTED, run_id=42), EXPECTED), ["binding_run_id"])
        self.assertEqual(lock.binding_problems([], EXPECTED), ["binding_keys"])


class Ppo(unittest.TestCase):
    PPO = ("Section\n"
           + lock.SC_QUERIES[0] + "\n" + lock.SC_STOP + "\n" + lock.SC_QUERIES[1] + "\n" + lock.SC_START + "\n"
           "System::Call 'kernel32::GetFileAttributesW(w \"$INSTDIR\\chronogazer-server.exe\") i .r0'\n"
           'MessageBox MB_OK|MB_ICONSTOP "使用中、または書き込めない実行ファイルがあります（$2）。\n\n上書きはまだ始めていません。" /SD IDOK\n'
           "SectionEnd\n")

    def test_dialog_kinds_from_hook_texts(self):
        self.assertEqual(lock.dialog_kind('MessageBox MB_OK "使用中、または書き込めない実行ファイルがあります（$2）。" /SD IDOK'), "main-dialog")
        self.assertEqual(lock.dialog_kind('MessageBox MB_OK "インストールを中止しましたが、その前に止めていた ChronoGazer サービスを再開できませんでした（$0）。" /SD IDOK'), "restart-failed-dialog")
        self.assertEqual(lock.dialog_kind('MessageBox MB_OK "ChronoGazer サービスがまだ止まっていません（RUNNING か PENDING のままです）。" /SD IDOK'), "inactive-dialog")
        self.assertEqual(lock.dialog_kind('MessageBox MB_OK|MB_ICONINFORMATION "$2" /SD IDOK'), "callback-dialog")
        self.assertEqual(lock.dialog_kind('MessageBox MB_OK "別の文面" /SD IDOK'), "other-dialog")

    def test_text_constants_match_the_unchanged_hooks(self):
        hooks = (ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh").read_text(encoding="utf-8")
        for text in (lock.MAIN_DIALOG_TEXT + "（$2）", lock.RESTART_FAILED_TEXT, *lock.INACTIVE_TEXTS,
                     'MessageBox MB_OK|MB_ICONINFORMATION "' + lock.CALLBACK_DIALOG_TEXT + '"'):
            self.assertIn(text, hooks)

    def test_audit_accepts_allowlist_and_counts(self):
        counts = lock.audit_install_ppo(self.PPO)
        self.assertEqual(counts[lock.SC_STOP], 1)
        self.assertEqual(counts[lock.SC_START], 1)

    def test_audit_rejects_exec_registry_relative_jump_and_missing_stop(self):
        for extra in ("ExecWait '\"$INSTDIR\\chronogazer-elev.exe\" service-install' $0",
                      'ReadRegStr $1 HKLM "SYSTEM\\CurrentControlSet\\Services\\ChronoGazer" "ImagePath"',
                      "nsExec::ExecToStack 'sc delete \"ChronoGazer\"'", "Goto +2"):
            with self.subTest(extra=extra), self.assertRaises(RuntimeError):
                lock.audit_install_ppo(self.PPO.replace("SectionEnd", extra + "\nSectionEnd"))
        with self.assertRaisesRegex(RuntimeError, "missing"):
            lock.audit_install_ppo(self.PPO.replace(lock.SC_STOP + "\n", ""))

    def test_instrument_logs_before_each_production_line(self):
        out = lock.instrument(self.PPO, r"C:\t\events.log").splitlines()
        for production, marker in ((lock.SC_STOP, "stop-attempt"), (lock.SC_START, "restart-attempt"),
                                   (lock.SC_QUERIES[0], "sc-query")):
            index = out.index(production)
            self.assertEqual(out[index - 3], f'FileWriteUTF16LE $R9 "{marker}$\\r$\\n"')
        dialog = next(i for i, line in enumerate(out) if line.startswith("MessageBox "))
        self.assertEqual(out[dialog - 3], 'FileWriteUTF16LE $R9 "main-dialog flag=$ChronoGazerWasRunning$\\r$\\n"')
        self.assertIn("$\\r$\\n$\\r$\\n上書き", out[dialog])  # PPO の物理改行を戻してから再コンパイルする。
        with self.assertRaisesRegex(RuntimeError, "unclassified_exec"):
            lock.instrument("nsExec::ExecToStack 'whoami'\n", r"C:\t\events.log")

    def test_template_is_non_silent_without_shell_quit(self):
        self.assertIn("SilentInstall normal", lock.TEMPLATE)
        self.assertNotIn("QuitShellForUpdate", lock.TEMPLATE)
        self.assertLess(lock.TEMPLATE.index("StopServiceIfRunning"), lock.TEMPLATE.index("RequireChronoGazerServiceInactive"))
        self.assertLess(lock.TEMPLATE.index("RequireChronoGazerServiceInactive"), lock.TEMPLATE.index("RequireChronoGazerInstallExesUnlocked"))


class Parsing(unittest.TestCase):
    def test_events_drop_partial_tail(self):
        raw = "section-begins\r\nstop-attempt\r\nrest".encode("utf-16-le") + b"\x00"
        self.assertEqual(lock.parse_events(raw), ["section-begins", "stop-attempt"])
        self.assertEqual(lock.parse_events(b""), [])

    def test_jsonl_drops_partial_tail_but_not_broken_lines(self):
        self.assertEqual(lock.parse_jsonl('{"seq":1}\n{"seq":2}\n{"se'), [{"seq": 1}, {"seq": 2}])
        with self.assertRaises(json.JSONDecodeError):
            lock.parse_jsonl('{"seq":1\n{"seq":2}\n')


class Events(unittest.TestCase):
    AT_DIALOG = ["section-begins", "sc-query", "stop-attempt", "sc-query", "preinstall-done flag=1",
                 "restart-attempt", "sc-query", "main-dialog flag=0"]

    def test_at_dialog_and_final_sequences(self):
        self.assertEqual(lock.event_problems(self.AT_DIALOG, final=False), [])
        final = self.AT_DIALOG + ["callback-before flag=0", "callback-after flag=0"]
        self.assertEqual(lock.event_problems(final, final=True), [])
        self.assertTrue(lock.event_problems(final, final=False))

    def test_rejects_double_restart_copy_order_and_flags(self):
        bad = (
            self.AT_DIALOG + ["copy-begins"],
            self.AT_DIALOG[:-1] + ["restart-attempt", "main-dialog flag=0"],
            [e for e in self.AT_DIALOG if e != "restart-attempt"][:-1] + ["main-dialog flag=0", "restart-attempt"],
            [e.replace("flag=1", "flag=0") for e in self.AT_DIALOG],
            self.AT_DIALOG[:-1] + ["restart-failed-dialog flag=0", "main-dialog flag=0"],
        )
        for events in bad:
            with self.subTest(events=events):
                self.assertTrue(lock.event_problems(events, final=False))
        final = self.AT_DIALOG + ["callback-before flag=1", "restart-attempt", "callback-after flag=0"]
        self.assertTrue(lock.event_problems(final, final=True))
        callback_dialog = self.AT_DIALOG + ["callback-before flag=0", "callback-dialog flag=0", "callback-after flag=0"]
        self.assertTrue(lock.event_problems(callback_dialog, final=True))


class StartsStopsHeartbeat(unittest.TestCase):
    def test_starts_bound_and_restart_before_dialog(self):
        starts = [start(PID_A, 10), start(PID_B, 50)]
        self.assertEqual(lock.start_problems(starts, EXPECTED, [PID_A, PID_B], 60), [])
        self.assertIn("restart_not_before_dialog", lock.start_problems(starts, EXPECTED, [PID_A, PID_B], 50))
        refused = starts + [{"event": "refused", "pid": 300, "reason": "x", "wall_ms": 70}]
        self.assertIn("starts_count_3", lock.start_problems(refused, EXPECTED, [PID_A, PID_B]))
        self.assertIn("start1_sha", lock.start_problems([starts[0], start(PID_B, 50, sha="b")], EXPECTED, [PID_A, PID_B]))
        self.assertIn("start1_args", lock.start_problems([starts[0], start(PID_B, 50, args=[])], EXPECTED, [PID_A, PID_B]))
        self.assertIn("start0_root", lock.start_problems([start(PID_A, 1, root="C:\\other")], EXPECTED, [PID_A]))

    def test_stops_only_from_installer(self):
        stop = {"event": "stop", "pid": PID_A, "signal": "Stop", "seq_at_stop": 3, "wall_ms": 5}
        self.assertEqual(lock.stop_problems([stop], [PID_A]), [])
        self.assertIn("stops_count_2", lock.stop_problems([stop, dict(stop, pid=PID_B)], [PID_A]))
        self.assertIn("stop0_mismatch", lock.stop_problems([dict(stop, signal="Preshutdown")], [PID_A]))

    def test_heartbeat_binding_and_progress(self):
        earlier = [beat(5, PID_A), beat(6, PID_A)]  # pid_b の最初の heartbeat より先に dialog を見た。
        self.assertEqual(lock.heartbeat_problems(earlier, [*earlier, beat(1, PID_B), beat(2, PID_B)], PID_A, PID_B, RUN, SHA), [])
        earlier = [beat(6, PID_A), beat(1, PID_B)]
        self.assertEqual(lock.heartbeat_problems(earlier, [*earlier, beat(2, PID_B)], PID_A, PID_B, RUN, SHA), ["heartbeat_not_advancing"])
        self.assertEqual(lock.heartbeat_problems(earlier, [*earlier, beat(3, PID_B, sha="b")], PID_A, PID_B, RUN, SHA), ["heartbeat_not_bound_to_pid_b"])
        self.assertEqual(lock.heartbeat_problems(earlier, [*earlier, beat(9, PID_A)], PID_A, PID_B, RUN, SHA), ["heartbeat_not_bound_to_pid_b"])
        self.assertIn("heartbeat_foreign_pid", lock.heartbeat_problems([beat(1, 999)], [beat(3, PID_B)], PID_A, PID_B, RUN, SHA))
        self.assertEqual(lock.heartbeat_problems([], [], PID_A, PID_B, RUN, SHA), ["heartbeat_not_bound_to_pid_b"])


class Timeline(unittest.TestCase):
    def entries(self, *rows):
        return [{"phase": phase, "state": state, "pid": pid} for phase, state, pid in rows]

    def test_steady_running_pid_b_from_dialog_to_exit(self):
        good = self.entries(("before-dialog", 1, 0), ("before-dialog", 2, PID_B), ("dialog", 4, PID_B),
                            ("after-ok", 4, PID_B), ("closing", 4, PID_B), ("after-exit", 4, PID_B))
        self.assertEqual(lock.timeline_problems(good, PID_B), [])
        flapped = good[:3] + self.entries(("after-ok", 1, 0), ("after-ok", 4, 300)) + good[4:]
        self.assertEqual(lock.timeline_problems(flapped, PID_B), ["service_not_steady_running_pid_b"])
        self.assertEqual(lock.timeline_problems(good[:3], PID_B), ["timeline_phases_missing"])


class Dialogs(unittest.TestCase):
    CAPTION = "Banto973 lock-abort 12345678"

    def test_children_must_have_readable_static(self):
        self.assertEqual(lock.classify_dialog_children([{"class": "Static", "title": ""}, {"class": "Button", "title": "OK"}]), "waiting")
        self.assertEqual(lock.classify_dialog_children([{"class": "Static", "title": ""}, {"class": "Static", "title": MAIN_TEXT}]), "main")
        self.assertEqual(lock.classify_dialog_children([{"class": "Static", "title": "使用中、または書き込めない実行ファイルがあります（chronogazer.exe）"}]), "other")

    def test_owner_pid_title_and_text_identify_the_main_dialog(self):
        main_window = window(10, 0)  # NSIS 主窓も #32770・同 title だが owner が 0。
        dialog = window(11, 10)
        foreign = window(12, 10, pid=8)
        children = {10: [{"class": "Static", "title": MAIN_TEXT}], 11: [{"class": "Static", "title": MAIN_TEXT}]}
        found, unexpected = lock.judge_dialogs([main_window, dialog, foreign], children.get, 7, self.CAPTION)
        self.assertEqual((found, unexpected), (dialog, []))
        self.assertEqual(lock.main_windows([main_window, dialog, foreign], 7), [main_window])

    def test_waiting_unexpected_and_ambiguous(self):
        dialog = window(11, 10)
        self.assertEqual(lock.judge_dialogs([dialog], lambda h: [{"class": "Static", "title": ""}], 7, self.CAPTION), (None, []))
        other = window(13, 10, title="別の title")
        self.assertEqual(lock.judge_dialogs([other], lambda h: [{"class": "Static", "title": MAIN_TEXT}], 7, self.CAPTION), (None, [other]))
        hidden = window(14, 10, visible=False)
        self.assertEqual(lock.judge_dialogs([hidden], lambda h: [{"class": "Static", "title": MAIN_TEXT}], 7, self.CAPTION), (None, []))
        with self.assertRaisesRegex(RuntimeError, "ambiguous"):
            lock.judge_dialogs([dialog, window(15, 10)], lambda h: [{"class": "Static", "title": MAIN_TEXT}], 7, self.CAPTION)


if __name__ == "__main__":
    unittest.main()
