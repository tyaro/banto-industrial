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
  !insertmacro RequireChronoGazerInstallExesUnlocked
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
  FileWrite $R9 "callback-before flag=$ChronoGazerWasRunning$\r$\n"
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
    }
    script = TEMPLATE
    for marker, value in replacements.items():
        if marker != "@FRESH_CHECK@":
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
    try:
        for index in locked_indexes:
            # GENERIC_READ, FILE_SHARE_READ only: deny any writer until child exits.
            handle = api.CreateFileW(str(install / EXES[index]), 0x80000000, 1, None, 3, 0, None)
            require(handle not in (None, ctypes.c_void_p(-1).value), "fixture_lock_failed")
            handles.append(handle)
        child = run_tool([directory / "harness.exe"], 20)
    finally:
        for handle in handles:
            api.CloseHandle(handle)

    events = log.read_text().splitlines()
    restarts = [event for event in events if event.startswith("restart ")]
    if locked_indexes:
        require(child.returncode == 2 and "copy-begins" not in events, "must_abort_before_any_copy")
        require(before == {name: file_hash(install / name) for name in EXES}, "old_hashes_changed")
        require(len(restarts) == was_running, "restart_count_wrong")
        require("callback-before flag=0" in events and "callback-after flag=0" in events,
                "callback_restarted_or_flag_wrong")
        if was_running:
            require(events[0].startswith("restart "), "restart_not_before_abort_callback")
            require(all(EXES[index] in events[0] for index in locked_indexes), "failed_names_lost")
    else:
        require(child.returncode == 0 and events == ["copy-begins"], "normal_path_failed")
        require(all((install / name).read_bytes() == payload.read_bytes() for name in EXES),
                "normal_copy_incomplete")
    return {
        "case": case[0], "passed": True, "exitCode": child.returncode, "events": events,
        "originalHashesPreserved": True if locked_indexes else None, "freshInput": fresh,
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
        report["success"] = True
    finally:
        result = work / "result.json"
        result.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps({"result": str(result), "success": report["success"],
                          "passedCases": len(report["cases"])}))


if __name__ == "__main__":
    main()
