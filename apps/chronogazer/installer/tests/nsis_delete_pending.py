"""Manual Windows NSIS uninstall-delete-pending regression (not automatic CI).

From repository root, with Windows, Python 3 and an existing NSIS installation:
    python apps/chronogazer/installer/tests/nsis_delete_pending.py --makensis C:/NSIS/makensis.exe
    python apps/chronogazer/installer/tests/nsis_delete_pending.py --counterexample

Real uninstall macros and un.onUninstFailed run in a user/silent uninstaller.
Registry reads and Exec primitives are injected; service restart is stubbed.
PPO must contain no registry or external-execution primitives before execution.
All exe fixtures are inert text, never launched. Apps registration/data are marker
files, not real registry/ProgramData. The builder only writes the test uninstaller.
Artifacts stay in a new repository target UUID. No SCM/HKLM/UAC/product execution.
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
from nsis_acl_abort import logical_ppo_lines, log_code


CASES = [
    (f"pending-{route}-running{running}", route, running, "1", "ok")
    for route in ("elev", "server", "sc") for running in (0, 1)
] + [
    ("pending-imagepath-empty", "skip", 1, "1", "ok"),
    ("normal-zero", "elev", 1, "0", "ok"),
    ("value-missing", "server", 1, "missing-value", "ok"),
    ("key-missing-skip", "skip", 0, "missing-key", "ok"),
    ("read-error-continues", "elev", 1, "read-error", "ok"),
    ("type-error-continues", "elev", 1, "type-error", "ok"),
    ("other-dword-continues", "elev", 1, "2", "ok"),
    ("nonzero-elev", "elev", 1, "1", "nonzero"),
    ("execerror-server", "server", 1, "1", "exec-error"),
    ("nonzero-sc-stopped", "sc", 0, "1", "nonzero"),
]

TEMPLATE = r'''Unicode true
RequestExecutionLevel user
SilentInstall silent
SilentUnInstall silent
Name "Isolated uninstall pending"
OutFile "@BUILDER@"
!include "LogicLib.nsh"
!define BANTO_INSTALLER_COMMON_DIR "@COMMON@"
!include "@HOOK@"
!macroundef StartServiceIfStoppedAndWait
!macro StartServiceIfStoppedAndWait service was_running
  StrCpy $0 "0"
  ${If} ${was_running} == "1"
    @RESTART_LOG@
    StrCpy ${was_running} "0"
  ${EndIf}
!macroend
@COUNTEREXAMPLE@
Section
  WriteUninstaller "@UNINSTALLER@"
SectionEnd
Function un.onInit
  StrCpy $INSTDIR "@INSTALL@"
  StrCpy $ChronoGazerWasRunning "@RUNNING@"
  StrCpy $ChronoGazerShellWasQuit "1"
FunctionEnd
Section "Uninstall"
  !insertmacro RequireChronoGazerExesUnlocked
  !insertmacro UnregisterChronoGazerService
  @DELETE_LOG@
  Delete "$INSTDIR\chronogazer.exe"
  Delete "$INSTDIR\chronogazer-server.exe"
  Delete "$INSTDIR\chronogazer-elev.exe"
  !insertmacro RequireChronoGazerExesDeleted
  Delete "$INSTDIR\uninstaller-record.txt"
  Delete "$INSTDIR\apps-record.txt"
SectionEnd
Function un.onUninstFailed
  @CALLBACK_BEFORE@
  !insertmacro ChronoGazerOnUninstFailed
  @CALLBACK_AFTER@
FunctionEnd
'''


def run_case(work, makensis, case, counterexample=False):
    name, route, running, flag, outcome = case
    directory = work / name
    directory.mkdir()
    install = directory / "install"
    install.mkdir()
    names = ["chronogazer.exe", "uninstaller-record.txt", "apps-record.txt", "data.txt"]
    if route in ("elev", "server"):
        names.append("chronogazer-server.exe")
    if route == "elev":
        names.append("chronogazer-elev.exe")
    for name in names:
        (install / name).write_bytes(("inert fixture " + name).encode())
    before = {name: file_hash(install / name) for name in names}
    log = directory / "events.log"
    uninstaller = install / "test-uninstall.exe"
    values = {"@COMMON@": ROOT / "installer-common", "@HOOK@": ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh",
              "@BUILDER@": directory / "builder.exe", "@UNINSTALLER@": uninstaller,
              "@INSTALL@": install}
    for value in values.values():
        require(not any(c in str(value) for c in ('"', '$', '\r', '\n')), "NSIS_special_path_character")
    script = TEMPLATE.replace("@RUNNING@", str(running))
    for marker, text in (("@RESTART_LOG@", "restart"), ("@DELETE_LOG@", "delete-begins"),
                         ("@CALLBACK_BEFORE@", "callback-before flag=$ChronoGazerWasRunning"),
                         ("@CALLBACK_AFTER@", "callback-after flag=$ChronoGazerWasRunning")):
        script = script.replace(marker, log_code(log, text))
    script = script.replace("@COUNTEREXAMPLE@", "" if not counterexample else
                            "!macroundef RequireChronoGazerServiceDeleteNotPending\n"
                            "!macro RequireChronoGazerServiceDeleteNotPending\n!macroend")
    for marker, value in values.items():
        script = script.replace(marker, str(value))
    nsi = directory / "harness.nsi"
    nsi.write_text(script, encoding="utf-8")
    expanded = run_tool([makensis, "/INPUTCHARSET", "UTF8", "/OUTPUTCHARSET", "UTF8", "/PPO", nsi], 20)
    require(expanded.returncode == 0, "PPO_failed")
    (directory / "expanded.nsi").write_text(expanded.stdout, encoding="utf-8")
    observed = []
    injected_reads = 0
    for line in logical_ppo_lines(expanded.stdout):
        stripped = line.strip()
        if stripped.startswith("ReadRegStr "):
            require('"ImagePath"' in line, "unknown_registry_read")
            observed += ['StrCpy $1 "' + ("" if route == "skip" else "registered") + '"']
            continue
        if stripped.startswith("ReadRegDWORD "):
            require('"DeleteFlag"' in line, "unknown_registry_value")
            injected_reads += 1
            observed.append(log_code(log, "deleteflag-read=" + flag))
            observed += ['StrCpy $4 "' + (flag if flag in ("0", "1", "2") else "") + '"',
                         "ClearErrors" if flag in ("0", "1", "2") else "SetErrors"]
            continue
        if stripped.startswith(("ExecWait ", "nsExec::ExecToLog ")):
            is_sc = stripped.startswith("nsExec::")
            operation = "register" if "service-install" in line else "unregister"
            operation_route = "sc" if is_sc else "elev" if "chronogazer-elev.exe" in line else "server"
            observed.append(log_code(log, operation + "-" + operation_route))
            code = "17" if outcome == "nonzero" else "0"
            observed += [f'Push "{code}"' if is_sc else f'StrCpy $0 "{code}"',
                         "SetErrors" if outcome == "exec-error" else "ClearErrors"]
            continue
        if stripped.startswith("MessageBox "):
            kind = "pending-dialog" if "サービスの削除が保留中です" in line else "failure-dialog" if "登録解除に失敗しました" in line else "shell-dialog"
            observed.append(log_code(log, kind + " flag=$ChronoGazerWasRunning"))
        observed.append(line)
    require(injected_reads == (0 if counterexample else 1), "deleteflag_read_injection_count")
    text = '\n'.join(observed) + '\n'
    require(not re.search(r'^\s*(?:Exec(?:Wait|Shell(?:Wait)?)?\b|nsExec::|(?:Read|Write|Delete)Reg\w*\b)', text, re.M),
            "external_execution_or_registry_primitive_survived")
    require(not re.search(r'(?:advapi32|OpenSCManager|OpenService|ChangeServiceConfig)', text, re.I), "SCM_operation_survived")
    instrumented = directory / "instrumented.nsi"
    instrumented.write_text(text, encoding="utf-8")
    compile_result = run_tool([makensis, "/INPUTCHARSET", "UTF8", "/V2", instrumented], 30)
    (directory / "compile.log").write_text(compile_result.stdout + compile_result.stderr, encoding="utf-8")
    require(compile_result.returncode == 0, "compile_failed")
    built = run_tool([directory / "builder.exe"], 20)
    require(built.returncode == 0 and uninstaller.is_file(), "test_uninstaller_not_created")
    uninstall_hash = file_hash(uninstaller)
    child = run_tool([uninstaller, "/S", "_?=" + str(install)], 20)
    events = log.read_text(encoding="utf-16-le").splitlines()
    (directory / "observation.json").write_text(json.dumps({
        "case": case[0], "exitCode": child.returncode, "events": events,
        "remainingFixtures": [name for name in names if (install / name).exists()],
        "uninstallerPreserved": uninstaller.is_file() and file_hash(uninstaller) == uninstall_hash,
    }, indent=2), encoding="utf-8")
    pending = flag == "1" and outcome == "ok"
    failed = pending or outcome != "ok"
    require(child.returncode == (2 if failed else 0), "unexpected_exit")
    require(not any(e.startswith("register-") for e in events), "registration_attempt")
    if failed:
        require("delete-begins" not in events, "must_abort_before_delete")
        require(all((install / name).is_file() and file_hash(install / name) == digest for name, digest in before.items()), "fixture_changed_on_abort")
        require(file_hash(uninstaller) == uninstall_hash, "uninstaller_changed_on_abort")
        require("callback-before flag=0" in events and "callback-after flag=0" in events, "callback_flag")
        require(events.count("restart") == (0 if pending else running), "restart_count")
        dialog = "pending-dialog flag=0" if pending else "failure-dialog flag=0"
        require(dialog in events and events.index(dialog) < events.index("callback-before flag=0"), "dialog_callback_order")
        if not pending and running:
            require(events.index("restart") < events.index(dialog), "restart_before_dialog")
    else:
        require("restart" not in events, "unexpected_restart_on_success")
        require("delete-begins" in events and not any(e.startswith("callback-") for e in events), "normal_path_did_not_continue")
        require(not any((install / name).exists() for name in names if name != "data.txt"), "normal_fixture_deletion_failed")
    require(file_hash(install / "data.txt") == before["data.txt"], "data_changed")
    if route == "skip":
        require(not any(e.startswith("unregister-") for e in events), "skip_executed_unregister")
    else:
        require(events.count("unregister-" + route) == 1, "wrong_unregister_route")
    return {"case": case[0], "passed": True, "events": events, "exitCode": child.returncode,
            "pending": pending, "contentsPreservedOnAbort": failed}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--makensis", type=Path)
    parser.add_argument("--counterexample", action="store_true", help="Disable only the new guard in an untracked harness; expect failure")
    args = parser.parse_args()
    require(os.name == "nt", "Windows_required")
    makensis = args.makensis or Path(os.environ["LOCALAPPDATA"]) / "tauri/NSIS/makensis.exe"
    require(makensis.is_file(), "NSIS_missing")
    work = ROOT / "target/973-delete-pending-isolated" / ("nsis-" + str(uuid.uuid4()))
    work.mkdir(parents=True)
    report = {"success": False, "counterexample": args.counterexample, "cases": [],
              "limits": "ReadReg/Exec/service stubs; Apps/data markers; no real SCM/HKLM/UI/B3-5 validation"}
    try:
        for case in CASES[:1] if args.counterexample else CASES:
            report["cases"].append(run_case(work, makensis, case, args.counterexample))
            print(json.dumps({"case": case[0], "passed": True}), flush=True)
        report["success"] = True
    except Exception as error:
        report["failure"] = str(error)
        raise
    finally:
        result = work / "result.json"
        result.write_text(json.dumps(report, indent=2), encoding="utf-8")
        print(json.dumps({"result": str(result), "success": report["success"], "cases": len(report["cases"])}), flush=True)


if __name__ == "__main__":
    main()
