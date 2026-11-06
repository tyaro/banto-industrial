"""#973 lock-abort (case C-server-locked) on a REAL SCM, ONLY on disposable GitHub-hosted Windows runners.

Never invoke this orchestrator on a developer PC. The dedicated workflow
(installer-lock-abort.yml) supplies both explicit opt-ins, the fresh RUNNER_TEMP root
and its marker, and the guarded running dummy at <root>\\dummy\\running_dummy.exe.
Importing this module or calling its pure helpers opens no SCM/User32 API.

The real hooks run in a user/NON-silent NSIS harness (the main MessageBox must be
really shown). chronogazer-server.exe in a fixture INSTDIR is held open with
GENERIC_READ + FILE_SHARE_READ only (the same lock as nsis_precopy.py), so the real
RequireChronoGazerInstallExesUnlocked aborts before any copy. Proven here: the dummy
service stopped by the harness is restarted BEFORE the main dialog, stays RUNNING with
the same new PID while the dialog is visible and after exit, and the non-silent
.onInstFailed (flag 0) does not restart it a second time. LockFileEx range locks and
image locks of a running exe are out of scope. Production hooks are not modified;
nsis_delete_pending_scm.py is reused by import/subclass only.
"""

import argparse
import ctypes as C
from ctypes import wintypes as W
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

sys.dont_write_bytecode = True
from nsis_precopy import ROOT, EXES, require, win32_api
from nsis_acl_abort import logical_ppo_lines, log_code
import nsis_delete_pending_scm as base
from nsis_delete_pending_scm import Trial, digest, write_json, has_install_protocol, nsis_words, reject_relative_jumps
import windows_gui_capability as gui
from windows_gui_capability import API, capability_reasons, png_bytes

CASE = "C-server-locked"
SERVICE = "ChronoGazer"
LOCKED_EXE = "chronogazer-server.exe"
# 文面定数はここだけに置く（hooks の MessageBox の文面と一致させる。hooks 側は変更しない）。
MAIN_DIALOG_TEXT = "使用中、または書き込めない実行ファイルがあります"
MAIN_DIALOG_PREFIX = MAIN_DIALOG_TEXT + "（" + LOCKED_EXE + "）"
RESTART_FAILED_TEXT = "再開できませんでした"
INACTIVE_TEXTS = ("サービスが停止の要求を受け付けず", "サービスの停止がまだ終わっていません",
                  "サービスがまだ止まっていません")
CALLBACK_DIALOG_TEXT = "$2"
DIALOG_CLASS = "#32770"
RUNNING, STOPPED = 4, 1
WM_CLOSE, WM_COMMAND, BN_CLICKED, GW_OWNER = 0x0010, 0x0111, 0, 4

SC_QUERIES = tuple("nsExec::ExecToStack 'cmd /c sc query \"ChronoGazer\" | findstr /C:\"" + state + "\" >nul'"
                   for state in ("RUNNING", "STOPPED", "PENDING"))
SC_STOP = "nsExec::ExecToStack 'sc stop \"ChronoGazer\"'"
SC_START = "nsExec::ExecToStack 'sc start \"ChronoGazer\"'"
ALLOWED_PPO = frozenset(SC_QUERIES + (SC_STOP, SC_START) + tuple(
    line for exe in EXES for line in (
        "System::Call 'kernel32::GetFileAttributesW(w \"$INSTDIR\\" + exe + "\") i .r0'",
        "System::Call 'kernel32::SetFileAttributesW(w \"$INSTDIR\\" + exe + "\", i r0) i .r0'")) + (
    # install-dir-common.nsh の Function 定義（hook の include で常に入る。harness からは呼ばない）。
    "System::Call 'kernel32::GetFullPathNameW(w r0, i 1024, w .r1, p 0) i .r2'",
    "System::Call 'kernel32::CreateFileW(w r1, i 0, i 7, p 0, i 3, i 33554432, p 0) p .r3'",
    "System::Call 'kernel32::GetFinalPathNameByHandleW(p r3, w .r4, i 1024, i 0) i .r5'",
    "System::Call 'kernel32::CloseHandle(p r3)'",
))

# 期待する events の並び（sc-query を除く）。数・順序・不在（copy-begins・他の dialog）を一度に見る。
EVENTS_AT_DIALOG = ("section-begins", "stop-attempt", "preinstall-done flag=1", "restart-attempt", "main-dialog flag=0")
EVENTS_FINAL = EVENTS_AT_DIALOG + ("callback-before flag=0", "callback-after flag=0")

TEMPLATE = r'''Unicode true
RequestExecutionLevel user
SilentInstall normal
ShowInstDetails show
AutoCloseWindow false
Name "@NAME@"
Caption "@NAME@"
OutFile "@OUT@"
Page instfiles
!include "LogicLib.nsh"
!define BANTO_INSTALLER_COMMON_DIR "@COMMON@"
!include "@HOOK@"
Function .onInit
  SetRegView 64
  StrCpy $INSTDIR "@INSTALL@"
  StrCpy $ChronoGazerShellWasQuit "0"
FunctionEnd
Section
  @LOG_SECTION@
  !insertmacro StopServiceIfRunning "ChronoGazer" $ChronoGazerWasRunning
  !insertmacro RequireChronoGazerServiceInactive "インストール"
  @LOG_PREINSTALL@
  !insertmacro RequireChronoGazerInstallExesUnlocked
  @LOG_COPY@
  SetOutPath "$INSTDIR"
  File "/oname=chronogazer.exe" "@PAYLOAD@"
  File "/oname=chronogazer-server.exe" "@PAYLOAD@"
  File "/oname=chronogazer-elev.exe" "@PAYLOAD@"
SectionEnd
Function .onInstFailed
  @CALLBACK_BEFORE@
  !insertmacro ChronoGazerOnInstFailed
  @CALLBACK_AFTER@
FunctionEnd
'''


class GuiUnsupported(RuntimeError):
    pass


# ---- pure helpers (unit-tested with fake data; no Win32) ----

def validate_environment(values):
    base.validate_environment(values)
    require(values.get("BANTO_973_GUI_TEST") == gui.OPT_IN, "refused_environment_BANTO_973_GUI_TEST")


def image_path(dummy, run_id, sha):
    return f'"{dummy}" run-service --run-id {run_id} --sha {sha}'


def command_matches(command, dummy, probe, run_id, sha):
    command = command.replace('\\\\?\\', '')
    accepted = (image_path(dummy, run_id, sha), f'{dummy} run-service --run-id {run_id} --sha {sha}', f'"{probe}" run-service', f'{probe} run-service')
    return command.casefold() in {item.casefold() for item in accepted}


def binding_problems(binding, expected):
    if not isinstance(binding, dict) or set(binding) != set(expected):
        return ["binding_keys"]
    problems = []
    for key, want in expected.items():
        got = binding[key]
        same = isinstance(got, str) and (got.casefold() == want.casefold() if key in ("root", "image_path") else got == want)
        if not same:
            problems.append("binding_" + key)
    return problems


def dialog_kind(line):
    words = nsis_words(line)
    text = words[2] if len(words) > 2 else ""
    if MAIN_DIALOG_TEXT in text:
        return "main-dialog"
    if RESTART_FAILED_TEXT in text:
        return "restart-failed-dialog"
    if any(item in text for item in INACTIVE_TEXTS):
        return "inactive-dialog"
    if text == CALLBACK_DIALOG_TEXT:
        return "callback-dialog"
    return "other-dialog"


def audit_install_ppo(text):
    counts = {}
    for line in logical_ppo_lines(text):
        line = line.strip()
        reject_relative_jumps(line)
        if re.match(r'(?:Exec\w*\b|nsExec::|(?:Read|Write|Delete)Reg\w*\b|System::Call)', line):
            require(line in ALLOWED_PPO, "PPO_operation_outside_allowlist")
            counts[line] = counts.get(line, 0) + 1
    require(counts.get(SC_STOP, 0) == 1 and counts.get(SC_START, 0) >= 1, "PPO_service_operations_missing")
    return counts


def instrument(ppo_text, log):
    """case_b と同形: production 行は全て残し、その直前に観測の 1 行を足す。"""
    out = []
    for line in logical_ppo_lines(ppo_text):
        stripped = line.strip()
        if stripped.startswith("nsExec::"):
            kind = ("stop-attempt" if stripped == SC_STOP else "restart-attempt" if stripped == SC_START
                    else "sc-query" if stripped in SC_QUERIES else None)
            require(kind is not None, "unclassified_exec")
            out.append(log_code(log, kind))
        if stripped.startswith("MessageBox "):
            out.append(log_code(log, dialog_kind(stripped) + " flag=$ChronoGazerWasRunning"))
        out.append(line)
    return '\n'.join(out) + '\n'


def parse_events(raw):
    """UTF-16LE の events。書き込み途中の最後の行は捨てる。"""
    raw = raw[:len(raw) - len(raw) % 2]
    return raw.decode("utf-16-le").split("\r\n")[:-1]


def parse_jsonl(text):
    """書き込み途中の最後の行（改行で終わっていない）は捨てる。完結した行は必ず JSON。"""
    return [json.loads(line) for line in text.split("\n")[:-1] if line.strip()]


def event_problems(events, final):
    core = [event for event in events if event != "sc-query"]
    expected = list(EVENTS_FINAL if final else EVENTS_AT_DIALOG)
    return [] if core == expected else ["event_sequence:" + json.dumps(core, ensure_ascii=False)]


def start_problems(starts, expected, pids, dialog_wall_ms=None):
    problems = [] if len(starts) == len(pids) else [f"starts_count_{len(starts)}"]
    for index, (start, pid) in enumerate(zip(starts, pids)):
        if start.get("event") != "start" or start.get("pid") != pid:
            problems.append(f"start{index}_event_or_pid")
        for key in ("run_id", "sha", "exe_sha256"):
            if start.get(key) != expected[key]:
                problems.append(f"start{index}_{key}")
        if str(start.get("root", "")).casefold() != expected["root"].casefold():
            problems.append(f"start{index}_root")
        if start.get("args") != [SERVICE]:
            problems.append(f"start{index}_args")
    if dialog_wall_ms is not None and not (len(starts) >= 2 and isinstance(starts[1].get("wall_ms"), int)
                                           and starts[1]["wall_ms"] < dialog_wall_ms):
        problems.append("restart_not_before_dialog")
    return problems


def stop_problems(stops, pids):
    problems = [] if len(stops) == len(pids) else [f"stops_count_{len(stops)}"]
    for index, (stop, pid) in enumerate(zip(stops, pids)):
        if stop.get("event") != "stop" or stop.get("signal") != "Stop" or stop.get("pid") != pid:
            problems.append(f"stop{index}_mismatch")
    return problems


def heartbeat_problems(earlier, later, pid_a, pid_b, run_id, sha):
    """later の最終行が pid_b で、earlier 時点の pid_b の最大 seq から 2 以上進んでいること。

    earlier の最終行は pid_a でもよい（pid_b の最初の heartbeat は起動 500ms 後で、
    dialog の初観測がそれより早いことがある）。それ以外の PID は失敗。
    """
    problems = []
    if earlier and earlier[-1].get("pid") not in (pid_a, pid_b):
        problems.append("heartbeat_foreign_pid")
    baseline = max([beat.get("seq", 0) for beat in earlier if beat.get("pid") == pid_b], default=0)
    last = later[-1] if later else {}
    if last.get("pid") != pid_b or last.get("run_id") != run_id or last.get("sha") != sha:
        problems.append("heartbeat_not_bound_to_pid_b")
    elif not (isinstance(last.get("seq"), int) and last["seq"] - baseline >= 2):
        problems.append("heartbeat_not_advancing")
    return problems


def timeline_problems(timeline, pid_b):
    after = [entry for entry in timeline if entry["phase"] != "before-dialog"]
    problems = []
    if not any(entry["phase"] == "dialog" for entry in after) or not any(entry["phase"] == "after-exit" for entry in after):
        problems.append("timeline_phases_missing")
    if any(entry["state"] != RUNNING or entry["pid"] != pid_b for entry in after):
        problems.append("service_not_steady_running_pid_b")
    return problems


def classify_dialog_children(children, prefix=MAIN_DIALOG_PREFIX):
    texts = [child["title"] for child in children if child["class"].casefold() == "static" and child["title"]]
    if not texts:
        return "waiting"  # 子 Static が読めるまで待つ（checklist §3）。
    return "main" if any(text.startswith(prefix) for text in texts) else "other"


def ok_button(children):
    """MessageBox の OK ボタン（Button・visible・title が OK）がちょうど 1 つのときだけ返す。"""
    found = [c for c in children if c["class"].casefold() == "button" and c["visible"] and c["title"] == "OK"]
    require(len(found) == 1, "ok_button_not_unique")
    return found[0]


def owned_dialogs(windows, pid):
    return [w for w in windows if w["pid"] == pid and w["class"] == DIALOG_CLASS and w["visible"] and w.get("owner")]


def main_windows(windows, pid):
    return [w for w in windows if w["pid"] == pid and w["class"] == DIALOG_CLASS and w["visible"] and not w.get("owner")]


def judge_dialogs(windows, children_of, pid, caption):
    """MessageBox も NSIS 主窓も #32770。pid・owner 非 0・title・子 Static 文面で主 dialog を同定する。"""
    found, unexpected = [], []
    for window in owned_dialogs(windows, pid):
        kind = classify_dialog_children(children_of(window["hwnd"])) if window["title"] == caption else "other"
        if kind == "main":
            found.append(window)
        elif kind == "other":
            unexpected.append(window)
    require(len(found) <= 1, "ambiguous_main_dialog")
    return (found[0] if found else None), unexpected


# ---- Win32 side (disposable runner only) ----

def guard():
    validate_environment(os.environ)
    return base.guard()


def read_bytes(path):
    try:
        return path.read_bytes()
    except FileNotFoundError:
        return b""


class LockTrial(Trial):
    def __init__(self, root):
        super().__init__(root)
        self.dummy_dir = root / "dummy"
        self.dummy = self.dummy_dir / "running_dummy.exe"
        self.run_id = os.environ["GITHUB_RUN_ID"]
        self.sha = os.environ["BANTO_973_EXPECTED_SHA"]
        for name, args, result in (
            ("StartServiceW", [W.HANDLE, W.DWORD, C.c_void_p], W.BOOL),
            ("ControlService", [W.HANDLE, W.DWORD, C.c_void_p], W.BOOL),
            ("QueryServiceStatusEx", [W.HANDLE, C.c_int, C.c_void_p, W.DWORD, C.POINTER(W.DWORD)], W.BOOL),
        ):
            function = getattr(self.scm.api, name)
            function.argtypes, function.restype = args, result

    def record_ownership(self, name, created=False):
        require(name == SERVICE, "invalid_owned_name")
        self.owned[name] = {"created": created, "executable": str(self.dummy), "kind": "running_dummy",
                            "run_id": self.run_id, "sha": self.sha}
        write_json(self.root / "ownership.json", self.owned)
        (self.root / "owned-services.txt").write_text('\n'.join(self.owned) + '\n', encoding="utf-8")

    def command_is_ours(self, name):
        return command_matches(self.scm.command(name), self.dummy.resolve(), self.probe.resolve(), self.run_id, self.sha)

    def status(self, name=SERVICE):
        handle, error = self.scm.open(name, 4)
        if not handle:
            return {"open_error": error}
        try:
            buffer, needed = (W.DWORD * 9)(), W.DWORD()
            require(bool(self.scm.api.QueryServiceStatusEx(handle, 0, buffer, C.sizeof(buffer), C.byref(needed))),
                    "QueryServiceStatusEx_failed")
            return {"open_error": 0, "state": int(buffer[1]), "pid": int(buffer[7]),
                    "win32_exit": int(buffer[3]), "specific_exit": int(buffer[4])}
        finally:
            self.scm.close(handle)

    def start(self, name=SERVICE):
        handle, error = self.scm.open(name, 0x10 | 4)
        require(bool(handle), "start_open_" + str(error))
        try:
            started = bool(self.scm.api.StartServiceW(handle, 0, None))
            error = 0 if started else C.get_last_error()
        finally:
            self.scm.close(handle)
        require(started, "StartService_failed_" + str(error))

    def stop(self, name=SERVICE):
        handle, error = self.scm.open(name, 0x20 | 4)
        require(bool(handle), "stop_open_" + str(error))
        try:
            stopped = bool(self.scm.api.ControlService(handle, 1, (W.DWORD * 7)()))
            error = 0 if stopped else C.get_last_error()
        finally:
            self.scm.close(handle)
        require(stopped or error == 1062, "ControlService_stop_failed_" + str(error))

    def wait_state(self, state, timeout, name=SERVICE):
        deadline, last = time.monotonic() + timeout, {}
        while time.monotonic() < deadline:
            last = self.status(name)
            if last.get("state") == state:
                return last
            time.sleep(.2)
        raise RuntimeError(f"service_state_{state}_not_reached:{json.dumps(last)}")

    def jsonl(self, name):
        return parse_jsonl(read_bytes(self.dummy_dir / name).decode("utf-8"))

    def collect_dummy_logs(self):
        target = self.proof / CASE / "dummy-logs"
        target.mkdir(parents=True, exist_ok=True)
        for path in self.dummy_dir.glob("*"):
            if path.suffix in (".jsonl", ".log", ".json"):
                shutil.copyfile(path, target / path.name)

    def cleanup(self):
        errors = []
        for name in list(self.owned):
            try:  # Running の自分の dummy を先に止めてから、基底の削除に渡す。
                state = self.status(name).get("state")
                if state is not None and state != STOPPED:
                    require(self.command_is_ours(name), "foreign_running_service_refused")
                    if state != 3:  # STOP_PENDING はそのまま待つ。
                        self.stop(name)
                    self.wait_state(STOPPED, 30, name)
            except Exception as error:
                errors.append({"name": name, "stop_failure": str(error)})
        try:
            self.collect_dummy_logs()
        except Exception as error:
            errors.append({"dummy_logs": True, "failure": str(error)})
        result = super().cleanup()
        result["errors"] = errors + result["errors"]
        result["success"] = not result["errors"]
        return result


class Monitor:
    """100–200ms 間隔の timeline（SCM・heartbeat・events・dialog の有無）。"""

    def __init__(self, trial, child, log, path):
        self.trial, self.child, self.log, self.path = trial, child, log, path
        self.started = time.monotonic()
        self.timeline = []

    def sample(self, phase):
        status = self.trial.status()
        beats = self.trial.jsonl("heartbeat.jsonl")
        events = parse_events(read_bytes(self.log))
        entry = {"t_ms": round((time.monotonic() - self.started) * 1000), "wall_ms": int(time.time() * 1000),
                 "phase": phase, "state": status.get("state"), "pid": status.get("pid"),
                 "hb_seq": beats[-1].get("seq") if beats else None, "hb_pid": beats[-1].get("pid") if beats else None,
                 "events_len": len(events), "last_event": events[-1] if events else None,
                 "dialog_seen": phase == "dialog", "installer_running": self.child.poll() is None}
        self.timeline.append(entry)
        with self.path.open("a", encoding="utf-8") as stream:
            stream.write(json.dumps(entry, ensure_ascii=False) + '\n')
        return entry, events, beats

    def hold(self, seconds, phase):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.sample(phase)
            time.sleep(.15)


def with_owner(api, windows):
    for window in windows:
        window["owner"] = int(api.u.GetWindow(window["hwnd"], GW_OWNER) or 0)
    return windows


def install_hashes(install):
    return {path.name: digest(path) for path in sorted(install.iterdir()) if path.is_file()}


def build_harness(trial, makensis, case, install, log, name):
    payload = case / "payload.bin"
    payload.write_bytes(b"new payload that must never be copied #973 lock-abort\n")
    values = {"@OUT@": case / "harness.exe", "@COMMON@": ROOT / "installer-common",
              "@HOOK@": ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh",
              "@INSTALL@": install, "@PAYLOAD@": payload, "@NAME@": name}
    for value in (*values.values(), log):
        require(not any(c in str(value) for c in ('"', '$', '\r', '\n')), "NSIS_special_path")
    script = TEMPLATE
    for marker, message in (("@LOG_SECTION@", "section-begins"), ("@LOG_PREINSTALL@", "preinstall-done flag=$ChronoGazerWasRunning"),
                            ("@LOG_COPY@", "copy-begins"), ("@CALLBACK_BEFORE@", "callback-before flag=$ChronoGazerWasRunning"),
                            ("@CALLBACK_AFTER@", "callback-after flag=$ChronoGazerWasRunning")):
        script = script.replace(marker, log_code(log, message))
    for marker, value in values.items():
        script = script.replace(marker, str(value))
    nsi = case / "harness.nsi"
    nsi.write_text(script, encoding="utf-8")
    expanded = trial.call([makensis, "/INPUTCHARSET", "UTF8", "/OUTPUTCHARSET", "UTF8", "/PPO", nsi], "PPO", case)
    require(expanded.returncode == 0, "PPO_failed")
    (case / "expanded.nsi").write_text(expanded.stdout, encoding="utf-8")
    write_json(case / "ppo-audit.json", audit_install_ppo(expanded.stdout))
    instrumented = case / "instrumented.nsi"
    instrumented.write_text(instrument(expanded.stdout, log), encoding="utf-8")
    require(trial.call([makensis, "/INPUTCHARSET", "UTF8", "/V2", instrumented], "compile", case).returncode == 0, "compile_failed")
    return case / "harness.exe"


def case_c(trial, makensis, report):
    require(trial.absent(SERVICE) and trial.app_absent(), "preexisting_ChronoGazer_or_apps_key")
    case = trial.proof / CASE
    case.mkdir(parents=True, exist_ok=True)
    result = {"case": CASE, "forced_termination": False}
    report["C"] = result
    # 4. inert 3 exe + data.txt（INSTDIR は dummy の外）。
    install = case / "install"
    install.mkdir()
    for exe in EXES:
        (install / exe).write_bytes(f"inert {exe}, never executed #973\n".encode())
    (install / "data.txt").write_bytes(b"data retained\n")
    before = install_hashes(install)
    write_json(case / "before-hashes.json", before)
    # 5. dummy の登録。
    trial.record_ownership(SERVICE)
    installed = trial.call([trial.dummy, "install"], "dummy-install")
    require(installed.returncode == 0 and has_install_protocol(installed.stdout, "install-kind=Created"), "dummy_install_not_created")
    trial.record_ownership(SERVICE, created=True)
    result["service_command"] = trial.scm.command(SERVICE)
    require(trial.command_is_ours(SERVICE), "installed_command_not_ours")
    require(trial.status().get("state") == STOPPED, "dummy_service_not_stopped")
    expected = {"run_id": trial.run_id, "sha": trial.sha, "root": str(trial.root), "exe_sha256": digest(trial.dummy),
                "service_name": SERVICE, "image_path": image_path(trial.dummy.resolve(), trial.run_id, trial.sha)}
    binding = json.loads((trial.dummy_dir / "binding.json").read_text(encoding="utf-8"))
    result["binding"] = binding
    require(not binding_problems(binding, expected), "binding_mismatch:" + ','.join(binding_problems(binding, expected)))
    # 6. 初回起動（installer の前に束縛が通って Running になることを確かめる）。
    trial.start()
    pid_a = trial.wait_state(RUNNING, 15)["pid"]
    result["pid_a"] = pid_a
    def initial_heartbeat_ok():
        beats = trial.jsonl("heartbeat.jsonl")
        return bool(beats) and beats[-1].get("pid") == pid_a and beats[-1].get("seq", 0) >= 2

    deadline = time.monotonic() + 5
    while time.monotonic() < deadline and not initial_heartbeat_ok():
        time.sleep(.2)
    require(not start_problems(trial.jsonl("starts.jsonl"), expected, [pid_a]), "initial_start_not_bound")
    require(initial_heartbeat_ok(), "initial_heartbeat_not_advancing")
    # 7–9. harness。
    name = "Banto973 lock-abort " + trial.root.name[-8:]
    log = case / "events.log"
    harness = build_harness(trial, makensis, case, install, log, name)
    api = API()
    bind = gui.bind
    bind(api.u, "GetWindow", [W.HWND, W.UINT], W.HWND)
    bind(api.u, "IsWindow", [W.HWND], W.BOOL)
    bind(api.u, "GetDlgCtrlID", [W.HWND], C.c_int)
    kernel = win32_api()
    invalid = C.c_void_p(-1).value
    lock = None
    child = None
    last_windows = []
    try:
        # 10. lock（nsis_precopy.py と同じ開き方）と自己検証。
        target = str(install / LOCKED_EXE)
        lock = kernel.CreateFileW(target, 0x80000000, 1, None, 3, 0, None)
        require(lock not in (None, invalid), "fixture_lock_failed")
        probe = kernel.CreateFileW(target, 0x40000000, 7, None, 3, 0, None)
        if probe not in (None, invalid):
            kernel.CloseHandle(probe)
        require(probe in (None, invalid), "lock_did_not_deny_write")
        # 11. 起動と timeline。
        environment = dict(os.environ)
        environment["PATH"] = str(Path(os.environ["SystemRoot"]) / "System32")
        child = subprocess.Popen([str(harness)], cwd=case, env=environment)
        result["installer_pid"] = child.pid
        monitor = Monitor(trial, child, log, case / "timeline.jsonl")
        children_of = lambda hwnd: api.windows(child.pid, hwnd)
        dialog = None
        deadline = time.monotonic() + 90
        while dialog is None:
            require(child.poll() is None, "installer_exited_before_dialog")
            last_windows = with_owner(api, api.windows(child.pid))
            dialog, unexpected = judge_dialogs(last_windows, children_of, child.pid, name)
            require(not unexpected, "unexpected_dialog")
            first, events, beats_first = monitor.sample("dialog" if dialog else "before-dialog")
            require(dialog is not None or time.monotonic() < deadline, "dialog_not_seen_within_90s")
            if dialog is None:
                time.sleep(.15)
        # 12–13. 観測 1（検出直後）と観測 2（1.5s 後）。
        dialog_children = children_of(dialog["hwnd"])
        write_json(case / "dialog.json", {"dialog": dialog, "children": dialog_children, "windows": last_windows})
        try:
            width, height, rgb = api.capture(dialog["rect"])
            (case / "dialog.png").write_bytes(png_bytes(width, height, rgb))  # 画素は assert しない。
        except Exception as error:
            result["dialog_png_error"] = str(error)
        pid_b = first["pid"]
        result.update(pid_b=pid_b, dialog_wall_ms=first["wall_ms"])
        require(first["state"] == RUNNING, "not_running_when_dialog_detected")
        require(pid_b != pid_a, "pid_reused_rerun_required")

        def observe(label, beats_earlier):
            windows = with_owner(api, api.windows(child.pid))
            entry, events, beats = monitor.sample("dialog")
            starts, stops = trial.jsonl("starts.jsonl"), trial.jsonl("stops.jsonl")
            alive = bool(api.u.IsWindow(dialog["hwnd"])) and any(w["hwnd"] == dialog["hwnd"] for w in owned_dialogs(windows, child.pid))
            hashes = install_hashes(install)
            problems = ([] if entry["state"] == RUNNING and entry["pid"] == pid_b else ["scm_not_running_pid_b"])
            problems += [] if alive else ["dialog_not_alive"]
            problems += event_problems(events, final=False)
            problems += start_problems(starts, expected, [pid_a, pid_b], first["wall_ms"])
            problems += stop_problems(stops, [pid_a])
            problems += [] if hashes == before else ["install_changed"]
            if beats_earlier is not None:
                problems += heartbeat_problems(beats_earlier, beats, pid_a, pid_b, trial.run_id, trial.sha)
            observed = {"label": label, "entry": entry, "events": events, "starts": starts, "stops": stops,
                        "heartbeat_tail": beats[-3:], "dialog_alive": alive, "hashes": hashes, "problems": problems}
            write_json(case / f"{label}.json", observed)
            require(not problems, label + "_failed:" + ','.join(problems))
            return beats

        beats_one = observe("observation-1", None)
        monitor.hold(1.5, "dialog")
        beats_two = observe("observation-2", beats_one)
        # 14. 主 dialog の OK。
        button = ok_button(dialog_children)
        control_id = int(api.u.GetDlgCtrlID(button["hwnd"]))
        require(control_id > 0, "ok_button_control_id_invalid")
        result["ok_button"] = {"hwnd": button["hwnd"], "control_id": control_id}
        require(bool(api.u.PostMessageW(dialog["hwnd"], WM_COMMAND, (BN_CLICKED << 16) | control_id, button["hwnd"])),
                "PostMessage_BN_CLICKED_failed")
        deadline = time.monotonic() + 10
        while api.u.IsWindow(dialog["hwnd"]):
            require(time.monotonic() < deadline, "dialog_did_not_close")
            monitor.sample("after-ok")
            time.sleep(.15)
        monitor.hold(1.0, "after-ok")
        entry = monitor.timeline[-1]
        require(entry["state"] == RUNNING and entry["pid"] == pid_b and child.poll() is None, "after_ok_state")
        # 15. 主窓（owner==0）を閉じる。新しい owned #32770 は押さずに失敗。
        last_windows = with_owner(api, api.windows(child.pid))
        mains = main_windows(last_windows, child.pid)
        require(len(mains) == 1, "main_window_not_unique")
        result["main_window"] = mains[0]
        require(bool(api.u.PostMessageW(mains[0]["hwnd"], WM_CLOSE, 0, 0)), "PostMessage_WM_CLOSE_failed")
        deadline = time.monotonic() + 30
        while child.poll() is None:
            require(time.monotonic() < deadline, "installer_did_not_exit_after_close")
            last_windows = with_owner(api, api.windows(child.pid))
            require(not owned_dialogs(last_windows, child.pid), "unexpected_dialog_after_close")
            monitor.sample("closing")
            time.sleep(.15)
        # 16. exit 後。
        result["returncode"] = child.returncode
        # 非 silent の exit code は初回 run で確定する。現状は非 0 だけを要求し、
        # 初回 run の観測値（abort で 2 の想定）を確かめたら == 2 に締める。
        require(child.returncode != 0, "installer_exit_zero")
        monitor.hold(1.0, "after-exit")
        entry, events, beats = monitor.sample("after-exit")
        problems = event_problems(events, final=True)
        problems += heartbeat_problems(beats_two, beats, pid_a, pid_b, trial.run_id, trial.sha)
        problems += start_problems(trial.jsonl("starts.jsonl"), expected, [pid_a, pid_b])
        problems += stop_problems(trial.jsonl("stops.jsonl"), [pid_a])
        problems += timeline_problems(monitor.timeline, pid_b)
        result.update(final_events=events, final_problems=problems)
        require(not problems, "after_exit_failed:" + ','.join(problems))
        # 17. lock 解放 → hash 再照合。
        require(bool(kernel.CloseHandle(lock)), "lock_close_failed")
        lock = None
        require(install_hashes(install) == before, "install_changed_after_release")
        # 18. 後片付け（本経路）: 停止 → stops 2 行 → dummy uninstall → 消滅。
        trial.stop()
        trial.wait_state(STOPPED, 30)
        require(not stop_problems(trial.jsonl("stops.jsonl"), [pid_a, pid_b]), "final_stops_mismatch")
        require(trial.call([trial.dummy, "uninstall"], "dummy-uninstall").returncode == 0, "dummy_uninstall_failed")
        trial.wait_absent(SERVICE)
        result["double_restart_absent"] = True
        return result
    finally:
        if child is not None and child.poll() is None:
            try:
                for window in main_windows(with_owner(api, api.windows(child.pid)), child.pid):
                    api.u.PostMessageW(window["hwnd"], WM_CLOSE, 0, 0)
                child.wait(timeout=10)
            except Exception:
                result["forced_termination"] = True
                child.terminate()  # Only the harness process created here.
                try:
                    child.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    result["installer_still_running"] = True
        if lock is not None:
            kernel.CloseHandle(lock)
        write_json(case / "windows-last.json", last_windows)
        write_json(case / "result.json", result)
        trial.collect_dummy_logs()


def finish_cleanup_only(trial):
    cleanup = trial.cleanup()
    write_json(trial.proof / "cleanup-always.json", cleanup)
    summary_path = trial.proof / "summary.json"
    summary = json.loads(summary_path.read_text()) if summary_path.exists() else {}
    write_json(trial.proof / "overall.json", {
        "passed": summary.get("status") == "passed" and summary.get("cleanup_success") is True and cleanup["success"],
        "main_status": summary.get("status", "not_executed"), "always_cleanup_success": cleanup["success"]})
    raise SystemExit(0 if cleanup["success"] else 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--makensis", type=Path)
    parser.add_argument("--cleanup-only", action="store_true")
    args = parser.parse_args()
    root = guard()  # Must precede any SCM/registry/User32 access, including cleanup.
    trial = LockTrial(root)
    if args.cleanup_only:
        finish_cleanup_only(trial)
    report = {"status": "pending", "cleanup_success": False, "case": CASE,
              "head_sha": os.environ["BANTO_973_EXPECTED_SHA"], "event_sha": os.environ["GITHUB_SHA"],
              "limits": "running dummy service + inert fixture exes; READ-share CreateFileW lock only "
                        "(no LockFileEx range lock / image lock); no collector/full setup/Tauri UI/B3-5."}
    write_json(trial.proof / "summary.json", report)
    try:
        environment = API().environment()
        report["gui_environment"] = environment
        reasons = capability_reasons(environment)
        if reasons:
            report["gui_reasons"] = reasons
            raise GuiUnsupported("gui_unsupported")
        require(trial.dummy.is_file() and args.makensis and args.makensis.is_file(), "missing_inputs")
        require(not trial.owned and trial.app_absent() and trial.absent(SERVICE), "preexisting_state")
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        require(head == report["head_sha"], "checkout_sha_mismatch")
        files = [Path(__file__), Path(base.__file__), Path(gui.__file__),
                 ROOT / "crates/banto-service-control/examples/running_dummy.rs",
                 ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh",
                 ROOT / "installer-common/service-common.nsh", ROOT / "installer-common/install-dir-common.nsh"]
        write_json(trial.proof / "provenance.json", {
            "head_sha": head, "event_sha": report["event_sha"], "dummy_sha256": digest(trial.dummy),
            "makensis_sha256": digest(args.makensis),
            "sources": {str(path.resolve().relative_to(ROOT)): digest(path) for path in files}})
        case_c(trial, args.makensis, report)
        report["status"] = "passed"
        print(json.dumps({"case": CASE, "passed": True}), flush=True)
    except GuiUnsupported:
        report["status"] = "gui_unsupported"  # 製品の失敗と区別する（それでも非 0 で終える）。
    except Exception as error:
        report["status"], report["failure"] = "failed", str(error)
    finally:
        cleanup = trial.cleanup()
        write_json(trial.proof / "cleanup-main.json", cleanup)
        report["cleanup_success"] = cleanup["success"]
        if not report["cleanup_success"] and report["status"] == "passed":
            report["status"] = "failed"
        write_json(trial.proof / "summary.json", report)
    raise SystemExit(0 if report["status"] == "passed" else 1)


if __name__ == "__main__":
    main()
