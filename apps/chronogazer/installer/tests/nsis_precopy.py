"""Manual Windows NSIS pre-copy regression (not run automatically by CI).

Requires Windows, Python 3, and NSIS (the existing Tauri NSIS cache is the default).
From repository root (other working directories may use the script absolute path):
    python apps/chronogazer/installer/tests/nsis_precopy.py
    python apps/chronogazer/installer/tests/nsis_precopy.py --makensis C:/NSIS/makensis.exe

Uses the real installer hook and real FileOpen/File instructions. Only the service
restart operation is stubbed. Fixture .exe files contain inert TEXT and are never
executed. The generated user/silent harness runs only against a fresh repository
target directory; this does not generate or execute a product installer setup.
No SCM calls or real product processes are allowed in the expanded harness.
"""

import argparse
import ctypes
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import threading
import time
import uuid


ROOT = Path(__file__).resolve().parents[4]
EXES = ("chronogazer.exe", "chronogazer-server.exe", "chronogazer-elev.exe")
CASES = (
    ("shell-locked", (0,), 1, 0, False),
    ("server-locked", (1,), 1, 0, False),
    ("elev-locked", (2,), 1, 0, False),
    ("all-locked", (0, 1, 2), 1, 0, False),
    ("already-stopped", (1,), 0, 0, False),
    ("restart-failed", (1,), 1, 1, False),
    ("fresh", (), 0, 0, True),
    ("unlocked", (), 1, 0, False),
    ("short-lock", (1,), 1, 0, False),
    ("readonly", (), 1, 0, False),
    ("readonly-locked", (1,), 1, 0, False),
)

# Include the real hook BEFORE overriding the one service-operation macro.
TEMPLATE = r'''Unicode true
RequestExecutionLevel user
SilentInstall silent
Name "Isolated pre-copy gate"
OutFile "@OUT@"
!include "LogicLib.nsh"
!define BANTO_INSTALLER_COMMON_DIR "@COMMON@"
!include "@HOOK@"
!macroundef StartServiceIfStoppedAndWait
!macro StartServiceIfStoppedAndWait service was_running
  StrCpy $0 "0"
  ${If} ${was_running} == "1"
    FileOpen $R9 "@LOG@" a
    FileSeek $R9 0 END
    FileWrite $R9 "restart names=$2$\r$\n"
    FileClose $R9
    StrCpy ${was_running} "0"
    StrCpy $0 "@CODE@"
  ${EndIf}
!macroend
Section
  StrCpy $INSTDIR "@INSTALL@"
  StrCpy $ChronoGazerWasRunning "@RUNNING@"
  StrCpy $ChronoGazerShellWasQuit "0"
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWrite $R9 "gate-begins$\r$\n"
  FileClose $R9
  !insertmacro RequireChronoGazerInstallExesUnlocked
  @READONLY_CHECK@
  @FRESH_CHECK@
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWrite $R9 "copy-begins$\r$\n"
  FileClose $R9
  SetOutPath "$INSTDIR"
  File "/oname=chronogazer.exe" "@PAYLOAD@"
  File "/oname=chronogazer-server.exe" "@PAYLOAD@"
  File "/oname=chronogazer-elev.exe" "@PAYLOAD@"
SectionEnd
Function .onInstFailed
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWrite $R9 "callback-before flag=$ChronoGazerWasRunning names=$2$\r$\n"
  FileClose $R9
  !insertmacro ChronoGazerOnInstFailed
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWrite $R9 "callback-after flag=$ChronoGazerWasRunning$\r$\n"
  FileClose $R9
FunctionEnd
'''


def require(condition, label):
    if not condition:
        raise RuntimeError(label)


def run_tool(command, timeout):
    return subprocess.run(
        [str(arg) for arg in command],
        capture_output=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )


def file_hash(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def win32_api():
    api = ctypes.WinDLL("kernel32", use_last_error=True)
    api.CreateFileW.argtypes = [
        ctypes.c_wchar_p, ctypes.c_uint32, ctypes.c_uint32,
        ctypes.c_void_p, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_void_p,
    ]
    api.CreateFileW.restype = ctypes.c_void_p
    api.CloseHandle.argtypes = [ctypes.c_void_p]
    api.CloseHandle.restype = ctypes.c_int
    api.GetFileAttributesW.argtypes = [ctypes.c_wchar_p]
    api.GetFileAttributesW.restype = ctypes.c_uint32
    api.SetFileAttributesW.argtypes = [ctypes.c_wchar_p, ctypes.c_uint32]
    api.SetFileAttributesW.restype = ctypes.c_int
    return api


def run_case(work, makensis, api, case):
    name, locked_indexes, was_running, restart_result, fresh = case
    directory = work / name
    directory.mkdir()
    install = directory / "install"
    install.mkdir()
    payload = directory / "new-payload.txt"
    payload.write_bytes(b"NEW inert fixture\n")
    before = {}
    if not fresh:
        for name in EXES:
            fixture = install / name
            fixture.write_bytes(("OLD " + name + "\n").encode())
            before[name] = file_hash(fixture)

    readonly_before = None
    readonly_abort_before = {}
    if case[0] == "readonly":
        readonly_path = str(install / EXES[1])
        attrs = api.GetFileAttributesW(readonly_path)
        require(attrs != 0xFFFFFFFF, "get_fixture_attributes")
        # ARCHIVE is a non-READONLY bit whose preservation is checked at the gate.
        readonly_before = attrs | 0x21
        require(api.SetFileAttributesW(readonly_path, readonly_before), "set_fixture_readonly")
    if case[0] == "readonly-locked":
        # Both the locked server and an unlocked shell start READONLY.
        for index in (0, 1):
            path = str(install / EXES[index])
            attrs = api.GetFileAttributesW(path)
            require(attrs != 0xFFFFFFFF, "get_abort_fixture_attributes")
            readonly_abort_before[EXES[index]] = attrs | 0x21
            require(api.SetFileAttributesW(path, attrs | 0x21), "set_abort_fixture_readonly")
    log = directory / "events.log"
    fresh_check = "\n".join(
        '${If} ${FileExists} "$INSTDIR\\' + name + '"\n'
        'Abort "fresh check created a missing file"\n${EndIf}'
        for name in EXES
    ) if fresh else ""
    replacements = {
        "@OUT@": directory / "harness.exe",
        "@COMMON@": ROOT / "installer-common",
        "@HOOK@": ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh",
        "@LOG@": log,
        "@CODE@": restart_result,
        "@INSTALL@": install,
        "@RUNNING@": was_running,
        "@PAYLOAD@": payload,
        "@FRESH_CHECK@": fresh_check,
        "@READONLY_CHECK@": r"""System::Call 'kernel32::GetFileAttributesW(w "$INSTDIR\chronogazer-server.exe") i .r8'
FileOpen $R9 "__LOG__" a
FileSeek $R9 0 END
FileWrite $R9 "gate-attributes=$8$\r$\n"
FileClose $R9""".replace("__LOG__", str(log)) if readonly_before is not None else "",
    }
    script = TEMPLATE
    for marker, value in replacements.items():
        if marker not in ("@FRESH_CHECK@", "@READONLY_CHECK@"):
            require(not any(char in str(value) for char in ('"', "$", "\n", "\r")),
                    "NSIS_special_character_in_input_path")
        script = script.replace(marker, str(value))
    nsi = directory / "harness.nsi"
    nsi.write_text(script, encoding="utf-8")

    expanded = run_tool([makensis, "/INPUTCHARSET", "UTF8", "/PPO", nsi], 20)
    require(expanded.returncode == 0, "NSIS_preprocess_failed")
    require(not re.search(
        r"^\s*(?:Exec(?:Wait|Shell(?:Wait)?)?\b|nsExec::)",
        expanded.stdout, re.MULTILINE | re.IGNORECASE,
    ), "external_operation_in_expanded_harness")
    require(not re.search(r"advapi32::|OpenSCManager|OpenService", expanded.stdout, re.IGNORECASE),
            "SCM_operation_in_expanded_harness")
    (directory / "expanded.nsi").write_text(expanded.stdout, encoding="utf-8")
    compiled = run_tool([makensis, "/INPUTCHARSET", "UTF8", "/V2", nsi], 30)
    (directory / "compile.log").write_text(compiled.stdout + compiled.stderr, encoding="utf-8")
    require(compiled.returncode == 0, "NSIS_compile_failed")

    handles = []
    handles_lock = threading.Lock()
    cancel = threading.Event()
    gate_observed = threading.Event()
    released = threading.Event()
    thread_errors = []
    worker = None

    def release_handles():
        with handles_lock:
            for handle in handles:
                api.CloseHandle(handle)
            handles.clear()

    def release_short_lock():
        try:
            deadline = time.monotonic() + 10
            while not cancel.is_set():
                try:
                    if "gate-begins" in log.read_text():
                        gate_observed.set()
                        break
                except (FileNotFoundError, PermissionError):
                    pass
                require(time.monotonic() < deadline, "gate_marker_timeout")
                cancel.wait(0.02)
            if cancel.is_set():
                return
            probe = api.CreateFileW(str(install / EXES[1]), 0x40000000, 7, None, 3, 0, None)
            denied = probe in (None, ctypes.c_void_p(-1).value)
            if not denied:
                api.CloseHandle(probe)
            require(denied, "short_fixture_not_write_denied")
            # Start timing only after the child reached the real gate and denial was observed.
            if not cancel.wait(1.2):
                release_handles()
                released.set()
        except Exception as error:
            thread_errors.append(str(error))
            release_handles()

    started = time.monotonic()
    try:
        for index in locked_indexes:
            handle = api.CreateFileW(str(install / EXES[index]), 0x80000000, 1, None, 3, 0, None)
            require(handle not in (None, ctypes.c_void_p(-1).value), "fixture_lock_failed")
            handles.append(handle)
        if case[0] == "short-lock":
            worker = threading.Thread(target=release_short_lock)
            worker.start()
        child = run_tool([directory / "harness.exe"], 20)
    finally:
        cancel.set()
        if worker is not None:
            worker.join(timeout=2)
        release_handles()
    require(worker is None or not worker.is_alive(), "release_thread_not_joined")
    require(not thread_errors, "release_thread_failed")
    elapsed = time.monotonic() - started
    events = log.read_text().splitlines()
    restarts = [event for event in events if event.startswith("restart ")]
    if locked_indexes and case[0] != "short-lock":
        require(elapsed >= 4.5, "persistent_lock_was_not_retried")
        require(child.returncode == 2 and "copy-begins" not in events, "must_abort_before_any_copy")
        require(before == {name: file_hash(install / name) for name in EXES}, "old_hashes_changed")
        require(len(restarts) == was_running, "restart_count_wrong")
        callbacks = [event for event in events if event.startswith("callback-before ")]
        require(len(callbacks) == 1 and callbacks[0].startswith("callback-before flag=0 names=")
                and "callback-after flag=0" in events, "callback_restarted_or_flag_wrong")
        callback_names = callbacks[0].split(" names=", 1)[1].split()
        require(callback_names == [EXES[index] for index in locked_indexes],
                "failed_names_lost_after_real_restart_wrapper")
        for name, attrs in readonly_abort_before.items():
            require(api.GetFileAttributesW(str(install / name)) == (attrs & ~1),
                    "readonly_clear_not_retained_after_abort")
        if was_running:
            require(events.index(restarts[0]) < events.index(callbacks[0]),
                    "restart_not_before_abort_callback")
            require(all(EXES[index] in restarts[0] for index in locked_indexes), "failed_names_lost")
    else:
        require(child.returncode == 0 and "copy-begins" in events and not restarts,
                "normal_path_failed")
        if case[0] == "short-lock":
            require(gate_observed.is_set() and released.is_set(), "short_lock_not_observed_and_released")
        if readonly_before is not None:
            gate_attrs = [int(event.split("=", 1)[1]) for event in events
                          if event.startswith("gate-attributes=")]
            require(gate_attrs == [readonly_before & ~1], "other_attributes_changed_at_gate")
        require(all((install / name).read_bytes() == payload.read_bytes() for name in EXES),
                "normal_copy_incomplete")
    return {
        "case": case[0], "passed": True, "exitCode": child.returncode, "events": events,
        "originalHashesPreserved": True if locked_indexes and case[0] != "short-lock" else None,
        "freshInput": fresh, "elapsedSeconds": round(elapsed, 3),
        "readonlyOtherBitsPreservedAtGate": True if readonly_before is not None else None,
        "readonlyClearingRetainedAfterAbort": True if readonly_abort_before else None,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--makensis", type=Path, default=None)
    args = parser.parse_args()
    require(os.name == "nt", "Windows_required")
    makensis = args.makensis or Path(os.environ["LOCALAPPDATA"]) / "tauri/NSIS/makensis.exe"
    require(makensis.is_file(), "NSIS_not_found_use_makensis_option")
    work = ROOT / "target/973-precopy-isolated" / ("manual-" + str(uuid.uuid4()))
    work.mkdir(parents=True)
    report = {"success": False, "realGateAndFileChecks": True, "serviceOperationStubbed": True,
              "noSCMOrProductExecution": True, "cases": [],
              "limits": "Share-write denial only; no real product/SCM, ACL faults, races or partial writes."}
    try:
        api = win32_api()
        for case in CASES:
            report["cases"].append(run_case(work, makensis, api, case))
            print(json.dumps({"case": case[0], "passed": True}), flush=True)
        report["success"] = True
    finally:
        result = work / "result.json"
        result.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps({"result": str(result), "success": report["success"],
                          "passedCases": len(report["cases"])}))


if __name__ == "__main__":
    main()
