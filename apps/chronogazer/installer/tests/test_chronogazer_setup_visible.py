"""chronogazer_setup_visible.py の純粋部分のテスト（Win32 は一切触らない）。

実行: python -B -m unittest discover -s apps/chronogazer/installer/tests -p "test_chronogazer_setup_visible.py"
"""

import sys
import time
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import chronogazer_setup_visible as v  # noqa: E402

PID = 4242


def child(cls, title="", visible=True, hwnd=1):
    return {"class": cls, "title": title, "visible": visible, "hwnd": hwnd}


def window(hwnd, owner=1, cls=v.DIALOG_CLASS, pid=PID, visible=True):
    return {"hwnd": hwnd, "pid": pid, "class": cls, "visible": visible, "owner": owner, "title": "t"}


QUESTION = v.QUESTION_PREFIX + "トレイの「終了」で閉じてください。"


class ImportSafetyTests(unittest.TestCase):
    def test_import_has_no_win32_side_effects(self):
        self.assertNotIn("windows_gui_capability", vars(v))
        self.assertNotIn("ctypes", vars(v))


class QuestionTests(unittest.TestCase):
    def test_question_matches(self):
        self.assertTrue(v.question_matches(QUESTION))
        self.assertFalse(v.question_matches("旧い版が見つかりました。"))
        self.assertFalse(v.question_matches(" " + QUESTION))


RECHECK = v.QUESTION_PREFIX + "まだファイルは変更していません。上書き前に止めたサービスは再開します。"
EARLIER = v.QUESTION_PREFIX + "まだ何も変更していません。"


class RecheckMatchTests(unittest.TestCase):
    def test_recheck_matches(self):
        self.assertTrue(v.recheck_question_matches(RECHECK))
        self.assertFalse(v.recheck_question_matches(EARLIER))
        self.assertFalse(v.recheck_question_matches("別の窓 " + RECHECK))
        self.assertFalse(v.recheck_question_matches(v.RECHECK_NOTE))

    def test_earlier_question_is_unexpected_in_recheck(self):
        found, unexpected = v.judge_dialogs(
            [window(2)], lambda h: [child("Static", EARLIER)], PID, matches=v.recheck_question_matches
        )
        self.assertIsNone(found)
        self.assertEqual(unexpected[0]["texts"], [EARLIER])
        found, _ = v.judge_dialogs(
            [window(2)], lambda h: [child("Static", RECHECK)], PID, matches=v.recheck_question_matches
        )
        self.assertEqual(found[0]["hwnd"], 2)


UNINSTALL_RECHECK = v.QUESTION_PREFIX + "\r\n\r\nタスクトレイの ChronoGazer から「終了」を選び（後始末の知らせの窓が出ていれば OK を押し）、終了してから [再試行] を押してください（もう 90 秒待ちます）。\r\n[キャンセル] でアンインストールを中止します（強制終了はしません。止めたサービスは再開します。まだ何も変更していません）。"
UNINSTALL_FIRST = v.QUESTION_PREFIX + "\r\n\r\nタスクトレイの ChronoGazer から「終了」を選び（後始末の知らせの窓が出ていれば OK を押し）、終了してから [再試行] を押してください（もう 90 秒待ちます）。\r\n[キャンセル] でアンインストールを中止します（強制終了はしません。まだ何も変更していません）。"


class UninstallRecheckMatchTests(unittest.TestCase):
    def test_matches_table(self):
        self.assertTrue(v.uninstall_recheck_question_matches(UNINSTALL_RECHECK))
        self.assertFalse(v.uninstall_recheck_question_matches(RECHECK))
        self.assertFalse(v.uninstall_recheck_question_matches(UNINSTALL_FIRST))
        self.assertFalse(v.uninstall_recheck_question_matches(v.UNINSTALL_RECHECK_NOTE))

    def test_first_question_is_unexpected(self):
        found, unexpected = v.judge_dialogs(
            [window(2)], lambda h: [child("Static", UNINSTALL_FIRST)], PID, matches=v.uninstall_recheck_question_matches
        )
        self.assertIsNone(found)
        self.assertEqual(unexpected[0]["texts"], [UNINSTALL_FIRST])

    def test_drive_recheck_cancel_passes_matches(self):
        with mock.patch.object(v, "_Driver") as drv:
            drv.return_value.await_question.return_value = ("dlg", [], 1.0)
            v.drive_recheck_cancel(None, 0.0, None, None, None, None, matches=v.uninstall_recheck_question_matches)
            drv.return_value.await_question.assert_called_once_with(0.0, matches=v.uninstall_recheck_question_matches)
            drv.reset_mock()
            v.drive_recheck_cancel(None, 0.0, None, None, None, None)
            drv.return_value.await_question.assert_called_once_with(0.0, matches=v.recheck_question_matches)
            self.assertEqual(drv.return_value.check_question.call_args.args[0], "recheck")
            drv.reset_mock()
            v.drive_recheck_cancel(None, 0.0, None, None, None, None, label="uninstall_recheck")
            self.assertEqual(drv.return_value.check_question.call_args.args[0], "uninstall_recheck")


class DriveRetryContinueTests(unittest.TestCase):
    def test_default_and_custom_matches_and_order(self):
        with mock.patch.object(v, "_Driver") as drv:
            d = drv.return_value
            d.await_question.return_value = ("dlg", [], 1.0)
            d.check_question.return_value = {"retry": 7}
            calls = mock.Mock()
            d.check_question.side_effect = lambda *a, **k: (calls.check(), {"retry": 7})[1]
            d.press.side_effect = lambda *a, **k: calls.press(a)
            v.drive_retry_continue(None, 0.0, None, None, None, calls.before)
            d.await_question.assert_called_once_with(0.0, matches=v.question_matches)
            self.assertEqual(d.check_question.call_args.args[0], "first")
            self.assertEqual([c[0] for c in calls.mock_calls[:3]], ["check", "before", "press"])
            self.assertEqual(d.press.call_args.args[1:], (7, v.IDRETRY, "retry"))
            d.finish_after_retry.assert_called_once_with()
            drv.reset_mock()
            d.await_question.return_value = ("dlg", [], 1.0)
            v.drive_retry_continue(None, 0.0, None, None, None, lambda: None, matches=v.recheck_question_matches, label="recheck")
            d.await_question.assert_called_once_with(0.0, matches=v.recheck_question_matches)
            self.assertEqual(d.check_question.call_args.args[0], "recheck")
            drv.reset_mock()
            d.await_question.return_value = ("dlg", [], 1.0)
            v.drive_retry_continue(
                None, 0.0, None, None, None, lambda: None, matches=v.uninstall_recheck_question_matches, label="uninstall_recheck_retry"
            )
            d.await_question.assert_called_once_with(0.0, matches=v.uninstall_recheck_question_matches)
            self.assertEqual(d.check_question.call_args.args[0], "uninstall_recheck_retry")


class FakeChild:
    """poll() は exit_at 秒後に 0 を返す。forced_rc を入れるとその値で終わる。"""

    def __init__(self, exit_at=None):
        self.t0 = time.monotonic()
        self.exit_at = exit_at
        self.pid = PID
        self.forced_rc = None

    def poll(self):
        if self.forced_rc is not None:
            return self.forced_rc
        if self.exit_at is not None and time.monotonic() - self.t0 >= self.exit_at:
            return 0
        return None


class FakeUser32:
    """PostMessageW の戻りと IsWindow を差し込める。close_ends は N 回目の WM_CLOSE で child を終わらせる。"""

    def __init__(self, child, post_results=(), alive=True, close_ends=None):
        self.child, self.results, self.alive, self.close_ends = child, list(post_results), alive, close_ends
        self.posts = []

    def PostMessageW(self, hwnd, msg, w, l):
        self.posts.append(msg)
        ok = self.results.pop(0) if self.results else True
        if ok and self.close_ends is not None and len(self.posts) >= self.close_ends:
            self.child.forced_rc = 1
        return ok

    def IsWindow(self, hwnd):
        return self.alive


def make_driver(child_, user32, windows=lambda: [window(1, owner=0)], dialog_text=None):
    d = v._Driver.__new__(v._Driver)
    d.child = child_
    d.pid = PID
    d.recorded = {}
    d.record = lambda k, val: d.recorded.__setitem__(k, val)
    d.api = mock.Mock()
    d.api.u = user32
    d.snapshot = lambda: windows()
    d.children_of = lambda h: [child("Static", dialog_text)] if dialog_text else []
    return d


FAST = dict(
    POLL_SECONDS=0.01,
    QUIET_AFTER_CANCEL_SECONDS=0.0,
    CLOSE_RESEND_SECONDS=0.05,
    SERVICE_RESUME_LIMIT_SECONDS=0.3,
    FINISH_LIMIT_SECONDS=0.5,
    INSTALL_FINISH_LIMIT_SECONDS=0.2,
)


class FinishTests(unittest.TestCase):
    def setUp(self):
        for k, val in FAST.items():
            m = mock.patch.object(v, k, val)
            m.start()
            self.addCleanup(m.stop)

    def test_no_close_while_not_ready_then_resends_until_exit(self):
        ch = FakeChild()
        u = FakeUser32(ch, close_ends=3)
        d = make_driver(ch, u)
        calls = {"n": 0}

        def ready_to_close():
            calls["n"] += 1
            self.assertEqual(u.posts, [])  # 真になるまでは WM_CLOSE を送らない
            return calls["n"] > 5

        d.finish_after_cancel(ready_to_close=ready_to_close)
        self.assertEqual(len(u.posts), 3)  # 真になった後、終わるまで送り直す
        self.assertEqual(d.recorded["main_window_close_posts"], 3)
        self.assertTrue(d.recorded["main_window_closed"])

    def test_never_ready_fails_without_posting(self):
        ch = FakeChild()
        u = FakeUser32(ch)
        d = make_driver(ch, u)
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel(ready_to_close=lambda: False)
        self.assertEqual(cm.exception.reason, "service_not_resumed_before_main_close")
        self.assertEqual(u.posts, [])

    def test_setup_exits_while_waiting(self):
        ch = FakeChild(exit_at=0.05)
        d = make_driver(ch, FakeUser32(ch))
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel(ready_to_close=lambda: False)
        self.assertEqual(cm.exception.reason, "setup_exited_before_service_resumed")
        self.assertEqual(cm.exception.observation, {"exit": 0})

    def test_post_failure_after_window_gone_is_ignored(self):
        ch = FakeChild(exit_at=0.15)
        u = FakeUser32(ch, post_results=[True] + [False] * 50, alive=False)
        d = make_driver(ch, u)
        d.finish_after_cancel(ready_to_close=lambda: True)
        self.assertEqual(d.recorded["main_window_close_posts"], 1)

    def test_post_failure_with_live_window_fails(self):
        ch = FakeChild()
        u = FakeUser32(ch, post_results=[False], alive=True)
        d = make_driver(ch, u)
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel()
        self.assertEqual(cm.exception.reason, "PostMessage_WM_CLOSE_failed")

    def test_owned_dialog_while_waiting_fails(self):
        ch = FakeChild()
        d = make_driver(ch, FakeUser32(ch), windows=lambda: [window(1, owner=0), window(2)], dialog_text="別の窓")
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel(ready_to_close=lambda: False)
        self.assertEqual(cm.exception.reason, "unexpected_dialog_before_main_close")

    def test_without_ready_resends_until_exit(self):
        ch = FakeChild()
        u = FakeUser32(ch, close_ends=2)
        make_driver(ch, u).finish_after_cancel()
        self.assertEqual(len(u.posts), 2)

    def test_other_owner_zero_window_after_main_gone_fails(self):
        ch = FakeChild()
        u = FakeUser32(ch)
        t0 = time.monotonic()
        # 主窓(1)が消えた後に別の hwnd(9) の owner 0 窓が出る
        d = make_driver(ch, u, windows=lambda: [window(1, owner=0)] if time.monotonic() - t0 < 0.1 else [window(9, owner=0)])
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel(ready_to_close=lambda: True)
        self.assertEqual(cm.exception.reason, "unexpected_owner_zero_window")
        self.assertGreaterEqual(len(u.posts), 1)

    def test_two_owner_zero_windows_fail(self):
        ch = FakeChild()
        u = FakeUser32(ch)
        d = make_driver(ch, u, windows=lambda: [window(1, owner=0), window(9, owner=0)])
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel(ready_to_close=lambda: True)
        self.assertEqual(cm.exception.reason, "unexpected_owner_zero_window")
        self.assertEqual(u.posts, [])

    def test_no_main_window_seen_with_ready_fails(self):
        ch = FakeChild(exit_at=0.05)
        u = FakeUser32(ch)
        d = make_driver(ch, u, windows=lambda: [])
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_cancel(ready_to_close=lambda: True)
        self.assertEqual(cm.exception.reason, "main_window_not_closed_after_resume")
        self.assertEqual(u.posts, [])

    def test_no_main_window_without_ready_is_not_judged(self):
        ch = FakeChild(exit_at=0.05)
        d = make_driver(ch, FakeUser32(ch), windows=lambda: [])
        d.finish_after_cancel()
        self.assertEqual(d.recorded["main_window_close_posts"], 0)

    def test_retry_finish_ok_and_timeout(self):
        ch = FakeChild(exit_at=0.05)
        make_driver(ch, FakeUser32(ch)).finish_after_retry()
        ch = FakeChild()
        with self.assertRaises(v.VisibleFailure) as cm:
            make_driver(ch, FakeUser32(ch)).finish_after_retry()
        self.assertEqual(cm.exception.reason, "setup_did_not_finish_after_retry")

    def test_retry_finish_owned_dialog_fails(self):
        ch = FakeChild()
        d = make_driver(ch, FakeUser32(ch), windows=lambda: [window(2)], dialog_text="別の窓")
        with self.assertRaises(v.VisibleFailure) as cm:
            d.finish_after_retry()
        self.assertEqual(cm.exception.reason, "unexpected_dialog_after_retry")


class ElapsedTests(unittest.TestCase):
    def test_in_range(self):
        for e in (89.0, 90.5, 200.0):
            self.assertEqual(v.elapsed_problems(e), [], e)

    def test_out_of_range(self):
        self.assertEqual(v.elapsed_problems(88.9), ["elapsed_below_89s"])
        self.assertEqual(v.elapsed_problems(200.1), ["elapsed_above_200s"])


class ButtonTests(unittest.TestCase):
    ids = {10: v.IDRETRY, 11: v.IDCANCEL, 12: 7}

    def test_exact_pair(self):
        kids = [child("Static", QUESTION, hwnd=9), child("Button", "x", hwnd=10), child("Button", "y", hwnd=11)]
        got = v.retry_cancel_buttons(kids, self.ids.get)
        self.assertEqual((got["retry"]["hwnd"], got["cancel"]["hwnd"]), (10, 11))

    def test_hidden_buttons_are_ignored(self):
        kids = [child("Button", hwnd=10), child("Button", hwnd=11), child("Button", visible=False, hwnd=12)]
        self.assertEqual(v.retry_cancel_buttons(kids, self.ids.get)["cancel"]["hwnd"], 11)

    def test_wrong_ids_or_count_fail(self):
        with self.assertRaises(v.VisibleFailure):
            v.retry_cancel_buttons([child("Button", hwnd=10)], self.ids.get)
        with self.assertRaises(v.VisibleFailure):
            v.retry_cancel_buttons([child("Button", hwnd=10), child("Button", hwnd=12)], self.ids.get)
        with self.assertRaises(v.VisibleFailure):
            v.retry_cancel_buttons([child("Button", hwnd=10), child("Button", hwnd=11), child("Button", hwnd=12)], self.ids.get)


LOCKED = v.LOCKED_EXE_NOTICE_PREFIX + "chronogazer-elev.exe）。上書きはまだ始めていません。"
RESUME_FAILED = v.RESUME_FAILED_PREFIX + "エラー）。"


class NoticeMatchTests(unittest.TestCase):
    def test_locked_exe_notice_matches(self):
        self.assertTrue(v.locked_exe_notice_matches(LOCKED))
        self.assertFalse(v.locked_exe_notice_matches(v.LOCKED_EXE_NOTICE_PREFIX + "chronogazer-elev.exe）。"))
        self.assertFalse(v.locked_exe_notice_matches(v.LOCKED_EXE_NOTICE_PREFIX + "other.exe）。上書きはまだ始めていません。"))
        self.assertFalse(
            v.locked_exe_notice_matches(
                v.LOCKED_EXE_NOTICE_PREFIX + "chronogazer-server.exe chronogazer-elev.exe）。上書きはまだ始めていません。"
            )
        )
        self.assertFalse(v.locked_exe_notice_matches("x" + LOCKED))
        self.assertFalse(v.locked_exe_notice_matches(RESUME_FAILED))

    def test_resume_failed_warning_matches(self):
        self.assertTrue(v.resume_failed_warning_matches(RESUME_FAILED))
        self.assertFalse(v.resume_failed_warning_matches(LOCKED))
        self.assertFalse(v.resume_failed_warning_matches("x" + RESUME_FAILED))


class OkButtonTests(unittest.TestCase):
    ids = {10: 1, 11: 2, 12: 7}

    def test_id_1_and_2_are_accepted(self):
        for hwnd, want in ((10, 1), (11, 2)):
            button, cid = v.ok_button([child("Static", LOCKED, hwnd=9), child("Button", hwnd=hwnd)], self.ids.get)
            self.assertEqual((button["hwnd"], cid), (hwnd, want))

    def test_other_id_fails(self):
        with self.assertRaises(v.VisibleFailure):
            v.ok_button([child("Button", hwnd=12)], self.ids.get)

    def test_count_must_be_one(self):
        with self.assertRaises(v.VisibleFailure):
            v.ok_button([child("Static", LOCKED, hwnd=9)], self.ids.get)
        with self.assertRaises(v.VisibleFailure):
            v.ok_button([child("Button", hwnd=10), child("Button", hwnd=11)], self.ids.get)
        self.assertEqual(v.ok_button([child("Button", hwnd=10), child("Button", visible=False, hwnd=11)], self.ids.get)[1], 1)


class ClassifyTests(unittest.TestCase):
    def test_waiting_until_static_text_is_readable(self):
        self.assertEqual(v.classify_dialog_children([]), "waiting")
        self.assertEqual(v.classify_dialog_children([child("Static", "")]), "waiting")
        self.assertEqual(v.classify_dialog_children([child("Button", "OK")]), "waiting")

    def test_question_and_other(self):
        self.assertEqual(v.classify_dialog_children([child("Static", QUESTION)]), "question")
        self.assertEqual(v.classify_dialog_children([child("Static", "別の窓")]), "other")


class JudgeTests(unittest.TestCase):
    def children_of(self, table):
        return lambda hwnd: table.get(hwnd, [])

    def test_finds_the_question_and_ignores_main_and_foreign(self):
        windows = [window(1, owner=0), window(2), window(3, pid=1), window(4, visible=False)]
        table = {2: [child("Static", QUESTION)], 3: [child("Static", "他")], 4: [child("Static", "他")]}
        found, unexpected = v.judge_dialogs(windows, self.children_of(table), PID)
        self.assertEqual(found[0]["hwnd"], 2)
        self.assertEqual(unexpected, [])

    def test_other_text_is_unexpected(self):
        windows = [window(2)]
        found, unexpected = v.judge_dialogs(windows, self.children_of({2: [child("Static", "旧い版")]}), PID)
        self.assertIsNone(found)
        self.assertEqual(unexpected[0]["texts"], ["旧い版"])

    def test_unreadable_children_are_waiting_not_unexpected(self):
        found, unexpected = v.judge_dialogs([window(2)], self.children_of({}), PID)
        self.assertIsNone(found)
        self.assertEqual(unexpected, [])

    def test_two_questions_are_ambiguous(self):
        table = {2: [child("Static", QUESTION)], 3: [child("Static", QUESTION)]}
        with self.assertRaises(v.VisibleFailure):
            v.judge_dialogs([window(2), window(3)], self.children_of(table), PID)

    def test_owned_and_main_windows(self):
        windows = [window(1, owner=0), window(2), window(3, cls="Button")]
        self.assertEqual([w["hwnd"] for w in v.owned_dialogs(windows, PID)], [2])
        self.assertEqual([w["hwnd"] for w in v.main_windows(windows, PID)], [1])


if __name__ == "__main__":
    unittest.main()
