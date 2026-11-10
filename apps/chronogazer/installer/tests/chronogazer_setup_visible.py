"""#973: 本物の ChronoGazer setup を /P（passive）で走らせ、シェルの終了待ち 90 秒の問いを実画面で確かめる。

chronogazer.exe という名前の終わらないプロセス（偽シェル）を置いておくと、setup は
`QuitShellForUpdate`（service-hooks.nsh）で最大 90 秒待った後、強制終了せずに
MB_RETRYCANCEL で問う。このモジュールが押すのは次の 3 通り:
- 最初の問い（Section WebView2 先頭の事前確認。サービスは止まっておらずファイルも変わらない）の
  [再試行] と [キャンセル]（`drive_shell_quit_cancel`）。
- 再確認の問い（サービス停止の後・上書きの前、restart_service_on_cancel=1。本文に
  `上書き前に止めたサービスは再開します` を含む）の [キャンセル]（`drive_recheck_cancel`）。
  サービスが再開されたのを見てから主窓を閉じる。
- 最初の問いの [再試行]（偽シェルを止めてから）で上書きまで続行する道（`drive_retry_continue`）。
アンインストールの再確認・WebView2 の後始末は対象外。

純粋な判定関数（本文・経過秒・ボタン・dialog の分類）と、Win32 を叩く駆動を分ける。Windows 専用の
import は関数の中でだけ行い、Linux でも import できる（純テストが読む）。
実行は使い捨ての GitHub-hosted Windows だけ（呼び出し側 chronogazer_setup_acceptance.py の guard が守る）。
"""

from __future__ import annotations

import time
from pathlib import Path

DIALOG_CLASS = "#32770"  # MessageBox も NSIS の主窓も同じ class。
IDCANCEL, IDRETRY = 2, 4
WM_CLOSE, WM_COMMAND, BN_CLICKED, GW_OWNER = 0x0010, 0x0111, 0, 4
# service-hooks.nsh の QuitShellForUpdate の MessageBox の書き出し。
QUESTION_PREFIX = "ChronoGazer のデスクトップシェル (chronogazer.exe) がまだ終了していません。"
# 再確認（Section Install の上書き直前）の問いだけに入る一文。前の 2 回の問いは「まだ何も変更していません」。
RECHECK_NOTE = "上書き前に止めたサービスは再開します"
# 1 秒おきの poll で Sleep は 89 回なので、spawn からの経過は 89 秒以上。上限は遅い runner の余裕。
MIN_ELAPSED_SECONDS = 89.0
MAX_ELAPSED_SECONDS = 200.0
QUESTION_WAIT_LIMIT_SECONDS = 240.0
DIALOG_CLOSE_LIMIT_SECONDS = 10.0
QUIET_AFTER_CANCEL_SECONDS = 2.0
FINISH_LIMIT_SECONDS = 30.0
CLOSE_RESEND_SECONDS = 3.0
SERVICE_RESUME_LIMIT_SECONDS = 60.0
INSTALL_FINISH_LIMIT_SECONDS = 300.0
POLL_SECONDS = 0.25


class VisibleFailure(RuntimeError):
    def __init__(self, reason: str, observation=None):
        super().__init__(reason)
        self.reason = reason
        self.observation = observation


# ---------------------------------------------------------------- 純粋な判定

def question_matches(text: str) -> bool:
    return text.startswith(QUESTION_PREFIX)


def recheck_question_matches(text: str) -> bool:
    return question_matches(text) and RECHECK_NOTE in text


def elapsed_problems(elapsed: float) -> list[str]:
    """問いが出るまでの経過秒。下限より早ければ待っていない、上限より遅ければ別の理由で詰まっている。"""
    problems = []
    if elapsed < MIN_ELAPSED_SECONDS:
        problems.append(f"elapsed_below_{MIN_ELAPSED_SECONDS:g}s")
    if elapsed > MAX_ELAPSED_SECONDS:
        problems.append(f"elapsed_above_{MAX_ELAPSED_SECONDS:g}s")
    return problems


def static_texts(children) -> list[str]:
    return [c["title"] for c in children if c["class"].casefold() == "static" and c["title"]]


def classify_dialog_children(children, matches=question_matches) -> str:
    """子 Static が読めるまでは waiting（親 HWND が見えても label が揃うまで待つ。checklist §3）。"""
    texts = static_texts(children)
    if not texts:
        return "waiting"
    return "question" if any(matches(t) for t in texts) else "other"


def retry_cancel_buttons(children, control_id_of) -> dict:
    """visible な Button がちょうど 2 つで control id が {IDRETRY, IDCANCEL}（ロケールに依らない）。"""
    buttons = [c for c in children if c["class"].casefold() == "button" and c["visible"]]
    ids = {c["hwnd"]: int(control_id_of(c["hwnd"])) for c in buttons}
    if len(buttons) != 2 or set(ids.values()) != {IDCANCEL, IDRETRY}:
        raise VisibleFailure("retry_cancel_buttons_not_exact", {"ids": sorted(ids.values()), "count": len(buttons)})
    return {
        "retry": next(c for c in buttons if ids[c["hwnd"]] == IDRETRY),
        "cancel": next(c for c in buttons if ids[c["hwnd"]] == IDCANCEL),
    }


def owned_dialogs(windows, pid):
    return [w for w in windows if w["pid"] == pid and w["class"] == DIALOG_CLASS and w["visible"] and w.get("owner")]


def main_windows(windows, pid):
    return [w for w in windows if w["pid"] == pid and w["class"] == DIALOG_CLASS and w["visible"] and not w.get("owner")]


def judge_dialogs(windows, children_of, pid, matches=question_matches):
    """pid・#32770・visible・owner 非 0 の dialog を子 Static の本文で分ける。(問い or None, 想定外の一覧)。"""
    found, unexpected = [], []
    for window in owned_dialogs(windows, pid):
        children = children_of(window["hwnd"])
        kind = classify_dialog_children(children, matches=matches)
        if kind == "question":
            found.append((window, children))
        elif kind == "other":
            unexpected.append({"window": window, "texts": static_texts(children)})
    if len(found) > 1:
        raise VisibleFailure("ambiguous_question_dialog", [w for w, _ in found])
    return (found[0] if found else None), unexpected


# ---------------------------------------------------------------- Win32 側（使い捨て Windows 専用）

def gui_check():
    """(環境, 使えない理由の一覧)。"""
    import windows_gui_capability as gui

    environment = gui.API().environment()
    return environment, gui.capability_reasons(environment)


def _make_api():
    import ctypes
    import ctypes.wintypes as W

    import windows_gui_capability as gui

    api = gui.API()
    gui.bind(api.u, "GetWindow", [W.HWND, W.UINT], W.HWND)
    gui.bind(api.u, "IsWindow", [W.HWND], W.BOOL)
    gui.bind(api.u, "GetDlgCtrlID", [W.HWND], ctypes.c_int)
    return gui, api


def _with_owner(api, windows):
    for window in windows:
        window["owner"] = int(api.u.GetWindow(window["hwnd"], GW_OWNER) or 0)
    return windows


class _Driver:
    def __init__(self, child, proof_dir: Path, record, on_question):
        self.child = child
        self.proof_dir = Path(proof_dir)
        self.record = record
        self.on_question = on_question
        self.gui, self.api = _make_api()
        self.pid = child.pid

    def children_of(self, hwnd):
        return self.api.windows(self.pid, hwnd)

    def snapshot(self):
        return _with_owner(self.api, self.api.windows(self.pid))

    def await_question(self, since: float, matches=question_matches):
        deadline = time.monotonic() + QUESTION_WAIT_LIMIT_SECONDS
        while True:
            rc = self.child.poll()
            if rc is not None:
                raise VisibleFailure("setup_exited_before_question", {"exit": rc})
            windows = self.snapshot()
            found, unexpected = judge_dialogs(windows, self.children_of, self.pid, matches=matches)
            if unexpected:
                raise VisibleFailure("unexpected_dialog_before_question", unexpected)
            if found is not None:
                return found[0], found[1], time.monotonic() - since
            if time.monotonic() >= deadline:
                raise VisibleFailure("question_not_seen_within_limit", windows)
            time.sleep(POLL_SECONDS)

    def check_question(self, label: str, dialog, children, elapsed: float):
        texts = static_texts(children)
        buttons = retry_cancel_buttons(children, lambda h: self.api.u.GetDlgCtrlID(h))
        self.record(f"{label}_texts", texts)
        self.record(f"{label}_elapsed_seconds", round(elapsed, 1))
        self.record(f"{label}_buttons", {k: v["hwnd"] for k, v in buttons.items()})
        width, height, rgb = self.api.capture(dialog["rect"])
        (self.proof_dir / f"{label}_question.png").write_bytes(self.gui.png_bytes(width, height, rgb))
        problems = elapsed_problems(elapsed)
        if problems:
            raise VisibleFailure("elapsed_out_of_range:" + ",".join(problems), round(elapsed, 1))
        self.on_question(label, elapsed)
        return buttons

    def press(self, dialog, button, control_id: int, name: str):
        if not self.api.u.PostMessageW(dialog["hwnd"], WM_COMMAND, (BN_CLICKED << 16) | control_id, button["hwnd"]):
            raise VisibleFailure(f"PostMessage_BN_CLICKED_failed_{name}")
        deadline = time.monotonic() + DIALOG_CLOSE_LIMIT_SECONDS
        while self.api.u.IsWindow(dialog["hwnd"]):
            if time.monotonic() >= deadline:
                raise VisibleFailure(f"dialog_did_not_close_after_{name}")
            time.sleep(POLL_SECONDS)

    def _no_owned_dialog(self, when: str):
        windows = self.snapshot()
        owned = owned_dialogs(windows, self.pid)
        if owned:
            raise VisibleFailure(
                f"unexpected_dialog_{when}",
                [{"window": w, "texts": static_texts(self.children_of(w["hwnd"]))} for w in owned],
            )
        return windows

    def finish_after_cancel(self, ready_to_close=None):
        """キャンセル後: 失敗の頁（主窓）が残るので、他の dialog が出ないことを見てから閉じる。閉じた後も見張る。

        ready_to_close があれば、真になるまで（再確認の [キャンセル] でサービスを戻している間）
        WM_CLOSE を送らずに待つ。Section が走っている間の WM_CLOSE は無視されうるため、
        閉じる段階では setup が終わるまで CLOSE_RESEND_SECONDS おきに送り直す。
        """
        started = time.monotonic()
        quiet_until = started + QUIET_AFTER_CANCEL_SECONDS
        while time.monotonic() < quiet_until and self.child.poll() is None:
            self._no_owned_dialog("after_cancel")
            time.sleep(POLL_SECONDS)
        if ready_to_close is not None:
            wait_deadline = time.monotonic() + SERVICE_RESUME_LIMIT_SECONDS
            while True:
                rc = self.child.poll()
                if rc is not None:
                    raise VisibleFailure("setup_exited_before_service_resumed", {"exit": rc})
                self._no_owned_dialog("before_main_close")
                if ready_to_close():
                    break
                if time.monotonic() >= wait_deadline:
                    raise VisibleFailure("service_not_resumed_before_main_close")
                time.sleep(POLL_SECONDS)
            started = time.monotonic()
        deadline = started + FINISH_LIMIT_SECONDS
        posts = 0
        recorded = False
        last_post = None
        while self.child.poll() is None:
            if time.monotonic() >= deadline:
                break
            windows = self._no_owned_dialog("after_main_close" if posts else "on_failed_page")
            mains = main_windows(windows, self.pid)
            if mains:
                if not recorded:
                    self.record("main_window", mains)
                    recorded = True
                now = time.monotonic()
                if last_post is None or now - last_post >= CLOSE_RESEND_SECONDS:
                    last_post = now
                    if self.api.u.PostMessageW(mains[0]["hwnd"], WM_CLOSE, 0, 0):
                        posts += 1
                    elif self.api.u.IsWindow(mains[0]["hwnd"]) and self.child.poll() is None:
                        raise VisibleFailure("PostMessage_WM_CLOSE_failed")
            time.sleep(POLL_SECONDS)
        self.record("main_window_close_posts", posts)
        self.record("main_window_closed", posts > 0)

    def finish_after_retry(self):
        """[再試行] の後: 上書きが通って setup が自分で終わるまで、他の dialog が出ないことを見る。"""
        deadline = time.monotonic() + INSTALL_FINISH_LIMIT_SECONDS
        while self.child.poll() is None:
            if time.monotonic() >= deadline:
                raise VisibleFailure("setup_did_not_finish_after_retry")
            self._no_owned_dialog("after_retry")
            time.sleep(POLL_SECONDS)


def drive_shell_quit_cancel(child, started: float, proof_dir, record, on_question) -> None:
    """child は /P で起動済みの setup。1 回目の問いで [再試行]、2 回目の問いで [キャンセル] を押し、頁を閉じる。"""
    d = _Driver(child, proof_dir, record, on_question)
    dialog, children, elapsed = d.await_question(started)
    buttons = d.check_question("first", dialog, children, elapsed)
    pressed = time.monotonic()
    d.press(dialog, buttons["retry"], IDRETRY, "retry")
    dialog, children, elapsed = d.await_question(pressed)
    buttons = d.check_question("second", dialog, children, elapsed)
    d.press(dialog, buttons["cancel"], IDCANCEL, "cancel")
    d.finish_after_cancel()


def drive_recheck_cancel(child, fake_started: float, proof_dir, record, on_question, service_resumed) -> None:
    """child は /P で起動済みの setup。再確認の問いで [キャンセル] を押し、サービスの再開を見てから頁を閉じる。"""
    d = _Driver(child, proof_dir, record, on_question)
    dialog, children, elapsed = d.await_question(fake_started, matches=recheck_question_matches)
    buttons = d.check_question("recheck", dialog, children, elapsed)
    d.press(dialog, buttons["cancel"], IDCANCEL, "cancel")
    d.finish_after_cancel(ready_to_close=service_resumed)


def drive_retry_continue(child, started: float, proof_dir, record, on_question, before_retry) -> None:
    """child は /P で起動済みの setup。最初の問いで、偽シェルを止めてから [再試行] を押し、上書きの完了を待つ。"""
    d = _Driver(child, proof_dir, record, on_question)
    dialog, children, elapsed = d.await_question(started)
    buttons = d.check_question("first", dialog, children, elapsed)
    before_retry()
    d.press(dialog, buttons["retry"], IDRETRY, "retry")
    d.finish_after_retry()
