"""#973: 本物の ChronoGazer setup を /P（passive）で走らせ、シェルの終了待ち 90 秒の問いを実画面で確かめる。

chronogazer.exe という名前の終わらないプロセス（偽シェル）を置いておくと、setup は
`QuitShellForUpdate`（service-hooks.nsh）で最大 90 秒待った後、強制終了せずに
MB_RETRYCANCEL で問う。このモジュールが押すのは次の 6 通り:
- 最初の問い（Section WebView2 先頭の事前確認。サービスは止まっておらずファイルも変わらない）の
  [再試行] と [キャンセル]（`drive_shell_quit_cancel`）。
- 再確認の問い（サービス停止の後・上書きの前、restart_service_on_cancel=1。本文に
  `上書き前に止めたサービスは再開します` を含む）の [キャンセル]（`drive_recheck_cancel`）。
  サービスが再開されたのを見てから主窓を閉じる。
- アンインストールの再確認の問い（`uninstall.exe /P`、PREUNINSTALL でサービスを止めた後。本文に
  `止めたサービスは再開します。まだ何も変更していません` を含む）の [キャンセル]。同じ `drive_recheck_cancel` に
  `matches=uninstall_recheck_question_matches` を渡す。
- 最初の問いの [再試行]（偽シェルを止めてから）で上書きまで続行する道（`drive_retry_continue`）。
- 再確認の問いの [再試行]（偽シェルを止めてから）で上書きまで続行する道。`drive_retry_continue` に
  `matches=recheck_question_matches` を渡す。
- アンインストールの再確認の問いの [再試行]（偽シェルを止めてから）で最後まで消す道。同じ
  `drive_retry_continue` に `matches=uninstall_recheck_question_matches` を渡す。
上の問いとは別に、使用中 exe で上書き前に中止する道の MB_OK の案内も確かめる（`drive_notices`。再開失敗の
警告が主案内より先に出る道を含む。偽シェルは使わない）。ACL の段階（INSTDIR 直下のサブフォルダ）で中止する
道の案内も同じ `drive_notices` で確かめる（`acl_refused_notice_matches`）。サービスの停止が 30+60 秒待っても終わらない
道の案内も同じ（`stop_pending_notice_matches`）。
WebView2 の後始末は対象外。

純粋な判定関数（本文・経過秒・ボタン・dialog の分類）と、Win32 を叩く駆動を分ける。Windows 専用の
import は関数の中でだけ行い、Linux でも import できる（純テストが読む）。
実行は使い捨ての GitHub-hosted Windows だけ（呼び出し側 chronogazer_setup_acceptance.py の guard が守る）。
"""

from __future__ import annotations

import time
from pathlib import Path

DIALOG_CLASS = "#32770"  # MessageBox も NSIS の主窓も同じ class。
IDOK, IDCANCEL, IDRETRY = 1, 2, 4
WM_CLOSE, WM_COMMAND, BN_CLICKED, GW_OWNER = 0x0010, 0x0111, 0, 4
# service-hooks.nsh の QuitShellForUpdate の MessageBox の書き出し。
QUESTION_PREFIX = "ChronoGazer のデスクトップシェル (chronogazer.exe) がまだ終了していません。"
# 再確認（Section Install の上書き直前）の問いだけに入る一文。前の 2 回の問いは「まだ何も変更していません」。
RECHECK_NOTE = "上書き前に止めたサービスは再開します"
# アンインストールの再確認（Section Uninstall、PREUNINSTALL でサービスを止めた後）の問いだけに入る一文。
UNINSTALL_RECHECK_NOTE = "止めたサービスは再開します。まだ何も変更していません"
# RequireChronoGazerInstallExesUnlocked（service-hooks.nsh）の MessageBox。主案内の前に、止めたサービスの再開に
# 失敗した警告が出ることがある。
LOCKED_EXE_NOTICE_PREFIX = "使用中、または書き込めない実行ファイルがあります（"
LOCKED_EXE_NOTICE_EXE = "chronogazer-elev.exe"
LOCKED_EXE_NOTICE_NOTE = "上書きはまだ始めていません"
RESUME_FAILED_PREFIX = "インストールを中止しましたが、その前に止めていた ChronoGazer サービスを再開できませんでした（"
# install-dir-common.nsh の AbortInstallDirAcl の MessageBox。INSTDIR 直下のサブフォルダで ACL の段階を中止する
# ときの本文（理由の文は共通、止めたサービスの一文は service-hooks.nsh の INSTALL_DIR_COMMON_ACL_ABORT_NOTE）。
ACL_REFUSED_NOTICE_PREFIX = "インストール先 "
ACL_REFUSED_NOTICE_HEAD = "の所有者・アクセス権を既定に戻せませんでした。"
ACL_REFUSED_NOTICE_REASON = "インストール先の中にサブフォルダがあります。"
# 受入が INSTDIR 直下に作るサブフォルダ名（acceptance.py もこれを使う。本文の `対象:` の行に出る）。
ACL_REFUSED_SUBDIR = "banto-973-subdir"
ACL_REFUSED_NOTICE_NOTE = "ファイルはまだ変更していません。"
ACL_REFUSED_NOTICE_RESUME = "上書き前に停止したサービスがあれば再開を試みました。"
# RequireChronoGazerServiceInactive（service-hooks.nsh）の PENDING の MessageBox。`sc stop` の後 30 秒 + 追加 60 秒
# 待っても止まらないとき。止めたサービスを再開する案内ではなく、何も変更せず中止した旨だけを伝える。
STOP_PENDING_NOTICE_PREFIX = (
    "ChronoGazer サービスの停止がまだ終わっていません（停止を頼んで、さらに 60 秒待ちましたが、PENDING のままです）。"
)
STOP_PENDING_NOTICE_ABORTED = "インストールを中止しました"
STOP_PENDING_NOTICE_UNCHANGED = "ファイルはまだ変更していません"
# 案内の表示中に状態が変わらないことを見続ける秒数。
NOTICE_HOLD_SECONDS = 3.0
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


def uninstall_recheck_question_matches(text: str) -> bool:
    return question_matches(text) and UNINSTALL_RECHECK_NOTE in text


def locked_exe_notice_matches(text: str) -> bool:
    return text.startswith(LOCKED_EXE_NOTICE_PREFIX + LOCKED_EXE_NOTICE_EXE + "）") and LOCKED_EXE_NOTICE_NOTE in text


def resume_failed_warning_matches(text: str) -> bool:
    return text.startswith(RESUME_FAILED_PREFIX)


def acl_refused_notice_matches(text: str) -> bool:
    return (
        text.startswith(ACL_REFUSED_NOTICE_PREFIX)
        and ACL_REFUSED_NOTICE_HEAD in text
        and ACL_REFUSED_NOTICE_REASON in text
        and ACL_REFUSED_SUBDIR in text
        and ACL_REFUSED_NOTICE_NOTE in text
        and ACL_REFUSED_NOTICE_RESUME in text
    )


def stop_pending_notice_matches(text: str) -> bool:
    return (
        text.startswith(STOP_PENDING_NOTICE_PREFIX)
        and STOP_PENDING_NOTICE_ABORTED in text
        and STOP_PENDING_NOTICE_UNCHANGED in text
    )


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


def ok_button(children, control_id_of):
    """visible な Button がちょうど 1 つで、control id が {IDOK, IDCANCEL}（MB_OK の OK は実際は IDCANCEL。§3）。(button, control_id)。"""
    buttons = [c for c in children if c["class"].casefold() == "button" and c["visible"]]
    if len(buttons) != 1:
        raise VisibleFailure("ok_button_not_exact", {"count": len(buttons)})
    control_id = int(control_id_of(buttons[0]["hwnd"]))
    if control_id not in (IDOK, IDCANCEL):
        raise VisibleFailure("ok_button_unexpected_id", {"id": control_id})
    return buttons[0], control_id


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

    def check_notice(self, label: str, dialog, children, elapsed: float):
        texts = static_texts(children)
        button, control_id = ok_button(children, lambda h: self.api.u.GetDlgCtrlID(h))
        self.record(f"{label}_texts", texts)
        self.record(f"{label}_elapsed_seconds", round(elapsed, 1))
        self.record(f"{label}_ok_control_id", control_id)
        width, height, rgb = self.api.capture(dialog["rect"])
        (self.proof_dir / f"{label}_notice.png").write_bytes(self.gui.png_bytes(width, height, rgb))
        return button, control_id

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
        WM_CLOSE は最初に見た主窓の hwnd にだけ送る（後から出た owner 0 の別窓を黙って閉じて誤って通らないため）。
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
        main_hwnd = None
        while self.child.poll() is None:
            if time.monotonic() >= deadline:
                break
            windows = self._no_owned_dialog("after_main_close" if posts else "on_failed_page")
            mains = main_windows(windows, self.pid)
            if len(mains) > 1 or (
                main_hwnd is not None and any(m["hwnd"] != main_hwnd for m in mains)
            ):
                raise VisibleFailure("unexpected_owner_zero_window", mains)
            if mains:
                if main_hwnd is None:
                    main_hwnd = mains[0]["hwnd"]
                if not recorded:
                    self.record("main_window", mains)
                    recorded = True
                now = time.monotonic()
                if last_post is None or now - last_post >= CLOSE_RESEND_SECONDS:
                    last_post = now
                    if self.api.u.PostMessageW(main_hwnd, WM_CLOSE, 0, 0):
                        posts += 1
                    elif self.api.u.IsWindow(main_hwnd) and self.child.poll() is None:
                        raise VisibleFailure("PostMessage_WM_CLOSE_failed")
            time.sleep(POLL_SECONDS)
        self.record("main_window_close_posts", posts)
        self.record("main_window_closed", posts > 0)
        if ready_to_close is not None and posts == 0:
            raise VisibleFailure("main_window_not_closed_after_resume")

    def finish_after_retry(self):
        """[再試行] の後: 上書きまたはアンインストールが通って自分で終わるまで、他の dialog が出ないことを見る。"""
        deadline = time.monotonic() + INSTALL_FINISH_LIMIT_SECONDS
        while self.child.poll() is None:
            if time.monotonic() >= deadline:
                raise VisibleFailure("setup_did_not_finish_after_retry")
            self._no_owned_dialog("after_retry")
            time.sleep(POLL_SECONDS)


LANGUAGE_DIALOG_TITLE = "Installer Language"


def select_language_dialog(windows, pid):
    """pid の可視 #32770 のうち title が `Installer Language` の窓（NSIS の言語選択）。無ければ None。"""
    found = [
        w for w in windows
        if w["pid"] == pid and w["class"] == DIALOG_CLASS and w["visible"] and w["title"] == LANGUAGE_DIALOG_TITLE
    ]
    if len(found) > 1:
        raise VisibleFailure("ambiguous_language_dialog", found)
    return found[0] if found else None


def dismiss_language_dialog(child, proof_dir, record, limit: float = 60.0) -> bool:
    """uninstall.exe は保存された言語が無いと先に言語選択の窓を出す（MUI_UNGETLANGUAGE）。
    出たら既定の選択のまま OK を押して閉じて True、本来の案内（owned の #32770）が先に出たら触らず False。"""
    d = _Driver(child, proof_dir, record, lambda *_: None)
    deadline = time.monotonic() + limit
    while True:
        rc = child.poll()
        if rc is not None:
            raise VisibleFailure("setup_exited_before_language_dialog_or_notice", {"exit": rc})
        windows = d.snapshot()
        dialog = select_language_dialog(windows, d.pid)
        if dialog is not None:
            break
        if owned_dialogs(windows, d.pid):
            record("language_dialog", "not_shown")
            return False
        if time.monotonic() >= deadline:
            raise VisibleFailure("language_dialog_or_notice_not_seen_within_limit", windows)
        time.sleep(POLL_SECONDS)
    children = d.children_of(dialog["hwnd"])
    record("language_dialog_texts", {"static": static_texts(children), "all": [(c["class"], c["title"]) for c in children]})
    try:
        width, height, rgb = d.api.capture(dialog["rect"])
        (d.proof_dir / "language_dialog.png").write_bytes(d.gui.png_bytes(width, height, rgb))
    except Exception as e:  # 画像が取れなくても OK を押して進める（本文は上で記録済み）
        record("language_dialog_capture_error", repr(e))
    button = None
    for c in children:
        if c["class"].casefold() == "button" and c["visible"] and int(d.api.u.GetDlgCtrlID(c["hwnd"])) == IDOK:
            button = c
            break
    if button is None:
        raise VisibleFailure("language_dialog_ok_button_missing", children)
    d.press(dialog, button, IDOK, "language_ok")
    record("language_dialog", "accepted")
    return True


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


def drive_recheck_cancel(
    child, fake_started: float, proof_dir, record, on_question, service_resumed, matches=recheck_question_matches, label="recheck"
) -> None:
    """child は /P で起動済みの setup（アンインストールなら uninstall.exe /P）。再確認の問いで [キャンセル] を押し、サービスの再開を見てから頁を閉じる。"""
    d = _Driver(child, proof_dir, record, on_question)
    dialog, children, elapsed = d.await_question(fake_started, matches=matches)
    buttons = d.check_question(label, dialog, children, elapsed)
    d.press(dialog, buttons["cancel"], IDCANCEL, "cancel")
    d.finish_after_cancel(ready_to_close=service_resumed)


def drive_retry_continue(
    child, started: float, proof_dir, record, on_question, before_retry, matches=question_matches, label: str = "first"
) -> None:
    """child は /P で起動済みの setup。matches に合う問い（既定は最初の問い、再確認なら recheck_question_matches）で、
    偽シェルを止めてから [再試行] を押し、上書きの完了を待つ。"""
    d = _Driver(child, proof_dir, record, on_question)
    dialog, children, elapsed = d.await_question(started, matches=matches)
    buttons = d.check_question(label, dialog, children, elapsed)
    before_retry()
    d.press(dialog, buttons["retry"], IDRETRY, "retry")
    d.finish_after_retry()


def drive_notices(child, started: float, proof_dir, record, notices, ready_to_close) -> None:
    """child は /P で起動済みの setup。notices の (label, matches, on_shown, while_shown) を順に待って OK を押し、
    最後に失敗の頁を閉じる。想定外の dialog が先に出れば await_question が失敗する（順序の確認）。"""
    d = _Driver(child, proof_dir, record, lambda *_: None)
    since = started
    for label, matches, on_shown, while_shown in notices:
        dialog, children, elapsed = d.await_question(since, matches=matches)
        button, control_id = d.check_notice(label, dialog, children, elapsed)
        on_shown(label, elapsed)
        hold_until = time.monotonic() + NOTICE_HOLD_SECONDS
        while time.monotonic() < hold_until:
            if not d.api.u.IsWindow(dialog["hwnd"]):
                raise VisibleFailure(f"notice_closed_by_itself_{label}")
            rc = child.poll()
            if rc is not None:
                raise VisibleFailure(f"setup_exited_while_notice_shown_{label}", {"exit": rc})
            while_shown(label)
            time.sleep(POLL_SECONDS)
        since = time.monotonic()
        d.press(dialog, button, control_id, "ok")
    d.finish_after_cancel(ready_to_close=ready_to_close)


def drive_file_locked_cancel(
    child, started: float, proof_dir, record, matches, on_shown, while_shown, ready_to_close, label: str = "file_locked"
) -> None:
    """child は /P で起動済みの setup。NSIS の `File` の使用中の窓（[再試行]/[キャンセル]）を matches で待ち、
    数秒保持して on_shown/while_shown で状態を見てから [キャンセル] を押し、失敗の頁を ready_to_close が真になるまで開いておく。
    90 秒待ちの問いと違い経過時間の範囲は見ない。"""
    d = _Driver(child, proof_dir, record, lambda *_: None)
    dialog, children, elapsed = d.await_question(started, matches=matches)
    d.record(f"{label}_texts", static_texts(children))
    d.record(f"{label}_elapsed_seconds", round(elapsed, 1))
    width, height, rgb = d.api.capture(dialog["rect"])
    (d.proof_dir / f"{label}_question.png").write_bytes(d.gui.png_bytes(width, height, rgb))
    buttons = retry_cancel_buttons(children, lambda h: d.api.u.GetDlgCtrlID(h))
    on_shown(label, elapsed)
    hold_until = time.monotonic() + NOTICE_HOLD_SECONDS
    while time.monotonic() < hold_until:
        if not d.api.u.IsWindow(dialog["hwnd"]):
            raise VisibleFailure(f"dialog_closed_by_itself_{label}")
        rc = child.poll()
        if rc is not None:
            raise VisibleFailure(f"setup_exited_while_dialog_shown_{label}", {"exit": rc})
        while_shown(label)
        time.sleep(POLL_SECONDS)
    d.press(dialog, buttons["cancel"], IDCANCEL, "cancel")
    d.finish_after_cancel(ready_to_close=ready_to_close)


NOTICE_AFTER_CLOSE_LIMIT_SECONDS = 90.0


def drive_file_locked_cancel_then_notice(
    child,
    started: float,
    proof_dir,
    record,
    matches,
    notice_matches,
    on_shown,
    while_shown,
    ready_to_close,
    on_notice,
    label: str = "file_locked",
    notice_label: str = "resume_failed",
) -> None:
    """`drive_file_locked_cancel` の続き: [キャンセル] の後、失敗の頁を閉じると .onInstFailed が MB_OK の警告を出す道。

    失敗の頁は ready_to_close が真になるまで開いておく。閉じた後に出る警告は、主窓が消えて owner が 0 になりうるので
    「pid が同じ・visible・#32770・本文が notice_matches に合う」で探し、owner は記録だけする。主窓以外で本文が
    合わない #32770 が出たら失敗。警告に OK を押した後も、ほかの窓が出ないことを setup の終了まで見る。"""
    d = _Driver(child, proof_dir, record, lambda *_: None)
    dialog, children, elapsed = d.await_question(started, matches=matches)
    d.record(f"{label}_texts", static_texts(children))
    d.record(f"{label}_elapsed_seconds", round(elapsed, 1))
    width, height, rgb = d.api.capture(dialog["rect"])
    (d.proof_dir / f"{label}_question.png").write_bytes(d.gui.png_bytes(width, height, rgb))
    buttons = retry_cancel_buttons(children, lambda h: d.api.u.GetDlgCtrlID(h))
    on_shown(label, elapsed)
    hold_until = time.monotonic() + NOTICE_HOLD_SECONDS
    while time.monotonic() < hold_until:
        if not d.api.u.IsWindow(dialog["hwnd"]):
            raise VisibleFailure(f"dialog_closed_by_itself_{label}")
        rc = child.poll()
        if rc is not None:
            raise VisibleFailure(f"setup_exited_while_dialog_shown_{label}", {"exit": rc})
        while_shown(label)
        time.sleep(POLL_SECONDS)
    d.press(dialog, buttons["cancel"], IDCANCEL, "cancel")

    # 失敗の頁（主窓）の間: 窓が増えないことを見て、ready_to_close が真になるまで閉じない。
    quiet_until = time.monotonic() + QUIET_AFTER_CANCEL_SECONDS
    while time.monotonic() < quiet_until and child.poll() is None:
        d._no_owned_dialog("after_cancel")
        time.sleep(POLL_SECONDS)
    wait_deadline = time.monotonic() + SERVICE_RESUME_LIMIT_SECONDS
    while True:
        rc = child.poll()
        if rc is not None:
            raise VisibleFailure("setup_exited_before_failed_page_closed", {"exit": rc})
        d._no_owned_dialog("before_main_close")
        if ready_to_close():
            break
        if time.monotonic() >= wait_deadline:
            raise VisibleFailure("not_ready_before_main_close")
        time.sleep(POLL_SECONDS)

    # 主窓を閉じて警告を待つ。主窓の hwnd にだけ WM_CLOSE を送る。
    main_hwnd = None
    posts = 0
    last_post = None
    notice = None
    deadline = time.monotonic() + NOTICE_AFTER_CLOSE_LIMIT_SECONDS
    while notice is None:
        rc = child.poll()
        if rc is not None:
            raise VisibleFailure(f"setup_exited_before_notice_{notice_label}", {"exit": rc})
        if time.monotonic() >= deadline:
            raise VisibleFailure(f"notice_not_seen_within_limit_{notice_label}", d.snapshot())
        shown = [
            w for w in d.snapshot() if w["pid"] == d.pid and w["class"] == DIALOG_CLASS and w["visible"]
        ]
        if main_hwnd is None:
            mains = [w for w in shown if not w.get("owner")]
            if len(mains) > 1:
                raise VisibleFailure("ambiguous_main_window", mains)
            if mains:
                main_hwnd = mains[0]["hwnd"]
                d.record("main_window", mains)
        found = []
        for w in shown:
            if w["hwnd"] == main_hwnd:
                continue
            kids = d.children_of(w["hwnd"])
            texts = static_texts(kids)
            if not texts:
                continue  # label が揃うまで待つ
            if any(notice_matches(t) for t in texts):
                found.append((w, kids))
            else:
                raise VisibleFailure("unexpected_dialog_after_main_close", {"window": w, "texts": texts})
        if len(found) > 1:
            raise VisibleFailure("ambiguous_notice_dialog", [w for w, _ in found])
        if found:
            if posts < 1:
                # WM_CLOSE を送る前に警告が出ている = 閉じた結果ではない。早すぎる窓として失敗させる。
                raise VisibleFailure(
                    "unexpected_dialog_after_main_close",
                    {"window": found[0][0], "reason": "notice_before_any_wm_close"},
                )
            notice = found[0]
            break
        now = time.monotonic()
        if main_hwnd is not None and (last_post is None or now - last_post >= CLOSE_RESEND_SECONDS):
            last_post = now
            if d.api.u.PostMessageW(main_hwnd, WM_CLOSE, 0, 0):
                posts += 1
            elif d.api.u.IsWindow(main_hwnd) and child.poll() is None:
                raise VisibleFailure("PostMessage_WM_CLOSE_failed")
        time.sleep(POLL_SECONDS)
    d.record("main_window_close_posts", posts)
    n_dialog, n_children = notice
    n_elapsed = time.monotonic() - started
    button, control_id = d.check_notice(notice_label, n_dialog, n_children, n_elapsed)
    d.record(f"{notice_label}_owner", n_dialog.get("owner"))
    on_notice(notice_label, n_elapsed)
    hold_until = time.monotonic() + NOTICE_HOLD_SECONDS
    while time.monotonic() < hold_until:
        if not d.api.u.IsWindow(n_dialog["hwnd"]):
            raise VisibleFailure(f"notice_closed_by_itself_{notice_label}")
        rc = child.poll()
        if rc is not None:
            raise VisibleFailure(f"setup_exited_while_notice_shown_{notice_label}", {"exit": rc})
        while_shown(notice_label)
        time.sleep(POLL_SECONDS)
    d.press(n_dialog, button, control_id, "ok")

    finish_deadline = time.monotonic() + FINISH_LIMIT_SECONDS
    while child.poll() is None and time.monotonic() < finish_deadline:
        extra = [
            w
            for w in d.snapshot()
            if w["pid"] == d.pid and w["class"] == DIALOG_CLASS and w["visible"] and w["hwnd"] != main_hwnd
        ]
        if extra:
            raise VisibleFailure(
                "unexpected_dialog_after_notice",
                [{"window": w, "texts": static_texts(d.children_of(w["hwnd"]))} for w in extra],
            )
        time.sleep(POLL_SECONDS)
