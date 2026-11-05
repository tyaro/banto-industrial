"""Manual Windows NSIS ACL-abort regression; no administrator/SCM/real ACL operations.

From repository root:
    python apps/chronogazer/installer/tests/nsis_acl_abort.py
    python apps/chronogazer/installer/tests/nsis_acl_abort.py --makensis C:/NSIS/makensis.exe

Real Reset/Abort and Chrono callback are expanded, then logs are injected before
MessageBox and after FindClose. Only icacls/service operations are stubbed. Safe
text fixtures, subdirectory, and explicit enumeration/reparse injections cover
failure branches. The user/silent harness executes; product setup/exes never do.
All artifacts remain in a fresh repository target UUID. CI does not run this test.
"""

import argparse
import json
import os
from pathlib import Path
import re
import sys
import uuid

sys.dont_write_bytecode = True
from nsis_precopy import ROOT, require, run_tool, file_hash


CASES = (
    ("icacls-root-restart-success", "root", 1, 0),
    ("icacls-root-restart-failed", "root", 1, 1),
    ("icacls-root-already-stopped", "root", 0, 0),
    ("icacls-file", "file", 1, 0),
    ("subdirectory", "dir", 1, 0),
    ("enumeration-failed", "enum", 1, 0),
    ("root-reparse-injected", "reparse", 1, 0),
    ("hub-default-no-callback", "hub", 0, 0),
    ("acl-pass-no-callback", "ok", 1, 0),
)

TEMPLATE = r'''Unicode true
RequestExecutionLevel user
SilentInstall silent
Name "Isolated ACL abort"
OutFile "@OUT@"
!include "LogicLib.nsh"
!define BANTO_INSTALLER_COMMON_DIR "@COMMON@"
@PRELUDE@
!macroundef IcaclsResetInstallDirEntry
!macro IcaclsResetInstallDirEntry
  StrCpy $0 "0"
  StrCpy $1 "stub icacls output"
  @ICACLS_FAIL@
!macroend
@SERVICE_STUB@
Section
  StrCpy $INSTDIR "@INSTALL@"
  StrCpy $ChronoGazerWasRunning "@RUNNING@"
  StrCpy $ChronoGazerShellWasQuit "0"
  !insertmacro ResetInstallDirAcl
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWriteUTF16LE $R9 "copy-begins flag=$ChronoGazerWasRunning$\r$\n"
  FileClose $R9
SectionEnd
Function .onInstFailed
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWriteUTF16LE $R9 "onfailed-before flag=$ChronoGazerWasRunning$\r$\n"
  FileClose $R9
  @ONFAILED@
  FileOpen $R9 "@LOG@" a
  FileSeek $R9 0 END
  FileWriteUTF16LE $R9 "onfailed-after flag=$ChronoGazerWasRunning$\r$\n"
  FileClose $R9
FunctionEnd
'''


def log_code(log, text):
    return ('FileOpen $R9 "' + str(log) + '" a\nFileSeek $R9 0 END\n'
            'FileWriteUTF16LE $R9 "' + text + '$\\r$\\n"\nFileClose $R9\n')


def logical_ppo_lines(text):
    # makensis PPO expands string CR/LF escapes to physical newlines. Restore
    # escapes inside quoted arguments before adding logs/recompiling the result.
    out = []
    quote = None
    for index, char in enumerate(text):
        escaped = index >= 2 and text[index - 2:index] == "$\\"
        if quote is not None and char == "\n":
            out.append(r"$\r$\n")
        else:
            out.append(char)
            if char == quote and not escaped:
                quote = None
            elif quote is None and char in ("\"", "'"):
                quote = char
    require(quote is None, "unclosed_PPO_argument")
    return "".join(out).splitlines()


def run_case(work, makensis, case):
    name, mode, running, restart_result = case
    directory = work / name
    directory.mkdir()
    install = directory / "install"
    install.mkdir()
    fixture = install / "fixture.txt"
    fixture.write_bytes(b"inert ACL fixture\n")
    if mode == "dir":
        (install / "nested").mkdir()
    before = file_hash(fixture)
    log = directory / "events.log"
    hub = mode == "hub"
    prelude = ('!include "' + str(ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh") + '"')
    if hub:
        prelude = r'''!define INSTALL_DIR_COMMON_LOG_PREFIX "hub-default-test"
!define INSTALL_DIR_COMMON_PRODUCT "Hub default test"
!define INSTALL_DIR_COMMON_DEFAULT_DIR "$PROGRAMFILES64\BantoHub"
!define INSTALL_DIR_COMMON_SERVICE_NOUN "service"
!define INSTALL_DIR_COMMON_ACL_ABORT_NOTE ""
!include "@COMMON@\install-dir-common.nsh"
Var ChronoGazerWasRunning
Var ChronoGazerShellWasQuit'''
    fail = ''
    if mode in ("root", "hub"):
        fail = 'StrCpy $0 "77"'
    elif mode == "file":
        fail = '${If} $4 != "$INSTDIR"\nStrCpy $0 "77"\n${EndIf}'
    service = ''
    if not hub:
        service = r'''!macroundef StartServiceIfStoppedAndWait
!macro StartServiceIfStoppedAndWait service flag
  StrCpy $0 "0"
  ${If} ${flag} == "1"
    @RESTART_LOG@
    StrCpy ${flag} "0"
    StrCpy $0 "@RESTART_RESULT@"
    StrCpy $1 "sentinel-output"
    StrCpy $4 "sentinel-target"
    Push "unconsumed service stdout"
  ${EndIf}
!macroend'''.replace("@RESTART_LOG@", log_code(log, "restart"))
    values = {
        "@OUT@": directory / "harness.exe", "@COMMON@": ROOT / "installer-common",
        "@INSTALL@": install, "@LOG@": log,
    }
    for value in values.values():
        require(not any(char in str(value) for char in ('"', '$', '\r', '\n')),
                "NSIS_special_path_character")
    script = TEMPLATE.replace("@PRELUDE@", prelude).replace("@ICACLS_FAIL@", fail)
    script = script.replace("@SERVICE_STUB@", service).replace("@ONFAILED@", '' if hub else
                                                                '!insertmacro ChronoGazerOnInstFailed')
    script = script.replace("@RUNNING@", str(running)).replace("@RESTART_RESULT@", str(restart_result))
    for marker, value in values.items():
        script = script.replace(marker, str(value))
    nsi = directory / "harness.nsi"
    nsi.write_text(script, encoding="utf-8")
    expanded = run_tool([makensis, "/INPUTCHARSET", "UTF8", "/OUTPUTCHARSET", "UTF8", "/PPO", nsi], 20)
    require(expanded.returncode == 0, "NSIS_preprocess_failed")
    require(not re.search(r'^\s*(?:Exec(?:Wait|Shell(?:Wait)?)?\b|nsExec::)',
                          expanded.stdout, re.MULTILINE | re.IGNORECASE), "external_process_in_harness")
    require(not re.search(r'advapi32::|OpenSCManager|OpenService', expanded.stdout, re.IGNORECASE),
            "SCM_or_ACL_API_in_harness")
    (directory / "expanded-original.nsi").write_text(expanded.stdout, encoding="utf-8")
    instrumented = []
    main_dialogs = 0
    injected = False
    for line in logical_ppo_lines(expanded.stdout):
        if mode == "enum" and line.strip().startswith('FindFirst $2 $3 '):
            # Inject a failing primitive without opening any enumeration handle.
            instrumented += ['StrCpy $2 ""', 'StrCpy $3 ""']
            injected = True
            continue
        # Observe actual runtime dialog order, not just instruction text order.
        if line.lstrip().startswith("MessageBox "):
            is_main = "所有者・アクセス権を既定に戻せませんでした" in line
            kind = "main-dialog" if is_main else "other-dialog"
            instrumented.append(log_code(log, kind + " code=$0 target=$4"))
            if is_main:
                main_dialogs += 1
                message = re.search(r'MessageBox \S+ "(.*)" /SD IDOK', line)
                require(message is not None, "unknown_main_dialog_form")
                instrumented.append(log_code(log, "main-text=" + message.group(1)))
        instrumented.append(line)
        if line.strip() == 'FindClose $2':
            instrumented.append(log_code(log, "find-close"))
        if mode == "reparse" and not injected and "GetFileAttributesW(w r4)" in line:
            instrumented.append('IntOp $5 $5 | 1024')
            injected = True
    require(main_dialogs >= 1, "main_dialog_not_instrumented")
    if mode in ("enum", "reparse"):
        require(injected, "failure_injection_not_applied")
    observed_nsi = directory / "instrumented.nsi"
    observed_nsi.write_text('\n'.join(instrumented) + '\n', encoding="utf-8")
    compiled = run_tool([makensis, "/INPUTCHARSET", "UTF8", "/V2", observed_nsi], 30)
    (directory / "compile.log").write_text(compiled.stdout + compiled.stderr, encoding="utf-8")
    require(compiled.returncode == 0, "NSIS_compile_failed")
    child = run_tool([directory / "harness.exe"], 20)
    events = log.read_text(encoding="utf-16-le").splitlines()
    if mode == "ok":
        require(child.returncode == 0 and "copy-begins flag=1" in events, "ACL_success_must_continue")
        require("restart" not in events and not any(
            event.startswith(("main-dialog", "other-dialog", "onfailed-")) for event in events),
            "callback_or_dialog_on_success")
        require(file_hash(fixture) == before, "fixture_contents_changed")
        return {"case": name, "passed": True, "events": events, "exitCode": child.returncode,
                "contentsUnchanged": True, "optionalCallback": True}
    require(child.returncode == 2 and not any(event.startswith("copy-begins") for event in events), "must_abort_before_File")
    require(file_hash(fixture) == before, "fixture_contents_changed")
    main_events = [event for event in events if event.startswith("main-dialog ")]
    require(len(main_events) == 1, "main_dialog_count")
    main_at = events.index(main_events[0])
    require(events.count("restart") == running, "restart_count")
    require("onfailed-before flag=0" in events and "onfailed-after flag=0" in events,
            "onfailed_flag_or_double_restart")
    require(main_at < events.index("onfailed-before flag=0"), "main_dialog_must_precede_abort_callback")
    if running:
        require(events.index("restart") < main_at, "restart_must_precede_main_dialog")
    if mode in ("file", "dir"):
        require("find-close" in events, "FindClose_not_observed")
        before_restore = events.index("restart") if running else main_at
        require(events.index("find-close") < before_restore, "FindClose_must_precede_restore")
    target = str(fixture) if mode == "file" else str(install / "nested") if mode == "dir" else str(install)
    require("target=" + target in main_events[0], "original_target_lost")
    if mode in ("root", "file", "hub"):
        require("code=77 " in main_events[0], "original_icacls_code_lost")
        require(any("icacls" in event and "77" in event for event in events), "original_reason_lost")
    require(not hub or "restart" not in events, "default_callback_changed")
    return {"case": name, "passed": True, "events": events, "exitCode": child.returncode,
            "contentsUnchanged": True, "optionalCallback": not hub}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--makensis", type=Path, default=None)
    args = parser.parse_args()
    require(os.name == "nt", "Windows_required")
    makensis = args.makensis or Path(os.environ["LOCALAPPDATA"]) / "tauri/NSIS/makensis.exe"
    require(makensis.is_file(), "NSIS_missing_use_makensis_option")
    work = ROOT / "target/973-acl-abort-isolated" / ("manual-" + str(uuid.uuid4()))
    work.mkdir(parents=True)
    report = {"success": False, "realResetAbortAndCallback": True, "dialogAndFindCloseInstrumented": True,
              "cases": [], "limits": "icacls/service are stubs; enum/reparse are injected. No real ACL/SCM/UI."}
    try:
        for case in CASES:
            report["cases"].append(run_case(work, makensis, case))
            print(json.dumps({"case": case[0], "passed": True}), flush=True)
        report["success"] = True
    finally:
        result = work / "result.json"
        result.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps({"result": str(result), "success": report["success"],
                          "passedCases": len(report["cases"])}))


if __name__ == "__main__":
    main()
