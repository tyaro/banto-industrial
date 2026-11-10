"""chronogazer_setup_visible.py の純粋部分のテスト（Win32 は一切触らない）。

実行: python -B -m unittest discover -s apps/chronogazer/installer/tests -p "test_chronogazer_setup_visible.py"
"""

import sys
import unittest
from pathlib import Path

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
