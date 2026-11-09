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
        self.assertTrue(lock.command_matches(f'{DUMMY} run-service --run-id {RUN} --sha {SHA}', DUMMY, PROBE, RUN, SHA))
        self.assertFalse(lock.command_matches(f'{DUMMY} run-service --run-id other --sha {SHA}', DUMMY, PROBE, RUN, SHA))
        self.assertFalse(lock.command_matches(f'{DUMMY} run-service --run-id {RUN} --sha {"b" * 40}', DUMMY, PROBE, RUN, SHA))
        self.assertFalse(lock.command_matches(f'{DUMMY} run-service --run-id {RUN} --sha {SHA} --extra', DUMMY, PROBE, RUN, SHA))
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

    def test_ok_button_must_be_unique_visible_button(self):
        def btn(title="OK", visible=True, cls="Button", hwnd=5):
            return {"hwnd": hwnd, "class": cls, "title": title, "visible": visible}

        static = {"hwnd": 4, "class": "Static", "title": "x", "visible": True}
        self.assertEqual(lock.ok_button([static, btn(cls="BUTTON")])["hwnd"], 5)
        for children in ([static], [btn(), btn(hwnd=6)], [btn(visible=False)], [btn(title="Cancel")]):
            with self.subTest(children=children), self.assertRaisesRegex(RuntimeError, "ok_button_not_unique"):
                lock.ok_button(children)

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


ACL = lock.CASES["C-acl-subdir"]
INSTALL = r"C:\a\_temp\Banto973-x\proof\C-acl-subdir\install"
ACL_TEXT = (f"インストール先 {INSTALL} の所有者・アクセス権を既定に戻せませんでした。\r\n"
            "インストール先の中にサブフォルダがあります。ChronoGazer のインストーラはサブフォルダを作らないので、ChronoGazer の物ではありません。\r\n"
            f"対象: {INSTALL}\\zz-unexpected-subdir\r\n\r\n既存のフォルダのアクセス権が弱いまま...")
ICACLS_SETOWNER = next(line for line in lock.ACL_PPO_EXTRA if "/setowner" in line)
ICACLS_RESET = next(line for line in lock.ACL_PPO_EXTRA if "/reset" in line)
SDDL_EXPLICIT = "O:BAG:SYD:PAI(A;;FA;;;SY)(A;;0x1301bf;;;BU)(A;ID;FA;;;BA)(A;ID;0x1200a9;;;WD)"
SDDL_INHERITED = "O:BAG:SYD:AI(A;ID;FA;;;SY)(A;OICIID;0x1200a9;;;BU)"


class AclCase(unittest.TestCase):
    def test_explicit_ace_sids_skip_inherited_and_collect_all_explicit(self):
        self.assertEqual(lock.explicit_ace_sids(SDDL_EXPLICIT), {"SY", "BU"})
        self.assertEqual(lock.explicit_ace_sids(SDDL_INHERITED), set())
        self.assertEqual(lock.explicit_ace_sids("O:BAG:SYD:PAI(A;OICI;0x1301bf;;;BU)(A;;FA;;;BA)S:(AU;SA;FA;;;WD)"), {"BU", "BA"})
        self.assertEqual(lock.explicit_ace_sids("O:BAG:SY"), set())

    def test_saved_sddl_reads_second_line_of_utf16(self):
        raw = ("C:\\x\\data.txt\r\n" + SDDL_EXPLICIT + "\r\n\r\n").encode("utf-16-le")
        self.assertEqual(lock.saved_sddl(raw), SDDL_EXPLICIT)
        self.assertEqual(lock.saved_sddl(b"\xff\xfe" + raw), SDDL_EXPLICIT)
        with self.assertRaisesRegex(RuntimeError, "unparsable"):
            lock.saved_sddl("only one line".encode("utf-16-le"))

    def test_acl_problems(self):
        labels = ("install-root", "chronogazer.exe", "chronogazer-server.exe", "chronogazer-elev.exe", "data.txt", "subdir")
        self.assertEqual(lock.ACL_LABELS, labels)
        weak = {label: {"BU"} for label in labels}
        weak["data.txt"] = {"BU", "SY"}
        both = ["chronogazer.exe", "data.txt", "chronogazer-elev.exe", "chronogazer-server.exe"]
        good_after = dict(weak, **{label: set() for label in labels if label != "subdir"})
        self.assertEqual(lock.acl_problems(weak, good_after, both), [])
        self.assertIn("acl_before_missing_data.txt", lock.acl_problems(dict(weak, **{"data.txt": set()}), good_after, both))
        self.assertIn("acl_before_missing_install-root", lock.acl_problems(dict(weak, **{"install-root": set()}), good_after, both))
        self.assertIn("acl_after_still_weak_data.txt", lock.acl_problems(weak, dict(good_after, **{"data.txt": {"BU"}}), both))
        self.assertIn("acl_after_changed_subdir", lock.acl_problems(weak, dict(good_after, subdir=set()), both))
        self.assertIn("acl_after_still_weak_install-root",
                      lock.acl_problems(weak, dict(good_after, **{"install-root": {"BU"}}), both))

    def test_acl_problems_cover_every_exe(self):
        weak = {label: {"BU"} for label in lock.ACL_LABELS}
        gone = {label: set() for label in lock.ACL_LABELS if label != "subdir"}
        gone["subdir"] = {"BU"}
        every = [label for label in lock.ACL_LABELS if label.endswith((".exe", ".txt"))]
        for label in ("chronogazer-server.exe", "chronogazer-elev.exe"):
            # processed にあるのに BU が残る。
            self.assertIn("acl_after_still_weak_" + label,
                          lock.acl_problems(weak, dict(gone, **{label: {"BU"}}), every))
            # processed に無いのに BU が消える。
            self.assertIn("acl_after_changed_" + label,
                          lock.acl_problems(weak, gone, [name for name in every if name != label]))
            # grant 前に BU が無い。
            self.assertIn("acl_before_missing_" + label, lock.acl_problems(dict(weak, **{label: set()}), gone, every))

    def test_acl_problems_follow_processed(self):
        weak = {label: {"BU"} for label in lock.ACL_LABELS}
        # サブフォルダが最初: 何も処理されず、root だけ消える。ファイルは残る。
        first = dict(weak, **{"install-root": set()})
        self.assertEqual(lock.acl_problems(weak, first, []), [])
        # 処理済みなのに残る / 未処理なのに消える。
        self.assertIn("acl_after_still_weak_data.txt", lock.acl_problems(weak, first, ["data.txt"]))
        gone = dict(first, **{"data.txt": set()})
        self.assertIn("acl_after_changed_data.txt", lock.acl_problems(weak, gone, []))
        self.assertEqual(lock.acl_problems(weak, gone, ["data.txt"]), [])
        # processed が空でも root が残れば拒否する。
        self.assertIn("acl_after_still_weak_install-root", lock.acl_problems(weak, dict(first, **{"install-root": {"BU"}}), []))

    PPO = ("Section\n" + lock.SC_QUERIES[0] + "\n" + lock.SC_STOP + "\n" + lock.SC_START + "\n"
           + ICACLS_SETOWNER + "\n" + ICACLS_RESET + "\n"
           "System::Call 'kernel32::GetFileAttributesW(w r4) i .r5'\n"
           'MessageBox MB_OK|MB_ICONSTOP "インストール先 $INSTDIR の所有者・アクセス権を既定に戻せませんでした。" /SD IDOK\n'
           "SectionEnd\n")

    def test_ppo_allowlist_is_per_case(self):
        self.assertEqual(lock.audit_install_ppo(self.PPO, ACL.allowed_ppo)[ICACLS_RESET], 1)
        for audit in (lambda: lock.audit_install_ppo(self.PPO),
                      lambda: lock.audit_install_ppo(self.PPO, lock.LOCK_SPEC.allowed_ppo)):
            with self.assertRaisesRegex(RuntimeError, "outside_allowlist"):
                audit()
        with self.assertRaisesRegex(RuntimeError, "outside_allowlist"):
            lock.audit_install_ppo(self.PPO.replace("/reset /L", "/reset /T"), ACL.allowed_ppo)

    def test_acl_allowlist_strings_match_the_unchanged_common_nsh(self):
        common = (ROOT / "installer-common/install-dir-common.nsh").read_text(encoding="utf-8")
        self.assertIn(ICACLS_SETOWNER, common)
        self.assertIn(ICACLS_RESET, common)
        self.assertIn("System::Call 'kernel32::GetFileAttributesW(w r4) i .r5'", common)
        for text in (lock.ACL_MAIN_FIXED, lock.ACL_REASON_TEXT):
            self.assertIn(text, common)

    def test_instrument_classifies_icacls_and_acl_dialog(self):
        out = lock.instrument(self.PPO, r"C:\t\events.log", ACL.main_text).splitlines()
        for production in (ICACLS_SETOWNER, ICACLS_RESET):
            self.assertEqual(out[out.index(production) - 3], 'FileWriteUTF16LE $R9 "icacls-attempt target=$4$\\r$\\n"')
        dialog = next(i for i, line in enumerate(out) if line.startswith("MessageBox "))
        self.assertEqual(out[dialog - 3], 'FileWriteUTF16LE $R9 "main-dialog flag=$ChronoGazerWasRunning$\\r$\\n"')

    def test_dialog_kind_for_acl_text(self):
        line = 'MessageBox MB_OK|MB_ICONSTOP "インストール先 $INSTDIR の所有者・アクセス権を既定に戻せませんでした。$\\r$\\n理由" /SD IDOK'
        self.assertEqual(lock.dialog_kind(line, ACL.main_text), "main-dialog")
        self.assertEqual(lock.dialog_kind(line), "other-dialog")  # 既定（lock）の文面では main にならない。

    def test_static_must_match_reason_and_target(self):
        self.assertTrue(ACL.dialog_matches(ACL_TEXT, INSTALL))
        self.assertFalse(ACL.dialog_matches(ACL_TEXT.replace("サブフォルダがあります", "再解析ポイントがあります"), INSTALL))
        self.assertFalse(ACL.dialog_matches(ACL_TEXT.replace("zz-unexpected-subdir", "data.txt"), INSTALL))
        self.assertFalse(ACL.dialog_matches(ACL_TEXT, r"C:\other"))
        self.assertFalse(ACL.dialog_matches(MAIN_TEXT, INSTALL))
        self.assertFalse(lock.LOCK_SPEC.dialog_matches(ACL_TEXT, INSTALL))
        matches = lambda text: ACL.dialog_matches(text, INSTALL)
        children = {11: [{"class": "Static", "title": ACL_TEXT}], 13: [{"class": "Static", "title": MAIN_TEXT}]}
        caption = "Banto973 lock-abort 12345678"
        self.assertEqual(lock.judge_dialogs([window(11, 10)], children.get, 7, caption, matches), (window(11, 10), []))
        other = window(13, 10)
        self.assertEqual(lock.judge_dialogs([other], children.get, 7, caption, matches), (None, [other]))
        self.assertEqual(lock.classify_dialog_children([{"class": "Static", "title": ACL_TEXT}], matches=matches), "main")

    HEAD = ["section-begins", "sc-query", "stop-attempt", "sc-query", "preinstall-done flag=1"]
    TAIL = ["restart-attempt", "sc-query", "main-dialog flag=0"]

    @staticmethod
    def icacls(*names):
        """root が 2 回 → 各項目が 2 回ずつ（間に sc-query）。"""
        targets = [INSTALL] + [INSTALL + "\\" + name for name in names]
        return [item for t in targets for item in ("icacls-attempt target=" + t, "sc-query") * 2]

    def at_dialog(self, *names):
        return self.HEAD + self.icacls(*names) + self.TAIL

    # 列挙順は保証されない: サブフォルダが最初（processed が空）・途中・最後の全順序。
    ORDERS = ([], ["data.txt"], ["chronogazer-elev.exe", "data.txt"],
              ["chronogazer.exe", "chronogazer-server.exe", "chronogazer-elev.exe", "data.txt"])

    def test_acl_event_sequence(self):
        for names in self.ORDERS:
            with self.subTest(names=names):
                events = self.at_dialog(*names)
                self.assertEqual(lock.acl_processed(events, INSTALL), (names, []))
                self.assertEqual(lock.event_problems(events, final=False, spec=ACL, install=INSTALL), [])
                final = events + ["callback-before flag=0", "callback-after flag=0"]
                self.assertEqual(lock.event_problems(final, final=True, spec=ACL, install=INSTALL), [])
        self.assertEqual(lock.acl_processed(self.at_dialog("DATA.TXT"), INSTALL.upper())[1], [])  # casefold。
        self.assertTrue(lock.event_problems(self.at_dialog("data.txt"), final=False))  # lock の列とは違う。

    def test_acl_processed_rejects_bad_targets(self):
        sub = "icacls-attempt target=" + INSTALL + "\\" + lock.ACL_SUBDIR
        head, tail, root = self.HEAD, self.TAIL, ["icacls-attempt target=" + INSTALL] * 2
        item = ["icacls-attempt target=" + INSTALL + "\\data.txt"] * 2
        bad = {
            "odd": head + root + item[:1] + tail,
            "root_not_first": head + item + root + tail,
            "subdir_processed": head + root + [sub] * 2 + tail,
            "duplicate": head + root + item + item + tail,
            "outside_install": head + root + ["icacls-attempt target=C:\\other\\data.txt"] * 2 + tail,
            "nested": head + root + ["icacls-attempt target=" + INSTALL + "\\a\\data.txt"] * 2 + tail,
            "unpaired": head + root + ["icacls-attempt target=" + INSTALL + "\\data.txt",
                                       "icacls-attempt target=" + INSTALL + "\\chronogazer.exe"] + tail,
            "none": head + tail,
            "after_restart": head + root + tail[:1] + item + tail[1:],
            "not_contiguous": head + root[:1] + ["sc-stop-x"] + root[1:] + tail,
            "before_preinstall": head[:4] + root + head[4:] + tail,
        }
        for name, events in bad.items():
            with self.subTest(name=name):
                self.assertTrue(lock.acl_processed(events, INSTALL)[1])
                self.assertTrue(lock.event_problems(events, final=False, spec=ACL, install=INSTALL))

    def test_acl_events_reject_wrong_icacls_count_copy_and_double_restart(self):
        names = ["data.txt"]
        events = self.at_dialog(*names)
        one_less = list(events)
        one_less.remove("icacls-attempt target=" + INSTALL + "\\data.txt")
        bad = (one_less, events + ["copy-begins"], events[:-1] + ["restart-attempt", "main-dialog flag=0"],
               [e.replace("flag=1", "flag=0") for e in events])
        for bad_events in bad:
            with self.subTest(events=bad_events):
                self.assertTrue(lock.event_problems(bad_events, final=False, spec=ACL, install=INSTALL))
        final = events + ["callback-before flag=0", "restart-attempt", "callback-after flag=0"]
        self.assertTrue(lock.event_problems(final, final=True, spec=ACL, install=INSTALL))

    def test_acl_step_sits_before_unlocked_check_and_is_absent_for_lock(self):
        template = lock.TEMPLATE
        self.assertLess(template.index("@LOG_PREINSTALL@"), template.index("@ACL_STEP@"))
        self.assertLess(template.index("@ACL_STEP@"), template.index("RequireChronoGazerInstallExesUnlocked"))
        acl = lock.apply_acl_step(template, ACL)
        self.assertNotIn("@ACL_STEP@", acl)
        self.assertLess(acl.index("RequireChronoGazerServiceInactive"), acl.index("!insertmacro ResetChronoGazerInstallDirAcl"))
        self.assertLess(acl.index("!insertmacro ResetChronoGazerInstallDirAcl"), acl.index("RequireChronoGazerInstallExesUnlocked"))
        plain = lock.apply_acl_step(template, lock.LOCK_SPEC)
        self.assertNotIn("ResetChronoGazerInstallDirAcl", plain)
        self.assertNotIn("@ACL_STEP@", plain)
        self.assertEqual(plain, template.replace("  @ACL_STEP@\n", ""))

    def test_cases_are_keyed_by_name(self):
        self.assertEqual(set(lock.CASES), {"C-server-locked", "C-acl-subdir", "C-restart-fail", "C-stop-timeout", "C-copy-fail"})
        for name in ("C-server-locked", "C-acl-subdir", "C-restart-fail"):
            self.assertFalse(lock.CASES[name].stop_held)
            self.assertEqual(lock.CASES[name].dialog_wait, 90)
        self.assertTrue(lock.LOCK_SPEC.lock_server and not lock.LOCK_SPEC.acl_step)
        self.assertTrue(ACL.acl_step and not ACL.lock_server)
        self.assertEqual(lock.LOCK_SPEC.events_at_dialog, lock.EVENTS_AT_DIALOG)


class RestartFail(unittest.TestCase):
    SPEC = lock.CASES["C-restart-fail"]
    WARNING_TEXT = (lock.RESTART_WARNING_PREFIX + "\r\n\r\nサービスの状態を確かめ（sc query ChronoGazer）、"
                    "管理者権限の PowerShell から sc start ChronoGazer で開始してください。")

    def test_start_type_is_disabled(self):
        self.assertTrue(lock.start_type_is_disabled("        START_TYPE         : 4   DISABLED"))
        self.assertTrue(lock.start_type_is_disabled("START_TYPE  : 4"))
        self.assertFalse(lock.start_type_is_disabled("        START_TYPE         : 3   DEMAND_START"))
        self.assertFalse(lock.start_type_is_disabled("START_TYPE : 40"))

    def test_restart_warning_matches(self):
        self.assertTrue(lock.restart_warning_matches(self.WARNING_TEXT))
        self.assertFalse(lock.restart_warning_matches(self.WARNING_TEXT.replace("（1058）", "（1062）")))
        self.assertFalse(lock.restart_warning_matches(MAIN_TEXT))

    def test_warning_text_matches_the_unchanged_hooks(self):
        hooks = (ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh").read_text(encoding="utf-8")
        self.assertIn(lock.RESTART_WARNING_PREFIX.replace("インストール", "${action}").replace("（1058）", "（$0）"), hooks)

    def test_dialog_kind_of_the_warning_line(self):
        line = ('MessageBox MB_OK|MB_ICONEXCLAMATION "インストールを中止しましたが、その前に止めていた ChronoGazer サービスを'
                '再開できませんでした（$0）。$\\r$\\n$\\r$\\nサービスの状態を確かめ" /SD IDOK')
        self.assertEqual(lock.dialog_kind(line), "restart-failed-dialog")

    def test_stopped_heartbeat_problems(self):
        earlier = [beat(5, PID_A), beat(6, PID_A)]
        self.assertEqual(lock.stopped_heartbeat_problems(earlier, list(earlier), PID_A), [])
        self.assertIn("heartbeat_advanced_while_stopped", lock.stopped_heartbeat_problems(earlier, [*earlier, beat(7, PID_A)], PID_A))
        self.assertIn("heartbeat_foreign_pid", lock.stopped_heartbeat_problems(earlier, [*earlier, beat(1, PID_B)], PID_A))
        self.assertTrue(lock.stopped_heartbeat_problems([], [], PID_A))
        self.assertTrue(lock.stopped_heartbeat_problems(earlier, [], PID_A))

    def test_stopped_timeline_problems(self):
        def entries(*rows):
            return [{"phase": phase, "state": state, "pid": pid} for phase, state, pid in rows]

        ok = entries(("before-dialog", lock.RUNNING, PID_A), ("warning", lock.STOPPED, 0), ("between", lock.STOPPED, 0),
                     ("dialog", lock.STOPPED, 0), ("after-exit", lock.STOPPED, 0))
        self.assertEqual(lock.stopped_timeline_problems(ok), [])
        running_mid = ok[:3] + [dict(ok[3], state=lock.RUNNING, pid=PID_B)] + ok[4:]
        self.assertEqual(lock.stopped_timeline_problems(running_mid), ["service_not_steady_stopped"])
        self.assertEqual(lock.stopped_timeline_problems([e for e in ok if e["phase"] != "warning"]), ["timeline_phases_missing"])
        self.assertIn("service_not_steady_stopped", lock.stopped_timeline_problems(ok[:-1] + [dict(ok[-1], pid=PID_A)]))

    def test_case_events_and_order(self):
        spec = self.SPEC
        self.assertTrue(spec.restart_fails and spec.lock_server and not spec.acl_step)
        self.assertFalse(lock.LOCK_SPEC.restart_fails or ACL.restart_fails)
        events = list(spec.events_at_dialog)
        with_queries = events[:1] + ["sc-query"] + events[1:]
        self.assertEqual(lock.event_problems(with_queries, final=False, spec=spec), [])
        self.assertEqual(lock.event_problems(events + ["callback-before flag=0", "callback-after flag=0"], final=True, spec=spec), [])
        swapped = events[:4] + [events[5], events[4]]
        self.assertTrue(lock.event_problems(swapped, final=False, spec=spec))
        self.assertTrue(lock.event_problems(events[:4] + events[5:], final=False, spec=spec))

    def test_warning_events_stop_before_main_dialog(self):
        events = list(self.SPEC.events_at_dialog[:-1])
        self.assertEqual(lock.warning_event_problems(events[:2] + ["sc-query"] + events[2:], self.SPEC), [])
        self.assertTrue(lock.warning_event_problems(events + ["main-dialog flag=0"], self.SPEC))
        self.assertTrue(lock.warning_event_problems(events[:-1], self.SPEC))


class StopTimeout(unittest.TestCase):
    SPEC = lock.CASES["C-stop-timeout"]
    DIALOG_TEXT = (lock.STOP_PENDING_DIALOG_PREFIX + "\r\n\r\n実行ファイルを入れ替えられないため、インストールを中止しました。"
                   "ファイルはまだ変更していません。sc query ChronoGazer で STOPPED になってから開始してください。")
    QUERIES = ["sc-query"] * lock.STOP_WAIT_QUERIES

    def wait_events(self, between=lock.STOP_WAIT_QUERIES, before=1):
        return (["section-begins"] + ["sc-query"] * before + ["stop-attempt"] + ["sc-query"] * between
                + ["main-dialog flag=0"])

    def test_dialog_matches(self):
        self.assertTrue(lock.stop_pending_dialog_matches(self.DIALOG_TEXT, "C:\\i"))
        self.assertFalse(lock.stop_pending_dialog_matches(self.DIALOG_TEXT.replace("ファイルはまだ変更していません", ""), "C:\\i"))
        self.assertFalse(lock.stop_pending_dialog_matches(self.DIALOG_TEXT.replace("インストールを中止しました", "中止"), "C:\\i"))
        self.assertFalse(lock.stop_pending_dialog_matches(self.DIALOG_TEXT.replace("さらに 60 秒", "さらに 30 秒"), "C:\\i"))
        self.assertFalse(lock.stop_pending_dialog_matches(MAIN_TEXT, "C:\\i"))
        self.assertFalse(lock.stop_pending_dialog_matches(lock.RESTART_WARNING_PREFIX, "C:\\i"))

    def test_text_and_waits_match_the_unchanged_hooks(self):
        hooks = (ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh").read_text(encoding="utf-8")
        self.assertIn(lock.STOP_PENDING_DIALOG_PREFIX.replace("60 秒", "${CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒"), hooks)
        self.assertIn(lock.STOP_PENDING_MAIN_TEXT, hooks)
        self.assertIn("!define CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS 60\n", hooks)
        common = (ROOT / "installer-common/service-common.nsh").read_text(encoding="utf-8")
        self.assertIn("${If} $StopWaitCounter >= 30", common)
        self.assertEqual(lock.STOP_WAIT_QUERIES, 30 + 2 + 60)

    def test_dialog_kind(self):
        line = ('MessageBox MB_OK|MB_ICONSTOP "ChronoGazer サービスの停止がまだ終わっていません（停止を頼んで、さらに '
                '${CHRONOGAZER_SERVICE_STOP_EXTRA_WAIT_SECONDS} 秒待ちましたが、PENDING のままです）。" /SD IDOK')
        self.assertEqual(lock.dialog_kind(line, lock.STOP_PENDING_MAIN_TEXT), "main-dialog")
        self.assertEqual(lock.dialog_kind(line), "inactive-dialog")

    def test_case_flags(self):
        spec = self.SPEC
        self.assertTrue(spec.stop_held and not spec.lock_server and not spec.acl_step and not spec.restart_fails)
        self.assertEqual(spec.dialog_wait, 180)
        self.assertEqual(spec.events_at_dialog, lock.STOP_HELD_EVENTS_AT_DIALOG)

    def test_event_sequences(self):
        spec = self.SPEC
        events = list(spec.events_at_dialog)
        queried = events[:1] + ["sc-query"] + events[1:2] + ["sc-query"] * 92 + events[2:]
        self.assertEqual(lock.event_problems(queried, final=False, spec=spec), [])
        self.assertEqual(lock.event_problems(events + ["callback-before flag=0", "callback-after flag=0"], final=True, spec=spec), [])
        for extra in ("restart-attempt", "copy-begins", "preinstall-done flag=1"):
            self.assertTrue(lock.event_problems(events[:2] + [extra] + events[2:], final=False, spec=spec), extra)

    def test_stop_wait_problems(self):
        self.assertEqual(lock.stop_wait_problems(self.wait_events()), [])
        self.assertEqual(lock.stop_wait_problems(self.wait_events(between=91)), ["stop_wait_queries_91"])
        self.assertEqual(lock.stop_wait_problems(self.wait_events(between=93)), ["stop_wait_queries_93"])
        self.assertEqual(lock.stop_wait_problems(self.wait_events(before=0)), ["stop_wait_queries_before_stop_0"])
        self.assertEqual(lock.stop_wait_problems(["section-begins", "sc-query", "stop-attempt"]), ["stop_wait_marker_missing"])
        self.assertEqual(lock.stop_wait_problems(["main-dialog flag=0"]), ["stop_wait_marker_missing"])

    def test_stop_hold_elapsed_problems(self):
        self.assertEqual(lock.stop_hold_elapsed_problems(1000, 1000 + lock.STOP_HOLD_MIN_MS), [])
        self.assertEqual(lock.stop_hold_elapsed_problems(1000, 1000 + lock.STOP_HOLD_MIN_MS - 1), ["stop_hold_too_short"])
        self.assertEqual(lock.stop_hold_elapsed_problems(None, 100000), ["stop_hold_wall_ms_missing"])

    def test_hold_problems(self):
        begin = {"event": "hold-begin", "pid": PID_A, "wall_ms": 1}
        end = {"event": "hold-end", "pid": PID_A, "reason": "released", "wall_ms": 2}
        self.assertEqual(lock.hold_problems([begin], PID_A, False), [])
        self.assertEqual(lock.hold_problems([begin, end], PID_A, True), [])
        self.assertEqual(lock.hold_problems([begin, dict(end, reason="expired")], PID_A, True), ["hold1_mismatch"])
        self.assertEqual(lock.hold_problems([begin, dict(end, pid=PID_B)], PID_A, True), ["hold1_mismatch"])
        self.assertEqual(lock.hold_problems([dict(begin, pid=PID_B)], PID_A, False), ["hold0_mismatch"])
        self.assertEqual(lock.hold_problems([begin, end], PID_A, False), ["holds_count_2"])
        self.assertEqual(lock.hold_problems([begin], PID_A, True), ["holds_count_1"])
        self.assertEqual(lock.hold_problems([], PID_A, False), ["holds_count_0"])
        self.assertEqual(lock.hold_problems([end, begin], PID_A, True), ["hold0_mismatch", "hold1_mismatch"])

    def test_timeline_state(self):
        rows = (("before-dialog", lock.RUNNING, PID_A), ("dialog", lock.STOP_PENDING, PID_A),
                ("after-ok", lock.STOP_PENDING, PID_A), ("after-exit", lock.STOP_PENDING, PID_A))
        good = [{"phase": phase, "state": state, "pid": pid} for phase, state, pid in rows]
        self.assertEqual(lock.timeline_problems(good, PID_A, lock.STOP_PENDING), [])
        running = good[:2] + [dict(good[2], state=lock.RUNNING)] + good[3:]
        self.assertEqual(lock.timeline_problems(running, PID_A, lock.STOP_PENDING), ["service_not_steady_running_pid_b"])
        # 既定は従来どおり RUNNING を要求する。
        self.assertEqual(lock.timeline_problems(good, PID_A), ["service_not_steady_running_pid_b"])
        steady = [dict(row, state=lock.RUNNING) for row in good]
        self.assertEqual(lock.timeline_problems(steady, PID_A), [])


class CopyFail(unittest.TestCase):
    SPEC = lock.CASES["C-copy-fail"]
    INSTALL = r"C:\a\_temp\Banto973-x\C-copy-fail\install"
    FILE_ERROR = ("Error opening file for writing: \r\n\r\n" + INSTALL + "\\chronogazer-server.exe\r\n\r\n"
                  "Click Retry to try again, or\r\nCancel to stop the installation.")
    CAPTION = "Banto973 lock-abort 12345678"

    def test_file_error_matches(self):
        self.assertTrue(lock.file_error_matches(self.FILE_ERROR, self.INSTALL))
        self.assertFalse(lock.file_error_matches(self.FILE_ERROR.replace("server", "elev"), self.INSTALL))
        self.assertFalse(lock.file_error_matches(self.FILE_ERROR, r"C:\other\install"))
        self.assertFalse(lock.file_error_matches(self.FILE_ERROR.replace("Click Retry to try again, or", "Click Abort to stop the installation, or"), self.INSTALL))
        self.assertFalse(lock.file_error_matches(self.FILE_ERROR.replace("Error opening file for writing:", "Error opening file for reading:"), self.INSTALL))

    def test_callback_text_matches_the_unchanged_hooks(self):
        self.assertTrue(lock.callback_restarted_matches(lock.CALLBACK_RESTARTED_TEXT))
        self.assertFalse(lock.callback_restarted_matches(lock.CALLBACK_RESTARTED_TEXT + " "))
        self.assertFalse(lock.callback_restarted_matches(MAIN_TEXT))
        hooks = (ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh").read_text(encoding="utf-8")
        self.assertIn(lock.CALLBACK_RESTARTED_TEXT, hooks)

    def test_template_matches_production_shape(self):
        template = lock.TEMPLATE
        self.assertEqual(template.count("AllowSkipFiles off\n"), 1)
        self.assertLess(template.index("AllowSkipFiles off"), template.index("Section\n"))
        self.assertGreater(template.index("RequireChronoGazerFilesCopied"), template.rindex("File \""))
        main_rs = (ROOT / "apps/chronogazer/installer/src/main.rs").read_text(encoding="utf-8")
        self.assertIn("AllowSkipFiles off", main_rs)

    def test_retry_cancel_button(self):
        def btn(hwnd, visible=True):
            return {"hwnd": hwnd, "class": "Button", "title": "x", "visible": visible}
        ids = {10: lock.IDRETRY, 11: lock.IDCANCEL, 12: 5}
        self.assertEqual(lock.retry_cancel_button([btn(10), btn(11)], ids.get)["hwnd"], 11)
        with self.assertRaises(RuntimeError):
            lock.retry_cancel_button([btn(10), btn(11), btn(12)], ids.get)
        with self.assertRaises(RuntimeError):
            lock.retry_cancel_button([btn(12)], ids.get)
        with self.assertRaises(RuntimeError):
            lock.retry_cancel_button([btn(10), btn(11, visible=False)], ids.get)
        self.assertEqual(lock.retry_cancel_button([btn(10), btn(11), btn(12, visible=False)], ids.get)["hwnd"], 11)

    def test_callback_dialogs(self):
        text = [{"class": "Static", "title": lock.CALLBACK_RESTARTED_TEXT}]
        for owner in (0, 5):
            self.assertEqual(lock.callback_dialogs([window(20, owner)], lambda h: text, 7, self.CAPTION, 5)[0]["hwnd"], 20)
        self.assertEqual(lock.callback_dialogs([window(5, 0)], lambda h: text, 7, self.CAPTION, 5), (None, []))
        other = window(21, 5)
        self.assertEqual(lock.callback_dialogs([other], lambda h: [{"class": "Static", "title": MAIN_TEXT}], 7, self.CAPTION, 5), (None, [other]))
        self.assertEqual(lock.callback_dialogs([other], lambda h: [{"class": "Static", "title": ""}], 7, self.CAPTION, 5), (None, []))
        self.assertEqual(lock.callback_dialogs([window(22, 5, pid=8)], lambda h: text, 7, self.CAPTION, 5), (None, []))
        with self.assertRaises(RuntimeError):
            lock.callback_dialogs([window(20, 5), window(23, 0)], lambda h: text, 7, self.CAPTION, 5)

    def test_partial_hashes_and_flags(self):
        before = {"chronogazer.exe": "a", "chronogazer-server.exe": "b", "data.txt": "c"}
        self.assertEqual(lock.partial_hashes(before, "n"), {"chronogazer.exe": "n", "chronogazer-server.exe": "b", "data.txt": "c"})
        self.assertEqual(before["chronogazer.exe"], "a")
        spec = self.SPEC
        self.assertTrue(spec.copy_fails)
        self.assertFalse(spec.lock_server or spec.acl_step or spec.restart_fails or spec.stop_held)
        self.assertEqual(spec.events_at_dialog, lock.COPY_FAIL_EVENTS_AT_DIALOG)
        self.assertEqual(spec.events_final, lock.COPY_FAIL_EVENTS_FINAL)

    def test_event_problems(self):
        spec = self.SPEC
        at = list(lock.COPY_FAIL_EVENTS_AT_DIALOG)
        final = list(lock.COPY_FAIL_EVENTS_FINAL)
        self.assertEqual(lock.event_problems(at[:1] + ["sc-query"] + at[1:], final=False, spec=spec), [])
        self.assertEqual(lock.event_problems(final, final=True, spec=spec), [])
        self.assertTrue(lock.event_problems([at[1], at[0]] + at[2:], final=False, spec=spec))
        self.assertTrue(lock.event_problems(final[:5] + ["restart-attempt"] + final[5:], final=True, spec=spec))
        early = at[:3] + ["restart-attempt", "copy-begins"]
        self.assertTrue(lock.event_problems(early, final=False, spec=spec))
        self.assertTrue(lock.event_problems(at[:3], final=False, spec=spec))
        self.assertTrue(lock.event_problems(at + ["other-dialog flag=1"], final=False, spec=spec))
        self.assertTrue(lock.event_problems(final[:4] + ["other-dialog flag=1"] + final[4:], final=True, spec=spec))

    def test_callback_event_problems(self):
        final = list(lock.COPY_FAIL_EVENTS_FINAL)
        self.assertEqual(lock.callback_event_problems(final[:-1]), [])
        self.assertEqual(lock.callback_event_problems(final[:2] + ["sc-query"] + final[2:-1]), [])
        self.assertTrue(lock.callback_event_problems(final))
        self.assertTrue(lock.callback_event_problems(final[:-2]))

    def test_timeline_problems(self):
        rows = (("before-dialog", lock.RUNNING, PID_A), ("file-error", lock.STOPPED, 0), ("after-cancel", lock.STOPPED, 0),
                ("failed-page", lock.STOPPED, 0), ("closing", lock.STOPPED, 0), ("callback-dialog", lock.RUNNING, PID_B),
                ("closing", lock.RUNNING, PID_B), ("after-exit", lock.RUNNING, PID_B))
        good = [{"phase": phase, "state": state, "pid": pid} for phase, state, pid in rows]
        self.assertEqual(lock.copy_fail_timeline_problems(good, PID_B), [])
        missing = [row for row in good if row["phase"] != "after-cancel"]
        self.assertEqual(lock.copy_fail_timeline_problems(missing, PID_B), ["timeline_phase_missing_after-cancel"])
        early = [dict(row, state=lock.RUNNING, pid=PID_B) if row["phase"] == "failed-page" else row for row in good]
        self.assertEqual(lock.copy_fail_timeline_problems(early, PID_B), ["timeline_phase_not_steady_failed-page"])
        other_pid = [dict(row, pid=PID_A) if row["phase"] == "after-exit" else row for row in good]
        self.assertEqual(lock.copy_fail_timeline_problems(other_pid, PID_B), ["timeline_phase_not_steady_after-exit"])


if __name__ == "__main__":
    unittest.main()
