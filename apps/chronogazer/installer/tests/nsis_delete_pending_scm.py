"""Real SCM #973 proof, ONLY on disposable GitHub-hosted Windows runners.

Never invoke this orchestrator on a developer PC. The dedicated workflow supplies
the explicit opt-in, fresh RUNNER_TEMP root and marker. Importing this module or
calling validate_environment/audit_ppo is pure; no SCM DLL is opened until guard.
No service is started. All proof files are retained; no filesystem tree is removed.
"""

import argparse
import ctypes as C
from ctypes import wintypes as W
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
from nsis_precopy import ROOT, require
from nsis_acl_abort import logical_ppo_lines, log_code

APP_KEY = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\ChronoGazer"
SERVICE_KEY = "SYSTEM\\CurrentControlSet\\Services\\"
OPT_IN = "github-hosted-disposable-only-v1"


def validate_environment(values):
    for key, expected in (("GITHUB_ACTIONS", "true"), ("RUNNER_ENVIRONMENT", "github-hosted"),
                          ("RUNNER_OS", "Windows"), ("BANTO_973_SCM_TEST", OPT_IN)):
        require(values.get(key) == expected, "refused_environment_" + key)


def owned_name(name):
    return name == "ChronoGazer" or re.fullmatch(r"Banto973-[A-Za-z0-9-]{1,80}", name) is not None


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + '\n', encoding="utf-8")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def has_install_protocol(stdout, expected):
    return expected in stdout.splitlines()


def guard():
    validate_environment(os.environ)
    require(os.name == "nt", "Windows_required")
    require(C.windll.shell32.IsUserAnAdmin() != 0, "administrator_token_required")
    temp = Path(os.environ["RUNNER_TEMP"]).resolve(strict=True)
    raw = Path(os.environ["BANTO_973_SCM_ROOT"])
    root = raw.resolve(strict=True)
    require(root != temp and root.is_relative_to(temp) and root.name.startswith("Banto973-"), "invalid_root")
    for ancestor in (raw, *raw.parents):
        require(not ancestor.is_symlink() and not ancestor.is_junction(), "reparse_root")
        if ancestor.resolve() == temp:
            break
    marker = os.environ["GITHUB_RUN_ID"] + '\n' + os.environ["BANTO_973_EXPECTED_SHA"] + '\n'
    require((root / ".banto-973-ci").read_text(encoding="utf-8") == marker, "marker_mismatch")
    return root


class SCM:
    def __init__(self):
        self.api = C.WinDLL("advapi32", use_last_error=True)
        for name, args, result in (
            ("OpenSCManagerW", [W.LPCWSTR, W.LPCWSTR, W.DWORD], W.HANDLE),
            ("OpenServiceW", [W.HANDLE, W.LPCWSTR, W.DWORD], W.HANDLE),
            ("CloseServiceHandle", [W.HANDLE], W.BOOL),
            ("DeleteService", [W.HANDLE], W.BOOL),
            ("QueryServiceStatus", [W.HANDLE, C.c_void_p], W.BOOL),
            ("QueryServiceConfigW", [W.HANDLE, C.c_void_p, W.DWORD, C.POINTER(W.DWORD)], W.BOOL),
        ):
            function = getattr(self.api, name)
            function.argtypes, function.restype = args, result
        self.manager = self.api.OpenSCManagerW(None, None, 1)
        require(bool(self.manager), "OpenSCManager_failed")
        self.holders = []

    def open(self, name, access=1):
        require(owned_name(name), "invalid_service_name")
        handle = self.api.OpenServiceW(self.manager, name, access)
        return handle, 0 if handle else C.get_last_error()

    def close(self, handle):
        require(bool(self.api.CloseServiceHandle(handle)), "CloseServiceHandle_failed")

    def observation(self, name):
        handle, error = self.open(name, 1 | 4)
        if not handle:
            return {"open_error": error}
        try:
            status = (W.DWORD * 7)()
            require(bool(self.api.QueryServiceStatus(handle, status)), "QueryServiceStatus_failed")
            return {"open_error": 0, "state": int(status[1])}
        finally:
            self.close(handle)

    def config_open_observation(self, name):
        handle, error = self.open(name, 1)  # QUERY_CONFIG only, independently observed.
        if handle:
            self.close(handle)
        return {"opened": bool(handle), "open_error": error}

    def command(self, name):
        class Config(C.Structure):
            _fields_ = [("service_type", W.DWORD), ("start", W.DWORD), ("error", W.DWORD),
                        ("binary", W.LPWSTR), ("group", W.LPWSTR), ("tag", W.DWORD),
                        ("dependencies", W.LPWSTR), ("account", W.LPWSTR), ("display", W.LPWSTR)]
        handle, error = self.open(name)
        require(bool(handle), "query_config_open_" + str(error))
        try:
            needed = W.DWORD()
            self.api.QueryServiceConfigW(handle, None, 0, C.byref(needed))
            require(0 < needed.value < 65536, "invalid_config_size")
            buffer = C.create_string_buffer(needed.value)
            require(bool(self.api.QueryServiceConfigW(handle, buffer, len(buffer), C.byref(needed))), "query_config_failed")
            return C.cast(buffer, C.POINTER(Config)).contents.binary
        finally:
            self.close(handle)

    def hold(self, name):
        handle, error = self.open(name)
        require(bool(handle), "holder_open_" + str(error))
        self.holders.append(handle)
        return handle

    def release(self, handle):
        self.close(handle)
        self.holders.remove(handle)

    def close_all(self):
        errors = []
        for handle in list(self.holders):
            try:
                self.release(handle)
            except Exception as error:
                errors.append(str(error))
        require(not errors, "holder_close_failures:" + ','.join(errors))


class Trial:
    def __init__(self, root):
        import winreg
        self.reg = winreg
        self.root = root
        self.proof = root / "proof"
        self.proof.mkdir(exist_ok=True)
        self.probe = root / "probe.exe"
        self.scm = SCM()
        path = root / "ownership.json"
        self.owned = json.loads(path.read_text()) if path.exists() else {}
        self.sequence = 0

    def record_ownership(self, name, created=False):
        require(owned_name(name), "invalid_owned_name")
        self.owned[name] = {"created": created, "executable": str(self.probe)}
        write_json(self.root / "ownership.json", self.owned)
        (self.root / "owned-services.txt").write_text('\n'.join(self.owned) + '\n', encoding="utf-8")

    def registry(self, path):
        try:
            return self.reg.OpenKey(self.reg.HKEY_LOCAL_MACHINE, path, 0, self.reg.KEY_READ | self.reg.KEY_WOW64_64KEY)
        except FileNotFoundError:
            return None

    def deletion(self, name):
        key = self.registry(SERVICE_KEY + name)
        if key is None:
            return {"key_exists": False}
        with key:
            try:
                value, kind = self.reg.QueryValueEx(key, "DeleteFlag")
                return {"key_exists": True, "value": value, "type": kind}
            except FileNotFoundError:
                return {"key_exists": True, "value_missing": True}

    def app_absent(self):
        key = self.registry(APP_KEY)
        if key is None:
            return True
        key.Close()
        return False

    def app_snapshot(self):
        key = self.registry(APP_KEY)
        if key is None:
            return None
        with key:
            return {name: self.reg.QueryValueEx(key, name) for name in ("Banto973Owner", "DisplayName")}

    def command_is_ours(self, name):
        command = self.scm.command(name)
        expected = str(self.probe.resolve())
        # Production passes canonical verbatim paths. Only this fixed dummy command is accepted.
        command = command.replace('\\\\?\\', '')
        return command.casefold() in (f'"{expected}" run-service'.casefold(), f'{expected} run-service'.casefold())

    def absent(self, name):
        return self.scm.observation(name).get("open_error") == 1060 and not self.deletion(name)["key_exists"]

    def wait_absent(self, name):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.absent(name):
                return
            time.sleep(.2)
        raise RuntimeError("service_or_key_did_not_disappear")

    def call(self, args, label, cwd=None):
        self.sequence += 1
        environment = dict(os.environ)
        environment["PATH"] = str(Path(os.environ["SystemRoot"]) / "System32")
        result = subprocess.run([str(arg) for arg in args], cwd=cwd or self.root, env=environment,
                                capture_output=True, text=True, encoding="utf-8", errors="replace",
                                timeout=40, creationflags=subprocess.CREATE_NO_WINDOW)
        (self.proof / f"{self.sequence:03}-{label}.log").write_text(result.stdout + result.stderr, encoding="utf-8")
        return result

    def install(self, name, expected):
        if name not in self.owned:
            require(self.absent(name), "preexisting_service_refused")
            self.record_ownership(name)
        result = self.call([self.probe, "install", name, self.probe], "install")
        require(result.returncode == 0 and has_install_protocol(result.stdout, "install-kind=" + expected), "install_result_" + expected)
        require(self.command_is_ours(name), "installed_command_not_ours")
        require(self.scm.observation(name).get("state") == 1, "dummy_service_not_stopped")
        if expected == "Created":
            self.record_ownership(name, created=True)

    def cleanup(self):
        errors = []
        try:
            self.scm.close_all()
        except Exception as error:
            errors.append({"holders": True, "failure": str(error)})
        for name, evidence in self.owned.items():
            try:
                if self.absent(name):
                    continue
                observation = self.scm.observation(name)
                if observation.get("open_error") == 1072:
                    require(evidence.get("created"), "pending_ownership_unproven")
                else:
                    require(observation.get("state") == 1 and self.command_is_ours(name), "foreign_or_running_service_refused")
                    handle, error = self.scm.open(name, 0x10000)
                    require(bool(handle), "cleanup_open_" + str(error))
                    try:
                        require(bool(self.scm.api.DeleteService(handle)), "cleanup_delete_failed")
                    finally:
                        self.scm.close(handle)
                self.wait_absent(name)
            except Exception as error:
                errors.append({"name": name, "failure": str(error)})
        try:
            if (self.root / "owned-apps-key").exists() and not self.app_absent():
                with self.registry(APP_KEY) as key:
                    require(self.reg.QueryValueEx(key, "Banto973Owner")[0] == self.root.name, "foreign_apps_key_refused")
                self.reg.DeleteKeyEx(self.reg.HKEY_LOCAL_MACHINE, APP_KEY, self.reg.KEY_WOW64_64KEY, 0)
            require(self.app_absent(), "apps_key_remains")
        except Exception as error:
            errors.append({"apps_key": True, "failure": str(error)})
        try:
            self.scm.close(self.scm.manager)
        except Exception as error:
            errors.append({"manager": True, "failure": str(error)})
        result = {"success": not errors, "errors": errors, "only_owned_exact_names": list(self.owned)}
        return result

    def case_a(self):
        name = "Banto973-" + str(uuid.uuid4())
        self.install(name, "Created")
        self.install(name, "AlreadyRegistered")
        holder = self.scm.hold(name)
        try:
            result = self.call([self.probe, "uninstall", name], "uninstall-held")
            deletion = self.deletion(name)
            opened = self.scm.config_open_observation(name)
            observed = {"name": name, "uninstall_exit": result.returncode, "delete_flag": deletion,
                        "open_service_query_config": opened}
            write_json(self.proof / "A-observation.json", observed)
            require(result.returncode == 0, "uninstall_held_failed")
            require(deletion.get("value") == 1 and deletion.get("type") == self.reg.REG_DWORD, "real_DeleteFlag_not_one")
            require(opened.get("open_error") in (0, 1072), "unexpected_pending_open_result")
            result = self.call([self.probe, "install", name, self.probe], "install-pending")
            observed["pending_install_exit"] = result.returncode
            observed["pending_install_stdout"] = result.stdout
            write_json(self.proof / "A-observation.json", observed)
            require(result.returncode != 0 and has_install_protocol(result.stdout, "install-error=DeletePending"), "pending_install_was_not_rejected")
            observed["production_pending_rejected"] = True
        finally:
            self.scm.release(holder)
        self.wait_absent(name)
        self.install(name, "Created")
        require(self.call([self.probe, "uninstall", name], "uninstall-normal").returncode == 0, "normal_uninstall_failed")
        self.wait_absent(name)
        observed["reinstall_created_and_normal_cleanup"] = True
        write_json(self.proof / "A-registration.json", observed)
        return observed


TEMPLATE = r'''Unicode true
RequestExecutionLevel user
SilentInstall silent
SilentUnInstall silent
Name "Disposable real SCM #973"
OutFile "@BUILDER@"
!include "LogicLib.nsh"
!define BANTO_INSTALLER_COMMON_DIR "@COMMON@"
!include "@HOOK@"
@GUARD_VARIANT@
Section
  WriteUninstaller "@UNINSTALLER@"
SectionEnd
Function un.onInit
  SetRegView 64
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
  Delete "$INSTDIR\uninstall.exe"
  DeleteRegKey HKLM "SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\ChronoGazer"
SectionEnd
Function un.onUninstFailed
  @CALLBACK_BEFORE@
  !insertmacro ChronoGazerOnUninstFailed
  @CALLBACK_AFTER@
FunctionEnd
'''


def nsis_words(line):
    """Tokenize NSIS quotes without treating Windows backslashes as shell escapes."""
    words = []
    word = []
    active = False
    quote = None
    index = 0
    while index < len(line):
        char = line[index]
        if line[index:index + 2] == '$\\' and index + 2 < len(line):
            active = True
            word.extend(line[index:index + 3])
            index += 3
            continue
        if quote:
            if char == quote:
                quote = None
            else:
                word.append(char)
        elif char in ('"', "'", '`'):
            active = True
            quote = char
        elif char in (';', '#'):
            break
        elif char.isspace():
            if active:
                words.append(''.join(word))
                word = []
                active = False
        else:
            active = True
            word.append(char)
        index += 1
    require(quote is None, "unclosed_NSIS_quote")
    if active:
        words.append(''.join(word))
    return words


def reject_relative_jumps(line):
    if not re.match(r'^(?:Goto|Call|IntCmp\w*|StrCmp\w*|IfErrors|IfFileExists|MessageBox|'
                    r'IfSilent|IfAbort|IfRebootFlag|IfShellVarContextAll|IsWindow|GetLabelAddress)\b', line, re.I):
        return
    words = nsis_words(line)
    if not words:
        return
    instruction, args = words[0].lower(), words[1:]
    if instruction in ('goto', 'call', 'iferrors', 'ifsilent', 'ifabort', 'ifrebootflag', 'ifshellvarcontextall'):
        targets = args
    elif instruction.startswith(('intcmp', 'strcmp')):
        targets = args[2:]  # Values may legitimately be signed integers.
    elif instruction in ('iffileexists', 'iswindow', 'getlabeladdress'):
        targets = args[1:]
    elif instruction == 'messagebox':
        targets = args[2:]  # Ignore flags and quoted message text.
    else:
        return
    require(not any(re.fullmatch(r'[+-][0-9]+', target) for target in targets),
            "relative_jump_would_change_under_instrumentation")


def audit_ppo(text):
    allowed = {
        'ReadRegStr $1 HKLM "SYSTEM\\CurrentControlSet\\Services\\ChronoGazer" "ImagePath"',
        'ReadRegDWORD $4 HKLM "SYSTEM\\CurrentControlSet\\Services\\ChronoGazer" "DeleteFlag"',
        'DeleteRegKey HKLM "' + APP_KEY + '"',
        r'''ExecWait '"$INSTDIR\chronogazer-elev.exe" service-uninstall' $0''',
        r'''ExecWait '"$INSTDIR\chronogazer-server.exe" uninstall' $0''',
        r'''ExecWait '"$INSTDIR\chronogazer-elev.exe" service-install' $0''',
        "nsExec::ExecToLog 'sc.exe delete ChronoGazer'",
        "nsExec::ExecToStack 'sc start \"ChronoGazer\"'",
        # Shared path helper definitions: metadata-only OPEN_EXISTING (access=0).
        "System::Call 'kernel32::GetFullPathNameW(w r0, i 1024, w .r1, p 0) i .r2'",
        "System::Call 'kernel32::CreateFileW(w r1, i 0, i 7, p 0, i 3, i 33554432, p 0) p .r3'",
        "System::Call 'kernel32::GetFinalPathNameByHandleW(p r3, w .r4, i 1024, i 0) i .r5'",
        "System::Call 'kernel32::CloseHandle(p r3)'",
    }
    allowed.update("nsExec::ExecToStack 'cmd /c sc query \"ChronoGazer\" | findstr /C:\"" + state + "\" >nul'" for state in ("RUNNING", "STOPPED"))
    for line in logical_ppo_lines(text):
        line = line.strip()
        reject_relative_jumps(line)
        if re.match(r'(?:Exec\w*\b|nsExec::|(?:Read|Write|Delete)Reg\w*\b|System::Call)', line):
            require(line in allowed, "PPO_operation_outside_allowlist")


def case_b(trial, makensis, route, running, counterexample=False):
    require(trial.absent("ChronoGazer") and trial.app_absent(), "preexisting_ChronoGazer_or_apps_key")
    case = trial.proof / (f"B-{route}-running{running}" + ("-counterexample" if counterexample else ""))
    case.mkdir()
    install = case / "install"
    install.mkdir()
    (install / "chronogazer.exe").write_bytes(b"inert shell, never executed")
    (install / "data.txt").write_bytes(b"data retained")
    if route in ("server", "elev"):
        shutil.copyfile(trial.probe, install / "chronogazer-server.exe")
    if route == "elev":
        shutil.copyfile(trial.probe, install / "chronogazer-elev.exe")
    trial.install("ChronoGazer", "Created")
    (trial.root / "owned-apps-key").write_text(trial.root.name)
    with trial.reg.CreateKeyEx(trial.reg.HKEY_LOCAL_MACHINE, APP_KEY, 0, trial.reg.KEY_WRITE | trial.reg.KEY_WOW64_64KEY) as key:
        trial.reg.SetValueEx(key, "Banto973Owner", 0, trial.reg.REG_SZ, trial.root.name)
        trial.reg.SetValueEx(key, "DisplayName", 0, trial.reg.REG_SZ, "Disposable #973 fixture")
    app_before = trial.app_snapshot()
    log = case / "events.log"
    uninstaller = install / "uninstall.exe"
    values = {"@BUILDER@": case / "builder.exe", "@COMMON@": ROOT / "installer-common",
              "@HOOK@": ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh",
              "@UNINSTALLER@": uninstaller, "@INSTALL@": install}
    script = TEMPLATE.replace("@RUNNING@", str(running))
    script = script.replace("@GUARD_VARIANT@", "" if not counterexample else
                            "!macroundef RequireChronoGazerServiceDeleteNotPending\n"
                            "!macro RequireChronoGazerServiceDeleteNotPending\n!macroend")
    for value in values.values():
        require(not any(c in str(value) for c in ('"', '$', '\r', '\n')), "NSIS_special_path")
    for marker, value in values.items():
        script = script.replace(marker, str(value))
    for marker, message in (("@DELETE_LOG@", "delete-begins"), ("@CALLBACK_BEFORE@", "callback-before flag=$ChronoGazerWasRunning"),
                            ("@CALLBACK_AFTER@", "callback-after flag=$ChronoGazerWasRunning")):
        script = script.replace(marker, log_code(log, message))
    nsi = case / "harness.nsi"
    nsi.write_text(script, encoding="utf-8")
    expanded = trial.call([makensis, "/INPUTCHARSET", "UTF8", "/OUTPUTCHARSET", "UTF8", "/PPO", nsi], "PPO", case)
    require(expanded.returncode == 0, "PPO_failed")
    (case / "expanded.nsi").write_text(expanded.stdout, encoding="utf-8")
    audit_ppo(expanded.stdout)
    observed = []
    for line in logical_ppo_lines(expanded.stdout):
        stripped = line.strip()
        if stripped.startswith(("ExecWait ", "nsExec::")):
            kind = "register-attempt" if "service-install" in line else "restart-attempt" if "sc start" in line else (
                "unregister-elev" if "service-uninstall" in line else "unregister-server" if '" uninstall' in line else
                "unregister-sc" if "sc.exe delete" in line else "sc-query")
            observed.append(log_code(log, kind))
        if stripped.startswith("MessageBox "):
            kind = "pending-dialog" if "サービスの削除が保留中です" in line else "other-dialog"
            observed.append(log_code(log, kind + " flag=$ChronoGazerWasRunning"))
        observed.append(line)  # Every production primitive remains unchanged.
    instrumented = case / "instrumented.nsi"
    instrumented.write_text('\n'.join(observed) + '\n', encoding="utf-8")
    require(trial.call([makensis, "/INPUTCHARSET", "UTF8", "/V2", instrumented], "compile", case).returncode == 0, "compile_failed")
    require(trial.call([case / "builder.exe"], "builder", case).returncode == 0, "builder_failed")
    before = {path.name: digest(path) for path in install.iterdir() if path.is_file()}
    write_json(case / "before-hashes.json", before)
    holder = trial.scm.hold("ChronoGazer")
    try:
        first_copy = case / "uninstall-pass1.exe"
        shutil.copyfile(uninstaller, first_copy)
        result = trial.call([first_copy, "/S", "_?=" + str(install)], "uninstall-pass1", case)
        events = log.read_text(encoding="utf-16-le").splitlines() if log.exists() else []
        deletion = trial.deletion("ChronoGazer")
        app_after = trial.app_snapshot()
        first = {"exitCode": result.returncode, "events": events, "deleteFlag": deletion,
                 "remaining_files": {path.name: digest(path) for path in install.iterdir() if path.is_file()},
                 "app_before": app_before, "app_after": app_after,
                 "open_service_query_config": trial.scm.config_open_observation("ChronoGazer")}
        write_json(case / "pass1-observation.json", first)
        if counterexample:
            require(result.returncode == 0 and "delete-begins" in events, "old_guard_variant_did_not_delete")
            require(deletion.get("value") == 1 and deletion.get("type") == trial.reg.REG_DWORD, "counterexample_not_really_pending")
            require(app_after is None and not any((install / name).exists() for name in before if name != "data.txt"), "counterexample_cleanup_not_observed")
            require(digest(install / "data.txt") == before["data.txt"], "counterexample_data_changed")
            require(not any(event in events for event in ("restart-attempt", "register-attempt")), "counterexample_restore_attempt")
            return {"guard_disabled": True, "expected_old_failure_observed": True, "pass1": first}
        require(result.returncode == 2, "pending_uninstaller_did_not_abort")
        require(deletion.get("value") == 1 and deletion.get("type") == trial.reg.REG_DWORD, "pending_flag_not_observed")
        require(all(path.is_file() and digest(path) == before[path.name] for path in (install / name for name in before)), "pending_files_changed")
        require(app_before == app_after, "pending_apps_values_changed")
        require("callback-before flag=0" in events and "callback-after flag=0" in events, "pending_callback_flag")
        require(not any(event in events for event in ("delete-begins", "restart-attempt", "register-attempt")), "pending_restore_or_delete")
        require(events.count("pending-dialog flag=0") == 1 and events.index("pending-dialog flag=0") < events.index("callback-before flag=0"), "pending_dialog_order")
        require(events.count("unregister-" + route) == 1, "unregister_route_not_observed")
        first.update({"files_preserved": True, "apps_preserved": True})
    finally:
        trial.scm.release(holder)
    trial.wait_absent("ChronoGazer")
    log.write_bytes(b"")
    second_copy = case / "uninstall-pass2.exe"
    shutil.copyfile(uninstaller, second_copy)
    result = trial.call([second_copy, "/S", "_?=" + str(install)], "uninstall-pass2", case)
    events = log.read_text(encoding="utf-16-le").splitlines() if log.exists() else []
    second = {"exitCode": result.returncode, "events": events,
              "remaining_files": {path.name: digest(path) for path in install.iterdir() if path.is_file()},
              "app_after": trial.app_snapshot(), "service": trial.scm.observation("ChronoGazer"),
              "deleteFlag": trial.deletion("ChronoGazer")}
    write_json(case / "pass2-observation.json", second)
    require(result.returncode == 0 and "delete-begins" in events, "second_pass_failed")
    require(not any(event in events for event in ("restart-attempt", "register-attempt")), "normal_restore_attempt")
    require(not any(event.startswith("unregister-") for event in events), "second_pass_did_not_skip_unregister")
    require(not any((install / name).exists() for name in before if name != "data.txt"), "second_pass_files_remain")
    require(trial.app_absent() and trial.absent("ChronoGazer"), "second_pass_registration_remains")
    require(digest(install / "data.txt") == before["data.txt"], "data_changed")
    second["data_preserved"] = True
    result = {"route": route, "initialWasRunning": running, "pass1": first, "pass2": second}
    write_json(case / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--makensis", type=Path)
    parser.add_argument("--cleanup-only", action="store_true")
    parser.add_argument("--include-counterexample", action="store_true")
    args = parser.parse_args()
    root = guard()  # Must precede any SCM/registry access, including cleanup.
    trial = Trial(root)
    if args.cleanup_only:
        cleanup = trial.cleanup()
        write_json(trial.proof / "cleanup-always.json", cleanup)
        summary_path = trial.proof / "summary.json"
        summary = json.loads(summary_path.read_text()) if summary_path.exists() else {}
        write_json(trial.proof / "overall.json", {"passed": summary.get("status") == "passed" and summary.get("cleanup_success") is True and cleanup["success"],
                   "main_status": summary.get("status", "not_executed"), "always_cleanup_success": cleanup["success"]})
        raise SystemExit(0 if cleanup["success"] else 1)
    report = {"status": "pending", "cases": [], "cleanup_success": False,
              "head_sha": os.environ["BANTO_973_EXPECTED_SHA"], "event_sha": os.environ["GITHUB_SHA"],
              "limits": "stopped dummy services only; no collector/full setup/UI/B3-5. Uninstaller run from outside copy; automatic temp-copy not tested."}
    write_json(trial.proof / "summary.json", report)
    try:
        require(trial.probe.is_file() and args.makensis and args.makensis.is_file(), "missing_inputs")
        require(not trial.owned and trial.app_absent() and trial.absent("ChronoGazer"), "preexisting_state")
        head = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
        require(head == report["head_sha"], "checkout_sha_mismatch")
        files = [Path(__file__), ROOT / "crates/banto-service-control/src/install.rs",
                 ROOT / "crates/banto-service-control/examples/delete_pending_probe.rs",
                 ROOT / "apps/chronogazer/installer/hooks/service-hooks.nsh", ROOT / "installer-common/service-common.nsh"]
        write_json(trial.proof / "provenance.json", {"head_sha": head, "event_sha": report["event_sha"],
                   "probe_sha256": digest(trial.probe), "makensis_sha256": digest(args.makensis),
                   "sources": {str(path.relative_to(ROOT)): digest(path) for path in files}})
        report["A"] = trial.case_a()
        for route in ("elev", "server", "sc"):
            for running in (0, 1):
                report["cases"].append(case_b(trial, args.makensis, route, running))
                print(json.dumps({"case": route, "initialWasRunning": running, "passed": True}), flush=True)
        if args.include_counterexample:
            report["counterexample"] = case_b(trial, args.makensis, "elev", 0, counterexample=True)
            trial.wait_absent("ChronoGazer")
        report["status"] = "passed"
    except Exception as error:
        report["status"], report["failure"] = "failed", str(error)
    finally:
        cleanup = trial.cleanup()
        write_json(trial.proof / "cleanup-main.json", cleanup)
        report["cleanup_success"] = cleanup["success"]
        if not report["cleanup_success"]:
            report["status"] = "failed"
        write_json(trial.proof / "summary.json", report)
    raise SystemExit(0 if report["status"] == "passed" else 1)


if __name__ == "__main__":
    main()
