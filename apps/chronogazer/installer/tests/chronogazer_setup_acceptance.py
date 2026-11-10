#!/usr/bin/env python3
"""ChronoGazer インストーラ（本物の setup.exe）の実 SCM 受入試験（#973 残件A、silent の通常系＋故障系）。

!!! 開発 PC では絶対に実行しない !!!
このスクリプトは実 `C:\\Program Files\\ChronoGazer`・`C:\\ProgramData\\ChronoGazer`・
ChronoGazer サービスを書き換え、アンインストールまで行う。
使い捨ての GitHub-hosted Windows ランナーでだけ
`.github/workflows/installer-chronogazer-acceptance.yml` から走らせる。ローカルで走らせて
よいのは `test_chronogazer_setup_acceptance.py`（純粋部分のユニットテスト）だけ。

フェーズ（各 `phase_*` は独立の関数。`phase_visible_*` の 4 つ以外はすべて silent の setup / uninstall）:
  1 拒否 / 2 新規 / 3 起動と種まき（DB の番兵表＋番兵ファイル） /
  4 ACL を弱めて稼働中の上書き / 5 停止中の上書き（AUTO_START を保つ） /
  6 稼働中のアンインストール / 7 再インストール
故障系: 8 使用中 exe でのインストール中止 / 9 ACL の段階（サブフォルダ・直下ジャンクション）での中止 /
  10 事前確認（サービス登録・前のインストールの別の場所）での中止 / 11 使用中 exe でのアンインストール中止 /
  12 サービス解除失敗 / 13 稼働中のサービスの service-install 失敗 / 14 破損 exe での再開失敗 /
  15 DeleteFlag 保留（拒否、解消、完了まで）
可視 UI（setup を /P で実画面に出す）: 14b シェル 90 秒の最初の問いの [再試行]→[キャンセル] /
  14c 再確認の問い（サービス停止の後）の [キャンセル]（止めたサービスの再開） /
  14d 最初の問いの [再試行] で続行して上書きまで通る道 /
  14e アンインストールの再確認の問い（uninstall.exe /P、サービス停止の後）の [キャンセル]（止めたサービスの再開）
  故障系は、中止後に止めたサービスが再開され（10 は止める前に断る）、ファイル・登録・データが
  変わらず、setup の再実行で回復できることを確かめる。アンインストールの故障系は `_?=` を付けて
  同じ本体をその場で走らせ、完了と終了コードを得る（フェーズ 15 の最後だけ本物の呼び方）。
各フェーズの観測を証拠 JSON（proof/evidence.json）に残し、期待と違えばフェーズ名と
観測を出して失敗する。

対象外（このスクリプトは確かめない）: 上の 4 つ以外の可視 UI（WebView2 の後始末・
再確認の [再試行]）・サービス停止の timeout・`File` の書込み途中の失敗・消し残り
（RequireChronoGazerExesDeleted）・実 collector・#392 B3-5。上の 4 つ以外は silent なので MessageBox は未確認。

標準ライブラリのみ。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import shutil
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

import chronogazer_setup_visible as vis

GUARD_ENV = "BANTO_973_CG_SETUP_TEST"
GUARD_VALUE = "github-hosted-disposable-only-v1"
ROOT_ENV = "BANTO_973_CG_ROOT"  # 証拠ルート（マーカー .banto-973-ci を持つ）
DIST_ENV = "BANTO_973_CG_DIST"  # build-release-chronogazer.ps1 の -OutDir

INSTDIR = Path(r"C:\Program Files\ChronoGazer")
ELSEWHERE = Path(r"C:\ChronoGazerElsewhere")
DATA_ROOT = Path(os.environ.get("ProgramData", r"C:\ProgramData")) / "ChronoGazer"
DB_FILE_NAME = "chronogazer.sqlite3"  # apps/chronogazer/core/src/paths.rs の DB_FILE_NAME
DB_PATH = DATA_ROOT / DB_FILE_NAME
ENDPOINT_FILE = DATA_ROOT / "run" / "endpoint.json"  # DataLayout::endpoint_file
SENTINEL = DATA_ROOT / "banto-973-sentinel.txt"
SERVICE = "ChronoGazer"
OPERATORS_GROUP = "ChronoGazer Operators"
SERVER_EXE = "chronogazer-server.exe"
ELEV_EXE = "chronogazer-elev.exe"
SHELL_EXE = "chronogazer.exe"
EXES = (SHELL_EXE, SERVER_EXE, ELEV_EXE)
SENTINEL_TABLE = "banto_973_sentinel"
USERS_SID = "*S-1-5-32-545"
HOST_PATH = "/api/host"  # apps/chronogazer/core/src/service_probe.rs の HOST_PATH
# 製品が停止の待ち切り（30 秒 + 追加 60 秒）で中止する道は最短でも 90 秒掛かる。
# それより短ければ、停止の待ち切りではない理由で中止したと分かる。
STOP_TIMEOUT_ABORT_SECONDS = 90.0


class PhaseFailure(Exception):
    def __init__(self, phase: str, message: str, observation=None):
        super().__init__(f"[{phase}] {message}")
        self.phase = phase
        self.message = message
        self.observation = observation


# ---------------------------------------------------------------- 純粋部分

def guard_problems(env, platform: str, is_admin: bool) -> list[str]:
    """実行してよい環境かの確かめ。問題の一覧（空なら実行可）。"""
    problems = []
    if env.get("GITHUB_ACTIONS") != "true":
        problems.append("GITHUB_ACTIONS != true")
    if env.get("RUNNER_ENVIRONMENT") != "github-hosted":
        problems.append("RUNNER_ENVIRONMENT != github-hosted")
    if platform != "win32":
        problems.append("not Windows")
    if env.get(GUARD_ENV) != GUARD_VALUE:
        problems.append(f"{GUARD_ENV} missing or wrong")
    if not is_admin:
        problems.append("not elevated")
    return problems


def root_marker_problem(env) -> str | None:
    """証拠ルートの目印（<root>/.banto-973-ci がファイル）の確かめ。問題なら理由、無ければ None。"""
    raw = env.get(ROOT_ENV)
    if not raw:
        return f"{ROOT_ENV} is not set"
    if not (Path(raw) / ".banto-973-ci").is_file():
        return "proof root marker missing"
    return None


def parse_sc_query(text: str) -> dict:
    """`sc queryex` の出力から state（RUNNING 等）と pid。サービス無しは state=None。"""
    m = re.search(r"STATE\s*:\s*\d+\s+([A-Z_]+)", text)
    pid = re.search(r"PID\s*:\s*(\d+)", text)
    return {"state": m.group(1) if m else None, "pid": int(pid.group(1)) if pid else None}


def parse_sc_qc(text: str) -> dict:
    """`sc qc` の出力から起動種別（DEMAND_START 等）と BINARY_PATH_NAME。"""
    st = re.search(r"START_TYPE\s*:\s*\d+\s+([A-Z_]+)", text)
    bp = re.search(r"BINARY_PATH_NAME\s*:\s*(.+)", text)
    return {"start_type": st.group(1) if st else None, "image_path": bp.group(1).strip() if bp else None}


def image_path_exe(image_path: str) -> str:
    """ImagePath から exe のパス（クォートあり・なしの両方を受ける。引数は落とす）。

    windows-service crate は空白の無い exe のパスをクォートせずに登録する
    （docs/implementation-checklist.md §3）が、空白のあるパスは引数付きで渡るので両方受ける。
    """
    p = image_path.strip()
    if p.startswith('"'):
        end = p.find('"', 1)
        return p[1:end] if end > 0 else p[1:]
    m = re.match(r"(.+?\.exe)(\s|$)", p, re.IGNORECASE)
    return m.group(1) if m else p


def image_path_is(image_path: str, expected_exe) -> bool:
    """ImagePath の exe が expected_exe と大文字小文字を除き完全一致するか（#1050 の採用パスの確認）。"""
    got = os.path.normcase(os.path.normpath(image_path_exe(image_path)))
    want = os.path.normcase(os.path.normpath(str(expected_exe)))
    return got == want


def icacls_explicit_users_aces(text: str) -> list[str]:
    """`icacls` の出力から、Users の継承でない（明示の）ACE の行。"""
    out = []
    for line in text.splitlines():
        if re.search(r"(BUILTIN\\Users|\bUsers):\(", line, re.IGNORECASE) and "(I)" not in line:
            out.append(line.strip())
    return out


def parse_owner_output(returncode: int, stdout: str, stderr: str) -> str:
    """`Get-Acl` の結果を検証する。失敗・空の観測は例外にし、「Administrators でない」と誤判定させない。"""
    if returncode != 0 or not stdout.strip():
        raise RuntimeError(f"owner 取得に失敗（returncode={returncode}）: {stderr[-300:]}")
    return stdout.strip()


def parse_icacls_output(returncode: int, stdout: str, stderr: str) -> str:
    """`icacls <path>` の出力。失敗・空は例外。"""
    if returncode != 0 or not stdout.strip():
        raise RuntimeError(f"icacls に失敗（returncode={returncode}）: {stderr[-300:]}")
    return stdout


def find_setup(dist: Path) -> tuple[Path, str]:
    cands = sorted(dist.glob("ChronoGazer_*_x64-setup.exe"))
    if len(cands) != 1:
        raise PhaseFailure("prepare", f"expected exactly one setup in {dist}", [c.name for c in cands])
    m = re.match(r"ChronoGazer_(.+)_x64-setup\.exe$", cands[0].name)
    return cands[0], m.group(1)


def dist_exe_name(stem: str, version: str) -> str:
    """scripts/build-release-chronogazer.ps1 の $copies と同じ名前。"""
    return f"{stem}-v{version}-windows-x86_64.exe"


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree_hashes(root: Path) -> dict[str, str]:
    return {str(p.relative_to(root)): sha256(p) for p in sorted(root.rglob("*")) if p.is_file()}


def parse_endpoint(raw: str) -> dict:
    """`run\\endpoint.json`（EndpointInfo、camelCase）。port と scope が使えなければ例外（空の観測を通さない）。"""
    d = json.loads(raw)
    port = d.get("port")
    scope = d.get("scope")
    if not isinstance(port, int) or isinstance(port, bool) or not (0 < port < 65536):
        raise RuntimeError(f"endpoint.json の port が不正: {port!r}")
    if not isinstance(scope, str) or not scope:
        raise RuntimeError(f"endpoint.json の scope が不正: {scope!r}")
    return {"port": port, "scope": scope, "pid": d.get("pid"), "host_kind": d.get("hostKind")}


def host_healthy(status: int, body, endpoint_scope: str) -> bool:
    """service_probe.rs の classify_host_response と同じ条件（Healthy のときだけ True）。"""
    if status != 200 or not isinstance(body, dict):
        return False
    return (
        body.get("product") == "chronogazer"
        and body.get("hostKind") == "service"
        and isinstance(body.get("scope"), str)
        and body.get("scope") == endpoint_scope
    )


def endpoint_pid_matches(endpoint_pid, service_pid) -> bool:
    """endpoint.json の pid が、いま SCM が報告しているサービスの PID と一致するか。

    scope は DB のパス由来で再起動の前後で同じなので、これが無いと、STOPPED の報告の後も
    port を握ったままの前のプロセスを「いま起動したプロセス」と区別できない。
    """
    for pid in (endpoint_pid, service_pid):
        if not isinstance(pid, int) or isinstance(pid, bool) or pid <= 0:
            return False
    return endpoint_pid == service_pid


def host_request_headers() -> dict:
    # service_probe.rs の fetch_host と同じ（CSRF のヘッダー X-Banto-Client が要る）。
    return {"X-Banto-Client": "banto", "Accept": "application/json"}


def write_db_sentinel(db: Path, value: str) -> None:
    """DB に試験用の表を足して値を 1 行入れ、確実に commit して閉じる。"""
    con = sqlite3.connect(str(db), timeout=30)
    try:
        con.execute(f"CREATE TABLE {SENTINEL_TABLE}(v TEXT)")
        con.execute(f"INSERT INTO {SENTINEL_TABLE}(v) VALUES (?)", (value,))
        con.commit()
    finally:
        con.close()


def read_db_sentinel(db: Path) -> list[str]:
    """読み取り専用で番兵の値を読む。表が無い・行が無いは例外（空の観測を「一致」と誤判定させない）。"""
    uri = "file:" + str(db).replace("\\", "/") + "?mode=ro"
    con = sqlite3.connect(uri, uri=True, timeout=30)
    try:
        rows = [r[0] for r in con.execute(f"SELECT v FROM {SENTINEL_TABLE}")]
    finally:
        con.close()
    if not rows:
        raise RuntimeError("番兵の表に行が無い")
    return rows


def parse_sc_sdshow(returncode: int, stdout: str, stderr: str) -> str:
    """`sc sdshow` の SDDL 行（`D:` か `O:` で始まる最初の行）。失敗・空は例外（空の観測を「変わっていない」と誤判定させない）。"""
    if returncode == 0:
        for line in stdout.splitlines():
            t = line.strip()
            if t.startswith(("D:", "O:")):
                return t
    raise RuntimeError(f"sc sdshow に失敗（returncode={returncode}）: {(stdout + stderr)[-300:]}")


_SDDL_DACL_HEAD = re.compile(r"(?:O:(?:S-[\d-]+|[A-Z]{2}))?(?:G:(?:S-[\d-]+|[A-Z]{2}))?D:(?:P|AI|AR)*")


def sddl_with_deny(sddl: str, right: str) -> str:
    """DACL の先頭に「Administrators に right（SD=DELETE、RC=READ_CONTROL 等）拒否」の ACE を入れる。O:/G: の中の文字や SACL は壊さない。"""
    sddl = sddl.strip()
    m = _SDDL_DACL_HEAD.match(sddl)
    if not m:
        raise RuntimeError(f"DACL が無い SDDL: {sddl!r}")
    return sddl[: m.end()] + f"(D;;{right};;;BA)" + sddl[m.end() :]


def uninstall_in_place_command(instdir, mode_flag: str = "/S") -> str:
    """`_?=` 付きでその場で走らせる呼び方。`_?=` は最後の引数でクォートしない（NSIS の規則）ので文字列で渡す。"""
    return f'"{instdir}\\uninstall.exe" {mode_flag} _?={instdir}'


def count_service_start_failures(returncode: int, stdout: str, stderr: str, needle: str = "chronogazer") -> int:
    """`wevtutil qe System ... /f:text`（SCM の 7000 = 起動失敗）の出力のうち、needle を含む事象の数。

    出力が空で returncode 0 は 0 件（正常）。returncode が非 0 は例外（取得失敗を 0 件と誤判定させない）。
    """
    if returncode != 0:
        raise RuntimeError(f"wevtutil に失敗（returncode={returncode}）: {(stdout + stderr)[-300:]}")
    blocks = re.split(r"(?m)^Event\[\d+\]", stdout)[1:]
    return sum(1 for b in blocks if needle.lower() in b.lower())


# ---------------------------------------------------------------- OS 操作

EVIDENCE: dict = {"phases": {}}
PROOF_DIR: Path | None = None


def save_evidence() -> None:
    if PROOF_DIR is not None:
        (PROOF_DIR / "evidence.json").write_text(
            json.dumps(EVIDENCE, ensure_ascii=False, indent=2), encoding="utf-8"
        )


def obs(phase: str, key: str, value) -> None:
    EVIDENCE["phases"].setdefault(phase, {})[key] = value
    save_evidence()


def run(args, timeout=300, check=False) -> subprocess.CompletedProcess:
    r = subprocess.run(
        args, capture_output=True, text=True, timeout=timeout, encoding="utf-8", errors="replace"
    )
    if check and r.returncode != 0:
        raise RuntimeError(f"{args} -> {r.returncode}: {r.stdout}{r.stderr}")
    return r


def is_admin() -> bool:
    import ctypes

    try:
        return bool(ctypes.windll.shell32.IsUserAnAdmin())
    except Exception:
        return False


def sc_state(name: str = SERVICE) -> dict:
    r = run(["sc", "queryex", name])
    d = parse_sc_query(r.stdout)
    d["exit"] = r.returncode
    return d


def sc_config(name: str = SERVICE) -> dict:
    return parse_sc_qc(run(["sc", "qc", name]).stdout)


def service_key_exists(name: str = SERVICE) -> bool:
    import winreg

    try:
        winreg.CloseKey(
            winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, rf"SYSTEM\CurrentControlSet\Services\{name}")
        )
        return True
    except FileNotFoundError:
        return False


SERVICE_MARKER = "Banto973Marker"


def set_service_marker(name: str = SERVICE) -> None:
    """サービスのキーに目印の値を置く（この run が作ったサービスの印）。解除と再登録でキーが作り直されると消える。"""
    run(
        ["reg", "add", rf"HKLM\SYSTEM\CurrentControlSet\Services\{name}", "/v", SERVICE_MARKER, "/t", "REG_SZ", "/d", "1", "/f"],
        check=True,
    )


def service_marker_present(name: str = SERVICE) -> bool:
    import winreg

    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, rf"SYSTEM\CurrentControlSet\Services\{name}") as k:
            winreg.QueryValueEx(k, SERVICE_MARKER)
        return True
    except FileNotFoundError:
        return False


def read_delete_flag(name: str = SERVICE) -> int | None:
    import winreg

    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, rf"SYSTEM\CurrentControlSet\Services\{name}") as k:
            value, typ = winreg.QueryValueEx(k, "DeleteFlag")
    except FileNotFoundError:
        return None
    if typ != winreg.REG_DWORD:
        raise RuntimeError(f"DeleteFlag の型が REG_DWORD でない: {typ}")
    return int(value)


def service_start_failure_count() -> int:
    """System ログの SCM 7000（起動失敗）のうち ChronoGazer のものの数。"""
    r = run(
        [
            "wevtutil", "qe", "System",
            "/q:*[System[Provider[@Name='Service Control Manager'] and (EventID=7000)]]",
            "/f:text",
        ]
    )
    return count_service_start_failures(r.returncode, r.stdout, r.stderr)


def wait_until(pred, timeout: float, interval: float = 1.0):
    end = time.monotonic() + timeout
    last = None
    while time.monotonic() < end:
        last = pred()
        if last:
            return last
        time.sleep(interval)
    return None


def wait_state(name: str, want: str, timeout: float = 90) -> dict:
    last = {}

    def p():
        nonlocal last
        last = sc_state(name)
        return last if last["state"] == want else None

    r = wait_until(p, timeout)
    if r is None:
        raise RuntimeError(f"{name} did not reach {want}: last={last}")
    return r


def http_get_json(url: str, timeout: float = 5):
    req = urllib.request.Request(url, headers=host_request_headers())
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw or b"{}")
        except ValueError:
            return e.code, {"raw": raw[:200].decode("utf-8", "replace")}


def wait_healthy(timeout: float = 90) -> dict:
    """Healthy を待つ: endpoint.json の port へ `GET /api/host`（X-Banto-Client 付き、シェルの
    起動判定と同じ要求）が 200 かつ product/hostKind/scope が一致し、endpoint.json の pid が
    SCM の PID と一致すること。シェルが見る pid の生存確認と自分の DB の scope は見ない。"""
    last: list = []

    def p():
        try:
            ep = parse_endpoint(ENDPOINT_FILE.read_text(encoding="utf-8"))
            code, body = http_get_json(f"http://127.0.0.1:{ep['port']}{HOST_PATH}")
            service_pid = sc_state()["pid"]
            last[:] = [ep, code, body, service_pid]
            healthy = host_healthy(code, body, ep["scope"]) and endpoint_pid_matches(ep["pid"], service_pid)
            return {"endpoint": ep, "body": body, "service_pid": service_pid} if healthy else None
        except Exception as e:  # noqa: BLE001 - 立ち上がるまでの待ち
            last[:] = [repr(e)]
            return None

    r = wait_until(p, timeout, 1.0)
    if r is None:
        raise RuntimeError(f"/api/host never became healthy: {last}")
    return r


def process_running(image: str) -> bool:
    r = run(["tasklist", "/FI", f"IMAGENAME eq {image}", "/FO", "CSV", "/NH"])
    return image.lower() in r.stdout.lower()


def run_setup(setup: Path, extra: list[str], phase: str, label: str) -> int:
    # /D= は最後の引数・クォートなし（NSIS の規則）。
    r = run([str(setup), "/S", *extra], timeout=900)
    obs(phase, f"{label}_exit", r.returncode)
    obs(phase, f"{label}_stdout_tail", r.stdout[-600:])
    obs(phase, f"{label}_stderr_tail", r.stderr[-600:])
    return r.returncode


def owner_of(path: Path) -> str:
    r = run(["pwsh", "-NoProfile", "-Command", f"(Get-Acl -LiteralPath '{path}').Owner"])
    return parse_owner_output(r.returncode, r.stdout, r.stderr)


def icacls_output(path) -> str:
    r = run(["icacls", str(path)])
    return parse_icacls_output(r.returncode, r.stdout, r.stderr)


def acl_snapshot(path) -> dict:
    return {"owner": owner_of(path), "icacls": icacls_output(path)}


def uninstall_registration() -> list[dict]:
    """Apps（Uninstall キー）のうち INSTDIR を指す登録（キー名と表示名）。"""
    import winreg

    needle = str(INSTDIR).lower()
    found = {}
    for view in (winreg.KEY_WOW64_64KEY, winreg.KEY_WOW64_32KEY):
        try:
            base = winreg.OpenKey(
                winreg.HKEY_LOCAL_MACHINE,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
                0,
                winreg.KEY_READ | view,
            )
        except FileNotFoundError:
            continue
        i = 0
        while True:
            try:
                sub = winreg.EnumKey(base, i)
            except OSError:
                break
            i += 1
            try:
                with winreg.OpenKey(base, sub) as k:
                    us = winreg.QueryValueEx(k, "UninstallString")[0]
                    if needle not in str(us).lower():
                        continue
                    try:
                        display = str(winreg.QueryValueEx(k, "DisplayName")[0])
                    except OSError:
                        display = None
                found[sub] = display
            except OSError:
                continue
    return [{"key": k, "display_name": v} for k, v in sorted(found.items())]


def operators_group_exists() -> bool:
    return run(["net", "localgroup", OPERATORS_GROUP]).returncode == 0


def _win_apis():
    """kernel32 / advapi32。HANDLE は 64bit 幅で明示する（切り詰めない）。"""
    import ctypes
    from ctypes import wintypes

    k = ctypes.WinDLL("kernel32", use_last_error=True)
    a = ctypes.WinDLL("advapi32", use_last_error=True)
    k.CreateFileW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
        wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE,
    ]
    k.CreateFileW.restype = wintypes.HANDLE
    k.CloseHandle.argtypes = [wintypes.HANDLE]
    k.CloseHandle.restype = wintypes.BOOL
    a.OpenSCManagerW.argtypes = [wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD]
    a.OpenSCManagerW.restype = wintypes.HANDLE
    a.OpenServiceW.argtypes = [wintypes.HANDLE, wintypes.LPCWSTR, wintypes.DWORD]
    a.OpenServiceW.restype = wintypes.HANDLE
    a.CloseServiceHandle.argtypes = [wintypes.HANDLE]
    a.CloseServiceHandle.restype = wintypes.BOOL
    return ctypes, k, a


_INVALID_HANDLE = 0xFFFFFFFFFFFFFFFF


class ExclusiveFileLock:
    """READ 共有だけで開いて保持し、書込みで開けない状態にする（使用中の exe を作る）。"""

    def __init__(self, path):
        self.path = str(path)
        self.handle = None

    def __enter__(self):
        ctypes, k, _ = _win_apis()
        h = k.CreateFileW(self.path, 0x80000000, 1, None, 3, 0, None)
        if not h or h == _INVALID_HANDLE:
            raise RuntimeError(f"ロック用の CreateFileW に失敗: {ctypes.get_last_error()}")
        self.handle = h
        w = k.CreateFileW(self.path, 0x40000000, 7, None, 3, 0, None)
        if w and w != _INVALID_HANDLE:
            k.CloseHandle(w)
            self.__exit__()
            raise RuntimeError("ロックが効いていない（書込みで開けた）")
        err = ctypes.get_last_error()
        if err != 32:
            self.__exit__()
            raise RuntimeError(f"書込みで開けない理由が共有違反でない: {err}")
        return self

    def __exit__(self, *exc):
        if self.handle:
            _, k, _ = _win_apis()
            k.CloseHandle(self.handle)
            self.handle = None
        return False


class ServiceHandleHold:
    """サービスのハンドルを保持する。DeleteService は 0 で返るが DeleteFlag が残る（削除保留）状態を作る。"""

    def __init__(self, name: str):
        self.name = name
        self.scm = None
        self.svc = None

    def __enter__(self):
        ctypes, _, a = _win_apis()
        self.scm = a.OpenSCManagerW(None, None, 1)
        if not self.scm:
            raise RuntimeError(f"OpenSCManagerW に失敗: {ctypes.get_last_error()}")
        self.svc = a.OpenServiceW(self.scm, self.name, 4)  # QUERY_STATUS
        if not self.svc:
            err = ctypes.get_last_error()
            self.__exit__()
            raise RuntimeError(f"OpenServiceW({self.name}) に失敗: {err}")
        return self

    def __exit__(self, *exc):
        _, _, a = _win_apis()
        if self.svc:
            a.CloseServiceHandle(self.svc)
            self.svc = None
        if self.scm:
            a.CloseServiceHandle(self.scm)
            self.scm = None
        return False


def _sc_sdshow(name: str) -> str:
    r = run(["sc", "sdshow", name])
    return parse_sc_sdshow(r.returncode, r.stdout, r.stderr)


class DenyServiceRight:
    """サービスの SDDL に Administrators の right 拒否を入れる。抜けるとき元に戻す。

    SD（DELETE）は DeleteService を、RC（READ_CONTROL）は grant-service-acl の読み取りを失敗させる。
    RC を拒否している間は `sc sdshow` も失敗するので、with の中で sdshow を呼ばないこと。
    """

    def __init__(self, right: str, name: str = SERVICE):
        self.right = right
        self.name = name
        self.original = None

    def __enter__(self):
        self.original = _sc_sdshow(self.name)
        run(["sc", "sdset", self.name, sddl_with_deny(self.original, self.right)], check=True)
        if self.right == "RC":
            r = run(["sc", "sdshow", self.name])
            if r.returncode == 0:
                run(["sc", "sdset", self.name, self.original])
                raise RuntimeError(f"READ_CONTROL 拒否が効いていない（sdshow が読めた）: {r.stdout}")
        else:
            now = _sc_sdshow(self.name)
            if f"(D;;{self.right};;;BA)" not in now:
                run(["sc", "sdset", self.name, self.original])
                raise RuntimeError(f"{self.right} 拒否が入っていない: {now}")
        return self

    def __exit__(self, exc_type, exc, tb):
        if self.original is not None:
            original = self.original
            r = run(["sc", "sdset", self.name, original])
            self.original = None
            if r.returncode != 0:
                message = f"sc sdset の戻しに失敗: {self.name} -> {r.returncode}: {r.stdout}{r.stderr}"
                print(message)
                if exc_type is None:
                    raise RuntimeError(message)
            elif exc_type is None:
                now = _sc_sdshow(self.name)
                if now != original:
                    raise RuntimeError(f"SDDL が元に戻っていない: {now} != {original}")
        return False


def tree_snapshot(root) -> dict[str, str]:
    """中身のハッシュ。空は例外（観測が空のときに通さない）。再解析ポイントの中は辿らないよう、置く前に取り、外した後に比べる。"""
    snap = tree_hashes(Path(root))
    if not snap:
        raise RuntimeError(f"tree_hashes が空: {root}")
    return snap


def timed(p: str, label: str, fn):
    """fn() の所要秒を証拠に残して (結果, 秒) を返す。"""
    start = time.monotonic()
    result = fn()
    elapsed = round(time.monotonic() - start, 1)
    obs(p, f"{label}_elapsed_seconds", elapsed)
    return result, elapsed


def expect(phase: str, cond: bool, message: str, observation=None) -> None:
    if not cond:
        raise PhaseFailure(phase, message, observation)


def expect_not_stop_timeout(p: str, elapsed: float) -> None:
    """同じ観測になる「停止の待ち切りでの中止」と見分ける。"""
    expect(
        p,
        elapsed < STOP_TIMEOUT_ABORT_SECONDS,
        "abort took as long as the stop-timeout path; the intended fault may not have been reached",
        elapsed,
    )


def weakened_outside(p: str, name: str):
    """RUNNER_TEMP 配下に ACL・owner を弱めた外部フォルダと keep.txt を作り、(dir, targets, before) を返す。"""
    outside = Path(os.environ["RUNNER_TEMP"]) / name
    outside.mkdir(parents=True, exist_ok=True)
    keep = outside / "keep.txt"
    keep.write_text("keep", encoding="utf-8")
    targets = [outside, keep]
    for t in targets:
        run(["icacls", str(t), "/grant", f"{USERS_SID}:(M)"], check=True)
        run(["icacls", str(t), "/setowner", os.environ["USERNAME"]], check=True)
        snap = acl_snapshot(t)
        weak = icacls_explicit_users_aces(snap["icacls"])
        obs(p, f"outside_weakened_{name}_{t.name}", {"explicit_users": weak, "owner": snap["owner"]})
        # 前提が崩れていると、setowner Administrators・reset への退行を検出できない
        expect(p, bool(weak), f"could not weaken ACL on {t}", weak)
        expect(p, "Administrators" not in snap["owner"], f"owner of {t} is already Administrators", snap["owner"])
    before = {"hashes": tree_hashes(outside), "acl": {str(t): acl_snapshot(t) for t in targets}}
    if not before["hashes"]:
        raise RuntimeError(f"tree_hashes が空: {outside}")
    return outside, targets, before


def expect_outside_unchanged(p: str, targets, before) -> None:
    """targets[0] は外部フォルダ。中身のハッシュと所有者・ACL の不変を確かめる。"""
    after = {"hashes": tree_hashes(targets[0]), "acl": {str(t): acl_snapshot(t) for t in targets}}
    obs(p, "outside_before", before)
    obs(p, "outside_after", after)
    expect(p, before["hashes"] == after["hashes"], "outside folder content changed", after["hashes"])
    expect(p, before["acl"] == after["acl"], "outside owner/ACL changed", after["acl"])


def expect_registered(phase: str, want_state: str, want_start: str = "DEMAND_START") -> None:
    st = sc_state()
    qc = sc_config()
    server = INSTDIR / SERVER_EXE
    obs(phase, "service_state", st)
    obs(phase, "service_config", qc)
    expect(phase, st["state"] == want_state, f"{SERVICE} not {want_state}", st)
    expect(phase, qc["start_type"] == want_start, f"{SERVICE} not {want_start}", qc)
    expect(
        phase,
        bool(qc["image_path"]) and image_path_is(qc["image_path"], server),
        f"{SERVICE} ImagePath is not {server}",
        qc,
    )


def expect_no_shell(phase: str) -> None:
    running = process_running(SHELL_EXE)
    obs(phase, "shell_running", running)
    expect(phase, not running, f"{SHELL_EXE} is running (the setup must not leave the shell up)")


def expect_apps_registration(phase: str) -> None:
    reg = uninstall_registration()
    obs(phase, "apps_registration", reg)
    expect(phase, bool(reg), "Apps registration not found")
    names = [str(r["display_name"]) for r in reg]
    expect(phase, any("chronogazer" in n.lower() for n in names), "Apps display name is not ChronoGazer", reg)


# ---------------------------------------------------------------- フェーズ

class Ctx:
    def __init__(self, setup: Path, version: str, dist: Path):
        self.setup = setup
        self.version = version
        self.dist = dist
        self.hashes: dict[str, str] = {}
        self.db_sentinel = secrets.token_hex(16)
        self.pid_before: int | None = None


def phase_reject(c: Ctx) -> None:
    p = "1_reject"
    expect(p, not INSTDIR.exists() and not ELSEWHERE.exists(), "targets pre-exist")
    rc = run_setup(c.setup, [f"/D={ELSEWHERE}"], p, "elsewhere")
    expect(p, rc != 0, "setup with foreign /D= must fail", rc)
    expect(p, not ELSEWHERE.exists(), "foreign dir was created")
    expect(p, not INSTDIR.exists(), "INSTDIR was created")
    expect(p, sc_state()["state"] is None, f"{SERVICE} registered after refusal")

    outside, targets, before = weakened_outside(p, "cg-junction-outside")
    run(["cmd", "/c", "mklink", "/J", str(INSTDIR), str(outside)], check=True)
    try:
        rc = run_setup(c.setup, [], p, "junction")
        expect(p, rc != 0, "setup over a junction must fail", rc)
        expect_outside_unchanged(p, targets, before)
        expect(p, sc_state()["state"] is None, f"{SERVICE} registered after junction refusal")
    finally:
        if INSTDIR.exists() or os.path.lexists(INSTDIR):
            os.rmdir(INSTDIR)  # ジャンクションだけ消す（中身は触らない）
    expect(p, not os.path.lexists(INSTDIR), "junction not removed")


def phase_fresh(c: Ctx) -> None:
    p = "2_fresh"
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "fresh setup failed", rc)
    if service_key_exists():
        set_service_marker()
    for exe in (*EXES, "uninstall.exe"):
        expect(p, (INSTDIR / exe).is_file(), f"{exe} missing")
    expect_registered(p, "STOPPED")
    expect_apps_registration(p)
    group = operators_group_exists()
    obs(p, "operators_group_exists", group)
    expect(p, group, f"local group {OPERATORS_GROUP!r} missing")
    expect_no_shell(p)


def phase_start_seed(c: Ctx) -> None:
    p = "3_start_seed"
    run(["sc", "start", SERVICE])
    obs(p, "running", wait_state(SERVICE, "RUNNING"))
    health = wait_healthy()
    obs(p, "health_before", health)
    # 番兵の DB 書込みは、サービスが DB を開いていない間に行う。
    run(["sc", "stop", SERVICE])
    wait_state(SERVICE, "STOPPED")
    expect(p, DB_PATH.is_file(), "chronogazer.sqlite3 missing after the first start", str(DB_PATH))
    write_db_sentinel(DB_PATH, c.db_sentinel)
    expect(p, read_db_sentinel(DB_PATH) == [c.db_sentinel], "sentinel row not readable after write")
    SENTINEL.parent.mkdir(parents=True, exist_ok=True)
    SENTINEL.write_text("banto-973 sentinel\n", encoding="utf-8")
    c.hashes["sentinel"] = sha256(SENTINEL)
    obs(p, "hashes", dict(c.hashes))
    obs(p, "db_path", str(DB_PATH))
    run(["sc", "start", SERVICE])
    wait_state(SERVICE, "RUNNING")
    obs(p, "health_after", wait_healthy())
    _expect_data_intact(p, c)


def _expect_data_intact(p: str, c: Ctx) -> None:
    expect(p, SENTINEL.is_file() and sha256(SENTINEL) == c.hashes["sentinel"], "sentinel file changed")
    expect(p, DB_PATH.is_file(), "chronogazer.sqlite3 missing", str(DB_PATH))
    try:
        rows = read_db_sentinel(DB_PATH)
    except Exception as e:  # noqa: BLE001
        raise PhaseFailure(p, "DB sentinel unreadable", repr(e)) from e
    expect(p, rows == [c.db_sentinel], "DB sentinel value changed", rows)


def phase_acl_overwrite(c: Ctx) -> None:
    p = "4_acl_overwrite"
    for target in (INSTDIR, INSTDIR / SERVER_EXE):
        run(["icacls", str(target), "/grant", f"{USERS_SID}:(M)"], check=True)
        weak = icacls_explicit_users_aces(icacls_output(target))
        obs(p, f"weakened_{target.name}", weak)
        expect(p, bool(weak), f"could not weaken ACL on {target}")
        run(["icacls", str(target), "/setowner", os.environ["USERNAME"]], check=True)
        weak_owner = owner_of(target)
        obs(p, f"weakened_owner_{target.name}", weak_owner)
        expect(p, "Administrators" not in weak_owner, f"could not change owner of {target}", weak_owner)
    want_elev = sha256(c.dist / dist_exe_name("chronogazer-elev", c.version))
    want_server = sha256(c.dist / dist_exe_name("chronogazer-server", c.version))
    with open(INSTDIR / ELEV_EXE, "ab") as f:
        f.write(b"\0")
    expect(p, sha256(INSTDIR / ELEV_EXE) != want_elev, "elev exe tamper did not change hash")
    c.pid_before = sc_state()["pid"]
    obs(p, "pid_before", c.pid_before)
    expect(p, sc_state()["state"] == "RUNNING" and bool(c.pid_before), f"{SERVICE} not running before overwrite")
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "overwrite with running service failed", rc)
    st = wait_state(SERVICE, "RUNNING")
    obs(p, "pid_after", st["pid"])
    expect(p, st["pid"] != c.pid_before, f"{SERVICE} PID unchanged (was not restarted)", st)
    health = wait_healthy()
    obs(p, "health", health)
    expect_registered(p, "RUNNING")
    for target in (INSTDIR, INSTDIR / SERVER_EXE):
        aces = icacls_explicit_users_aces(icacls_output(target))
        owner = owner_of(target)
        obs(p, f"after_{target.name}", {"explicit_users": aces, "owner": owner})
        expect(p, not aces, f"Users explicit ACE remains on {target}", aces)
        expect(p, "Administrators" in owner, f"owner of {target} is not Administrators", owner)
    # chronogazer.exe は tauri-bundler が書き換えるので dist とは比べない（checklist §2-14）。
    got = {ELEV_EXE: sha256(INSTDIR / ELEV_EXE), SERVER_EXE: sha256(INSTDIR / SERVER_EXE)}
    obs(p, "exe_hash", {"dist": {ELEV_EXE: want_elev, SERVER_EXE: want_server}, "installed": got})
    expect(p, got[ELEV_EXE] == want_elev, "installed elev exe differs from dist")
    expect(p, got[SERVER_EXE] == want_server, "installed server exe differs from dist")
    _expect_data_intact(p, c)
    expect_no_shell(p)


def phase_stopped_overwrite(c: Ctx) -> None:
    p = "5_stopped_overwrite"
    run(["sc", "stop", SERVICE])
    wait_state(SERVICE, "STOPPED")
    run(["sc", "config", SERVICE, "start=", "auto"], check=True)
    expect(p, sc_config()["start_type"] == "AUTO_START", "could not set AUTO_START", sc_config())
    try:
        rc = run_setup(c.setup, [], p, "setup")
        expect(p, rc == 0, "overwrite with stopped service failed", rc)
        # service-install は登録済みなら作り直さない（hooks）ので、起動種別は AUTO_START のまま。
        expect_registered(p, "STOPPED", want_start="AUTO_START")
        _expect_data_intact(p, c)
        expect_no_shell(p)
    finally:
        run(["sc", "config", SERVICE, "start=", "demand"])
    expect(p, sc_config()["start_type"] == "DEMAND_START", "could not restore DEMAND_START", sc_config())


def _wait_uninstall_done(p: str) -> None:
    """親が戻った後の子（temp 本体）の完了待ち。サービス・exe・Apps 登録がすべて消えること。"""

    def done():
        key = service_key_exists() or sc_state()["state"] is not None
        files = [e for e in (*EXES, "uninstall.exe") if (INSTDIR / e).exists()]
        reg = uninstall_registration()
        return {"ok": True} if not key and not files and not reg else None

    ok = wait_until(done, 240, 2.0)
    obs(p, "service_key_exists", service_key_exists())
    obs(p, "files_left", [e for e in (*EXES, "uninstall.exe") if (INSTDIR / e).exists()])
    expect(p, ok is not None, "uninstall did not finish (service/files/registration remain)")
    reg = uninstall_registration()
    obs(p, "apps_registration", reg)
    expect(p, not reg, "Apps registration remains", reg)


def phase_uninstall_running(c: Ctx) -> None:
    p = "6_uninstall_running"
    run(["sc", "start", SERVICE])
    wait_state(SERVICE, "RUNNING")
    wait_healthy()
    # `_?=` を付けない実際の呼び方。親は子（temp にコピーされた本体）を残してすぐ戻る。
    r = run([str(INSTDIR / "uninstall.exe"), "/S"], timeout=600)
    obs(p, "uninstall_parent_exit", r.returncode)
    _wait_uninstall_done(p)
    # Operators グループの有無は記録だけ（期待にしない）。
    obs(p, "operators_group_exists", operators_group_exists())
    expect(p, DB_PATH.is_file(), "sqlite was removed")
    _expect_data_intact(p, c)


def phase_reinstall(c: Ctx) -> None:
    p = "7_reinstall"
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "reinstall failed", rc)
    if service_key_exists():
        set_service_marker()
    expect_registered(p, "STOPPED")
    expect_apps_registration(p)
    expect_no_shell(p)
    run(["sc", "start", SERVICE])
    wait_state(SERVICE, "RUNNING")
    obs(p, "health", wait_healthy())
    _expect_data_intact(p, c)


# ---------------------------------------------------------------- 故障系

CORRUPT_SERVER = b"banto-973 corrupt: not a PE\n"
ELSEWHERE_IMAGE_PATH = rf'"{ELSEWHERE}\{SERVER_EXE}" run-service'
SERVICE_KEY = rf"SYSTEM\CurrentControlSet\Services\{SERVICE}"
UNINSTALL_KEY = rf"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{SERVICE}"


def _dist_exe(c: Ctx, stem: str) -> Path:
    return c.dist / dist_exe_name(stem, c.version)


def _ensure_running(p: str) -> int:
    if sc_state()["state"] != "RUNNING":
        run(["sc", "start", SERVICE])
        wait_state(SERVICE, "RUNNING")
    health = wait_healthy()
    pid = sc_state()["pid"]
    obs(p, "pid_before", pid)
    obs(p, "health_before", health)
    expect(p, bool(pid), f"{SERVICE} has no PID")
    return pid


def _expect_restarted(p: str, before_pid: int) -> None:
    st = wait_state(SERVICE, "RUNNING")
    health = wait_healthy()
    obs(p, "pid_after", st["pid"])
    obs(p, "health_after", health)
    expect(p, st["pid"] != before_pid, f"{SERVICE} PID unchanged (stopped service was not resumed)", st)


def _expect_untouched(p: str, before_pid: int) -> None:
    st = sc_state()
    obs(p, "service_untouched", st)
    expect(p, st["state"] == "RUNNING" and st["pid"] == before_pid, f"{SERVICE} was stopped before the precheck", st)


def _tamper_elev(p: str, c: Ctx) -> str:
    """同じ版を上書きするのでハッシュ比較では上書きを見分けられない。末尾の \\0 が残っているかで見る。"""
    with open(INSTDIR / ELEV_EXE, "ab") as f:
        f.write(b"\0")
    got = sha256(INSTDIR / ELEV_EXE)
    obs(p, "elev_tampered_hash", got)
    expect(p, got != sha256(_dist_exe(c, "chronogazer-elev")), "elev exe tamper did not change hash")
    return got


def _expect_elev_still(p: str, tampered: str) -> None:
    got = sha256(INSTDIR / ELEV_EXE)
    obs(p, "elev_hash_after_abort", {"tampered": tampered, "now": got})
    expect(p, got == tampered, "elev exe was overwritten despite the abort")


def _expect_exes_match_dist(p: str, c: Ctx) -> None:
    # chronogazer.exe は tauri-bundler が書き換えるので dist とは比べない（checklist §2-14）。
    want = {ELEV_EXE: sha256(_dist_exe(c, "chronogazer-elev")), SERVER_EXE: sha256(_dist_exe(c, "chronogazer-server"))}
    got = {ELEV_EXE: sha256(INSTDIR / ELEV_EXE), SERVER_EXE: sha256(INSTDIR / SERVER_EXE)}
    obs(p, "exe_hash", {"dist": want, "installed": got})
    for name in want:
        expect(p, want[name] == got[name], f"installed {name} differs from dist")


def _uninstall_in_place(p: str, label: str) -> int:
    r = run(uninstall_in_place_command(INSTDIR), timeout=600)
    obs(p, f"{label}_exit", r.returncode)
    obs(p, f"{label}_stderr_tail", r.stderr[-300:])
    return r.returncode


def _expect_still_installed(p: str, c: Ctx, before: dict) -> None:
    after = tree_snapshot(INSTDIR)
    reg = uninstall_registration()
    obs(p, "instdir_changed", sorted(k for k in set(before) | set(after) if before.get(k) != after.get(k)))
    obs(p, "apps_registration", reg)
    expect(p, after == before, "INSTDIR content changed by the aborted uninstall")
    expect(p, bool(reg), "Apps registration removed by the aborted uninstall")
    expect(p, DB_PATH.is_file(), "sqlite was removed")
    _expect_data_intact(p, c)


def _expect_not_delete_pending(p: str) -> None:
    flag = read_delete_flag()
    obs(p, "delete_flag", flag)
    expect(p, flag in (None, 0), f"{SERVICE} has DeleteFlag", flag)


def _recover_by_setup(p: str, c: Ctx) -> None:
    rc = run_setup(c.setup, [], p, "recover")
    expect(p, rc == 0, "recovery setup failed", rc)
    wait_state(SERVICE, "RUNNING")
    obs(p, "recover_health", wait_healthy())
    expect_registered(f"{p}_recover", "RUNNING")
    _expect_exes_match_dist(p, c)
    _expect_not_delete_pending(p)
    _expect_data_intact(p, c)
    expect_no_shell(p)


def phase_install_locked_exe(c: Ctx) -> None:
    p = "8_install_locked_exe"
    pid = _ensure_running(p)
    tampered = _tamper_elev(p, c)
    before = tree_snapshot(INSTDIR)
    with ExclusiveFileLock(INSTDIR / ELEV_EXE):
        rc, elapsed = timed(p, "locked_setup", lambda: run_setup(c.setup, [], p, "locked_setup"))
    expect(p, rc != 0, "setup over a locked exe must fail", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_restarted(p, pid)
    expect_registered(p, "RUNNING")
    _expect_elev_still(p, tampered)
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the aborted setup")
    _expect_data_intact(p, c)
    expect_no_shell(p)
    _recover_by_setup(p, c)


def phase_install_acl_refused(c: Ctx) -> None:
    p = "9_install_acl_refused"
    # ChronoGazer の ACL の段階はサービスを止めた後で、中止の前に再開を試みる（PID が変わる）。
    pid = _ensure_running(p)
    tampered = _tamper_elev(p, c)
    before = tree_snapshot(INSTDIR)
    # サブフォルダ
    subdir = INSTDIR / "banto-973-subdir"
    subdir.mkdir()
    (subdir / "keep.txt").write_text("keep", encoding="utf-8")
    try:
        rc = run_setup(c.setup, [], p, "subdir_setup")
        expect(p, rc != 0, "setup with a subfolder must fail", rc)
        _expect_restarted(p, pid)
        expect(p, subdir.is_dir() and (subdir / "keep.txt").is_file(), "subfolder was removed by the setup")
    finally:
        shutil.rmtree(subdir, ignore_errors=True)
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the refused setup (subfolder)")
    _expect_elev_still(p, tampered)
    # 直下のジャンクション
    pid = sc_state()["pid"]
    obs(p, "pid_before_junction", pid)
    outside, targets, outside_before = weakened_outside(p, "cg-nested-junction-outside")
    link = INSTDIR / "banto-973-link"
    run(["cmd", "/c", "mklink", "/J", str(link), str(outside)], check=True)
    try:
        rc = run_setup(c.setup, [], p, "junction_setup")
        expect(p, rc != 0, "setup with a nested junction must fail", rc)
        _expect_restarted(p, pid)
        expect_outside_unchanged(p, targets, outside_before)
    finally:
        if os.path.lexists(link):
            os.rmdir(link)  # ジャンクションだけ消す（中身は触らない）
    expect(p, not os.path.lexists(link), "nested junction not removed")
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the refused setup (junction)")
    _expect_elev_still(p, tampered)
    _recover_by_setup(p, c)


def _read_image_path():
    import winreg

    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, SERVICE_KEY, 0, winreg.KEY_READ) as k:
        value, typ = winreg.QueryValueEx(k, "ImagePath")
    return str(value), typ


def _write_image_path(value: str, typ: int) -> None:
    import winreg

    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, SERVICE_KEY, 0, winreg.KEY_SET_VALUE) as k:
        winreg.SetValueEx(k, "ImagePath", 0, typ, value)


def _uninstall_views_with_location():
    """InstallLocation を持つ Uninstall キーのビュー（KEY_WOW64_64KEY / 32KEY）と (値, 型) の一覧。"""
    import winreg

    found = []
    for name, view in (("64", winreg.KEY_WOW64_64KEY), ("32", winreg.KEY_WOW64_32KEY)):
        try:
            with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, UNINSTALL_KEY, 0, winreg.KEY_READ | view) as k:
                value, typ = winreg.QueryValueEx(k, "InstallLocation")
        except FileNotFoundError:
            continue
        found.append((name, view, str(value), typ))
    return found


def _write_install_location(view: int, value: str, typ: int) -> None:
    import winreg

    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, UNINSTALL_KEY, 0, winreg.KEY_SET_VALUE | view) as k:
        winreg.SetValueEx(k, "InstallLocation", 0, typ, value)


def _read_install_location(view: int):
    import winreg

    with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, UNINSTALL_KEY, 0, winreg.KEY_READ | view) as k:
        value, typ = winreg.QueryValueEx(k, "InstallLocation")
    return str(value), typ


def phase_install_preflight_refused(c: Ctx) -> None:
    p = "10_install_preflight_refused"
    # サービスを止める前に断る事前確認。PID は変わらない。
    pid = _ensure_running(p)
    tampered = _tamper_elev(p, c)
    before = tree_snapshot(INSTDIR)
    # (0b) サービス登録が別の場所を指す。SCM の設定はキャッシュされ NSIS はレジストリを読むので sc config は使わない。
    orig, typ = _read_image_path()
    obs(p, "image_path_original", orig)
    _write_image_path(ELSEWHERE_IMAGE_PATH, typ)
    try:
        expect(p, _read_image_path() == (ELSEWHERE_IMAGE_PATH, typ), "could not rewrite ImagePath", _read_image_path())
        rc = run_setup(c.setup, [], p, "image_path_setup")
    finally:
        _write_image_path(orig, typ)
    back = _read_image_path()
    obs(p, "image_path_restored", back)
    expect(p, back == (orig, typ), "ImagePath not restored", back)
    expect(p, rc != 0, "setup with a foreign service registration must fail", rc)
    _expect_untouched(p, pid)
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the refused setup (ImagePath)")
    # (0c) 前のインストールが別の場所を指す。
    views = _uninstall_views_with_location()
    obs(p, "install_location_views", [{"view": n, "value": v, "type": t} for n, _, v, t in views])
    if not views:
        raise PhaseFailure(p, "no Uninstall key with InstallLocation in either registry view")
    ELSEWHERE.mkdir(parents=True, exist_ok=True)
    (ELSEWHERE / SHELL_EXE).write_bytes(b"banto-973 dummy\n")
    foreign = f'"{ELSEWHERE}"'  # テンプレートと同じく両端に " を含む
    try:
        for _, view, _, typ_l in views:
            _write_install_location(view, foreign, typ_l)
        rc = run_setup(c.setup, [], p, "install_location_setup")
    finally:
        for _, view, value, typ_l in views:
            _write_install_location(view, value, typ_l)
        shutil.rmtree(ELSEWHERE, ignore_errors=True)
    for name, view, value, typ_l in views:
        back = _read_install_location(view)
        obs(p, f"install_location_restored_{name}", back)
        expect(p, back == (value, typ_l), f"InstallLocation ({name}) not restored", back)
    expect(p, not ELSEWHERE.exists(), "foreign dir was not removed")
    expect(p, rc != 0, "setup over a foreign previous install must fail", rc)
    _expect_untouched(p, pid)
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the refused setup (InstallLocation)")
    _expect_elev_still(p, tampered)
    _recover_by_setup(p, c)


def phase_uninstall_locked_exe(c: Ctx) -> None:
    p = "11_uninstall_locked_exe"
    pid = _ensure_running(p)
    before = tree_snapshot(INSTDIR)
    with ExclusiveFileLock(INSTDIR / ELEV_EXE):
        rc, elapsed = timed(p, "locked_uninstall", lambda: _uninstall_in_place(p, "locked_uninstall"))
    expect(p, rc != 0, "uninstall over a locked exe must fail", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_restarted(p, pid)
    expect_registered(p, "RUNNING")
    _expect_not_delete_pending(p)
    _expect_still_installed(p, c, before)


def phase_uninstall_delete_fail(c: Ctx) -> None:
    p = "12_uninstall_delete_fail"
    # 止めた状態を先に確かめる: 止まっていれば使用中 exe の検査は通り、解除の失敗だけが中止の理由になる。
    _ensure_running(p)
    run(["sc", "stop", SERVICE])
    wait_state(SERVICE, "STOPPED")
    before = tree_snapshot(INSTDIR)
    set_service_marker()
    expect(p, service_marker_present(), "could not set the service marker")
    with DenyServiceRight("SD"):
        rc = _uninstall_in_place(p, "stopped_uninstall")
    expect(p, rc != 0, "uninstall must fail when the service cannot be deleted (stopped)", rc)
    st = sc_state()
    obs(p, "stopped_state_after", st)
    expect(p, st["state"] == "STOPPED", f"{SERVICE} was resumed although it was stopped", st)
    marker = service_marker_present()
    obs(p, "stopped_marker_after", marker)
    expect(p, marker, "service registration was recreated although its deletion was denied (stopped)")
    _expect_not_delete_pending(p)
    _expect_still_installed(p, c, before)
    # 稼働中の確かめ用に動かし直し、スナップショットと目印も取り直す。
    pid = _ensure_running(p)
    before = tree_snapshot(INSTDIR)
    set_service_marker()
    expect(p, service_marker_present(), "could not set the service marker")
    with DenyServiceRight("SD"):
        rc, elapsed = timed(p, "uninstall", lambda: _uninstall_in_place(p, "uninstall"))
    expect(p, rc != 0, "uninstall must fail when the service cannot be deleted", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_restarted(p, pid)
    # 削除は拒まれたので、キーは作り直されていない。
    marker = service_marker_present()
    obs(p, "marker_after", marker)
    expect(p, marker, "service registration was recreated although its deletion was denied")
    expect_registered(p, "RUNNING")
    _expect_not_delete_pending(p)
    _expect_still_installed(p, c, before)


def phase_install_service_install_fails_running(c: Ctx) -> None:
    p = "13_install_service_install_fails_running"
    # 動いていたサービスで service-install だけを失敗させる（grant-service-acl は READ_CONTROL|WRITE_DAC で開く）。
    pid = _ensure_running(p)
    tampered = _tamper_elev(p, c)
    set_service_marker()
    expect(p, service_marker_present(), "could not set the service marker")
    with DenyServiceRight("RC"):  # この中で sc sdshow を呼ばない
        rc, elapsed = timed(p, "setup", lambda: run_setup(c.setup, [], p, "setup"))
    expect(p, rc != 0, "setup must fail when service-install fails", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_restarted(p, pid)  # 前回の登録のまま新しいファイルで再開
    _expect_exes_match_dist(p, c)  # ファイルは入っている（改ざんした elev は dist に戻った）
    marker = service_marker_present()
    obs(p, "marker_after", marker)
    expect(p, marker, "service registration was recreated")
    expect_registered(p, "RUNNING")
    _expect_data_intact(p, c)
    expect_no_shell(p)
    _recover_by_setup(p, c)


def phase_restart_fails_corrupt_exe(c: Ctx) -> None:
    p = "14_restart_fails_corrupt_exe"
    # 中止の後の再開が失敗する道: サービスの exe が壊れていて起動できない。
    _ensure_running(p)
    tampered = _tamper_elev(p, c)
    set_service_marker()
    expect(p, service_marker_present(), "could not set the service marker")
    server = INSTDIR / SERVER_EXE
    old = INSTDIR / "chronogazer-server.banto973-old"
    os.rename(server, old)  # 動いている exe も rename はできる
    server.write_bytes(CORRUPT_SERVER)
    failures_before = service_start_failure_count()
    obs(p, "start_failures_before", failures_before)
    with ExclusiveFileLock(INSTDIR / ELEV_EXE):
        rc, elapsed = timed(p, "setup", lambda: run_setup(c.setup, [], p, "setup"))
    expect(p, rc != 0, "setup over a locked exe must fail", rc)
    expect_not_stop_timeout(p, elapsed)
    reached = wait_until(lambda: service_start_failure_count() >= failures_before + 1, 30)
    time.sleep(3)  # 余分な再開の試みが続かないことを見る
    failures_after = service_start_failure_count()
    obs(p, "start_failures_after", failures_after)
    expect(p, bool(reached), "no service start failure was logged (resume not attempted?)", failures_after)
    expect(
        p,
        failures_after == failures_before + 1,
        "the resume must be attempted exactly once",
        {"before": failures_before, "after": failures_after},
    )
    st = sc_state()
    obs(p, "service_state_after", st)
    expect(p, st["state"] == "STOPPED", f"{SERVICE} not STOPPED after the failed resume", st)
    expect_registered(p, "STOPPED")
    marker = service_marker_present()
    obs(p, "marker_after", marker)
    expect(p, marker, "service registration was recreated")
    expect(p, server.read_bytes() == CORRUPT_SERVER, "corrupt server exe was overwritten despite the abort")
    _expect_elev_still(p, tampered)
    _expect_data_intact(p, c)
    expect_no_shell(p)
    # 復旧: サービスは止まっているので控えの旧 exe を消し、setup を再実行して正しい exe に戻す。
    old.unlink()
    rc = run_setup(c.setup, [], p, "recover")
    expect(p, rc == 0, "recovery setup failed", rc)
    expect_registered(f"{p}_recover", "STOPPED")  # 止まっていたので再開しない
    _expect_exes_match_dist(p, c)
    run(["sc", "start", SERVICE])
    wait_state(SERVICE, "RUNNING")
    obs(p, "recover_health", wait_healthy())
    _expect_data_intact(p, c)


def _stop_fake_shell(fake) -> str:
    """偽シェルを止める。terminate で足りなければ kill。結果を返す。"""
    if fake.poll() is not None:
        return f"already_exited:{fake.returncode}"
    fake.terminate()
    try:
        fake.wait(timeout=10)
        return "terminated"
    except subprocess.TimeoutExpired:
        fake.kill()
        fake.wait(timeout=10)
        return "killed"


def _prepare_fake_shell(p: str):
    """偽シェルの exe（ping.exe の写し。INSTDIR の外、artifact の外）を用意する。起動はしない。"""
    root = Path(os.environ[ROOT_ENV])
    fake_exe = root / "fake-shell" / SHELL_EXE
    fake_exe.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(Path(os.environ["SystemRoot"]) / "System32" / "ping.exe", fake_exe)
    obs(p, "fake_shell_path", str(fake_exe))
    obs(p, "fake_shell_outside_instdir", not fake_exe.resolve().is_relative_to(INSTDIR))
    expect(p, not fake_exe.resolve().is_relative_to(INSTDIR), "fake shell must be outside INSTDIR")
    return fake_exe


def _start_fake_shell(fake_exe: Path):
    return subprocess.Popen(
        [str(fake_exe), "-n", "900", "127.0.0.1"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        creationflags=subprocess.CREATE_NO_WINDOW,
    )


def _visible_preamble(p: str) -> int:
    environment, reasons = vis.gui_check()
    obs(p, "gui_environment", environment)
    expect(p, not reasons, "gui_unsupported", reasons)
    expect(p, PROOF_DIR is not None, "proof dir is not initialised")
    pid = _ensure_running(p)
    expect_no_shell(p)
    return pid


def service_left_running(st: dict) -> bool:
    """sc の結果が「RUNNING でない」か。sc の失敗や state 空を RUNNING でないと読まず例外にする。"""
    if st.get("exit") != 0 or not st.get("state"):
        raise RuntimeError(f"sc queryex unusable: {st}")
    return st["state"] != "RUNNING"


def _spawn_fake_when_service_stops(p: str, child, fake_exe: Path):
    """setup の起動後、サービスが RUNNING でなくなった瞬間に偽シェルを起動する。(偽シェル, 起動直前の時刻)。

    sleep は入れない（sc の起動が間隔になる）。再確認の tasklist より前に偽シェルを出すため。
    """
    deadline = time.monotonic() + 120
    st = None
    while True:
        rc = child.poll()
        if rc is not None:
            raise PhaseFailure(p, "setup exited before the service was stopped", rc)
        try:
            st = sc_state()
            left = service_left_running(st)
        except RuntimeError as e:
            raise PhaseFailure(p, "sc queryex failed while waiting for the service to stop", str(e)) from e
        if left:
            started = time.monotonic()
            fake = _start_fake_shell(fake_exe)
            obs(p, "trigger_service_state", st)
            return fake, started
        if time.monotonic() >= deadline:
            raise PhaseFailure(p, "service did not leave RUNNING within 120 s of starting setup", st)


def _cleanup_visible(p: str, child, fake) -> None:
    if child is not None and child.poll() is None:
        child.kill()
        child.wait(timeout=30)
        obs(p, "cleanup_setup", "killed")
    if fake is not None:
        obs(p, "cleanup_fake_shell", _stop_fake_shell(fake))


def phase_visible_shell_quit_cancel(c: Ctx) -> None:
    p = "14b_visible_shell_quit_cancel"
    # /P（passive）の本物の setup: シェルの終了待ち 90 秒の後の問い（最初の事前確認）で
    # [再試行] → もう 90 秒 → [キャンセル]。何も変えず、偽シェルも強制終了されない。
    pid = _visible_preamble(p)
    before = tree_snapshot(INSTDIR)
    fake_exe = _prepare_fake_shell(p)
    fake = _start_fake_shell(fake_exe)
    child = None
    try:
        time.sleep(2)
        expect(p, fake.poll() is None, "fake shell exited early", fake.returncode)
        expect(p, process_running(SHELL_EXE), f"{SHELL_EXE} (fake) not visible to tasklist")

        def on_question(label: str, elapsed: float) -> None:
            st = sc_state()
            obs(p, f"{label}_service", st)
            expect(p, st["state"] == "RUNNING" and st["pid"] == pid, f"service changed while the {label} question is shown", st)
            expect(p, tree_snapshot(INSTDIR) == before, f"INSTDIR changed while the {label} question is shown")
            expect(p, fake.poll() is None and process_running(SHELL_EXE), f"fake shell gone during the {label} question")

        started = time.monotonic()
        child = subprocess.Popen([str(c.setup), "/P"])  # /S は付けない（MessageBox を実画面に出す）
        try:
            vis.drive_shell_quit_cancel(child, started, PROOF_DIR, lambda k, v: obs(p, k, v), on_question)
        except vis.VisibleFailure as e:
            raise PhaseFailure(p, e.reason, e.observation) from e
        try:
            rc = child.wait(timeout=30)
        except subprocess.TimeoutExpired as e:
            raise PhaseFailure(p, "setup did not exit within 30 s of closing the failed page") from e
        obs(p, "setup_exit", rc)
        expect(p, rc != 0, "cancelled setup must exit non-zero", rc)
        st = sc_state()
        obs(p, "service_after", st)
        expect(p, st["state"] == "RUNNING" and st["pid"] == pid, "service changed by the cancelled setup", st)
        expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the cancelled setup")
        expect(p, fake.poll() is None and process_running(SHELL_EXE), "fake shell was killed by the setup")
        _expect_data_intact(p, c)
    finally:
        _cleanup_visible(p, child, fake)
    expect_no_shell(p)
    _recover_by_setup(p, c)


def _run_visible_recheck_cancel(p: str, c: Ctx, command, matches, expect_after, label: str = "recheck") -> None:
    """14c と 14e の共通本体。command は Popen へそのまま渡す。偽シェルは最初は置かず、サービスが止まった
    瞬間に出す（事前確認の 2 回は通り、再確認だけが問う）。[キャンセル] で止めたサービスが再開され、
    ファイルは変わらず、偽シェルも強制終了されない。expect_after(before) は終了後のフェーズ固有の確認。"""
    pid = _visible_preamble(p)
    before = tree_snapshot(INSTDIR)
    fake_exe = _prepare_fake_shell(p)
    fake = None
    child = None
    resumed = {}
    try:

        def on_question(label: str, elapsed: float) -> None:
            st = sc_state()
            obs(p, f"{label}_service", st)
            expect(p, st["state"] == "STOPPED", f"service not stopped while the {label} question is shown", st)
            expect(p, tree_snapshot(INSTDIR) == before, f"INSTDIR changed while the {label} question is shown")
            expect(p, fake.poll() is None and process_running(SHELL_EXE), f"fake shell gone during the {label} question")

        def service_resumed() -> bool:
            st = sc_state()
            if st["state"] == "RUNNING" and st["pid"] and st["pid"] != pid:
                resumed["pid"] = st["pid"]
                obs(p, "service_resumed", st)
                return True
            return False

        child = subprocess.Popen(command)
        fake, fake_started = _spawn_fake_when_service_stops(p, child, fake_exe)
        try:
            vis.drive_recheck_cancel(
                child, fake_started, PROOF_DIR, lambda k, v: obs(p, k, v), on_question, service_resumed, matches=matches, label=label
            )
        except vis.VisibleFailure as e:
            raise PhaseFailure(p, e.reason, e.observation) from e
        try:
            rc = child.wait(timeout=30)
        except subprocess.TimeoutExpired as e:
            raise PhaseFailure(p, "setup did not exit within 30 s of closing the failed page") from e
        obs(p, "setup_exit", rc)
        expect(p, rc != 0, "cancelled setup must exit non-zero", rc)
        st = sc_state()
        obs(p, "service_after", st)
        expect(
            p,
            st["state"] == "RUNNING" and st["pid"] == resumed.get("pid"),
            "service was restarted again after the resume (.onInstFailed)",
            st,
        )
        obs(p, "health_after", wait_healthy())
        expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the cancelled setup")
        expect(p, fake.poll() is None and process_running(SHELL_EXE), "fake shell was killed by the setup")
        expect_after(before)
    finally:
        _cleanup_visible(p, child, fake)
    expect_no_shell(p)


def phase_visible_recheck_cancel(c: Ctx) -> None:
    p = "14c_visible_recheck_cancel"
    # 再確認（上書き直前、QuitShellForUpdate(1)）の [キャンセル]。
    # 次のフェーズが上書きを通すので _recover_by_setup は呼ばない。
    _run_visible_recheck_cancel(
        p, c, [str(c.setup), "/P"], vis.recheck_question_matches, lambda before: _expect_data_intact(p, c)
    )


def phase_visible_uninstall_recheck_cancel(c: Ctx) -> None:
    p = "14e_visible_uninstall_recheck_cancel"
    # uninstall.exe /P（_?= でその場。/S は付けない）。PREUNINSTALL がサービスを止めた後の再確認
    # （QuitShellForUpdate "アンインストール" 1）の [キャンセル] で、止めたサービスが再開され Abort する。
    # 登録解除とファイル削除はその後なので何も変わらない（問いの表示中の STOPPED は登録が残る確認も兼ねる）。
    # 何も変えていないので _recover_by_setup は呼ばない。

    def expect_after(before: dict) -> None:
        _expect_still_installed(p, c, before)
        expect_registered(p, "RUNNING")
        _expect_not_delete_pending(p)

    _run_visible_recheck_cancel(
        p, c, uninstall_in_place_command(INSTDIR, "/P"), vis.uninstall_recheck_question_matches, expect_after, label="uninstall_recheck"
    )


def phase_visible_retry_continues(c: Ctx) -> None:
    p = "14d_visible_retry_continues"
    # 最初の問いで偽シェルを止めてから [再試行] を押すと、事前確認を抜けて上書きまで通る。
    pid = _visible_preamble(p)
    fake_exe = _prepare_fake_shell(p)
    fake = _start_fake_shell(fake_exe)
    child = None
    try:
        time.sleep(2)
        expect(p, fake.poll() is None, "fake shell exited early", fake.returncode)
        expect(p, process_running(SHELL_EXE), f"{SHELL_EXE} (fake) not visible to tasklist")
        # 本当に上書きされたことを示すため、elev exe を壊す（dist と違う hash にする）。
        want_elev = sha256(_dist_exe(c, "chronogazer-elev"))
        with open(INSTDIR / ELEV_EXE, "ab") as f:
            f.write(b"\0")
        tampered = sha256(INSTDIR / ELEV_EXE)
        obs(p, "elev_hash", {"dist": want_elev, "tampered": tampered})
        expect(p, tampered != want_elev, "elev exe tamper did not change hash")
        before = tree_snapshot(INSTDIR)

        def on_question(label: str, elapsed: float) -> None:
            st = sc_state()
            obs(p, f"{label}_service", st)
            expect(p, st["state"] == "RUNNING" and st["pid"] == pid, f"service changed while the {label} question is shown", st)
            expect(p, tree_snapshot(INSTDIR) == before, f"INSTDIR changed while the {label} question is shown")
            expect(p, fake.poll() is None and process_running(SHELL_EXE), f"fake shell gone during the {label} question")

        def before_retry() -> None:
            obs(p, "stop_fake_shell", _stop_fake_shell(fake))
            gone = wait_until(lambda: not process_running(SHELL_EXE), 10)
            if not gone:
                raise PhaseFailure(p, f"{SHELL_EXE} still running after stopping the fake shell")

        started = time.monotonic()
        child = subprocess.Popen([str(c.setup), "/P"])
        try:
            vis.drive_retry_continue(child, started, PROOF_DIR, lambda k, v: obs(p, k, v), on_question, before_retry)
        except vis.VisibleFailure as e:
            raise PhaseFailure(p, e.reason, e.observation) from e
        try:
            rc = child.wait(timeout=30)
        except subprocess.TimeoutExpired as e:
            raise PhaseFailure(p, "setup did not exit within 30 s after the overwrite") from e
        obs(p, "setup_exit", rc)
        expect(p, rc == 0, "retried setup must succeed", rc)
        _expect_restarted(p, pid)
        expect_registered(p, "RUNNING")
        _expect_exes_match_dist(p, c)
        _expect_not_delete_pending(p)
        _expect_data_intact(p, c)
    finally:
        _cleanup_visible(p, child, fake)
    expect_no_shell(p)


def phase_uninstall_delete_pending(c: Ctx) -> None:
    p = "15_uninstall_delete_pending"
    _ensure_running(p)  # 保留中のサービスは再開されないので PID の比較はしない
    before = tree_snapshot(INSTDIR)
    with ServiceHandleHold(SERVICE):
        rc = _uninstall_in_place(p, "uninstall")
        flag = read_delete_flag()
        obs(p, "delete_flag", flag)
        expect(p, rc != 0, "uninstall must abort on a pending deletion", rc)
        expect(p, flag == 1, "DeleteFlag not set", flag)
        st = sc_state()
        obs(p, "state_pending", st)
        expect(p, st["state"] != "RUNNING", "service with a pending deletion was resumed", st)
        _expect_still_installed(p, c, before)
        r = run([str(INSTDIR / ELEV_EXE), "service-install"])
        obs(p, "service_install_while_pending", {"exit": r.returncode, "stderr_tail": r.stderr[-300:]})
        expect(p, r.returncode != 0, "service-install must refuse a pending deletion", r.returncode)
        flag = read_delete_flag()
        obs(p, "delete_flag_after_install", flag)
        expect(p, flag == 1, "DeleteFlag changed by service-install", flag)
        # setup も service-install が断るので失敗する（止まっていたので再開しない）。
        rc = run_setup(c.setup, [], p, "setup_while_pending")
        expect(p, rc != 0, "setup must fail while the deletion is pending", rc)
        flag = read_delete_flag()
        obs(p, "delete_flag_after_setup", flag)
        expect(p, flag == 1, "DeleteFlag changed by the setup", flag)
        st = sc_state()
        obs(p, "state_after_setup", st)
        expect(p, st["state"] != "RUNNING", "service with a pending deletion was resumed by the setup", st)
        _expect_data_intact(p, c)
    # service_key_exists はレジストリだけを見るので、追加のハンドルを持たない
    gone = wait_until(lambda: not service_key_exists(), 60)
    obs(p, "key_gone", bool(gone))
    expect(p, bool(gone), "service registration did not disappear after the handle closed")
    # `_?=` なしの本物の呼び方。
    r = run([str(INSTDIR / "uninstall.exe"), "/S"], timeout=600)
    obs(p, "uninstall_parent_exit", r.returncode)
    _wait_uninstall_done(p)
    expect(p, DB_PATH.is_file(), "sqlite was removed")
    _expect_data_intact(p, c)
    rc = run_setup(c.setup, [], p, "reinstall")
    expect(p, rc == 0, "reinstall after the pending deletion failed", rc)
    if service_key_exists():
        set_service_marker()
    expect_registered(p, "STOPPED")
    expect_apps_registration(p)
    run(["sc", "start", SERVICE])
    wait_state(SERVICE, "RUNNING")
    obs(p, "health", wait_healthy())
    _expect_data_intact(p, c)


PHASES = (
    phase_reject,
    phase_fresh,
    phase_start_seed,
    phase_acl_overwrite,
    phase_stopped_overwrite,
    phase_uninstall_running,
    phase_reinstall,
    phase_install_locked_exe,
    phase_install_acl_refused,
    phase_install_preflight_refused,
    phase_uninstall_locked_exe,
    phase_uninstall_delete_fail,
    phase_install_service_install_fails_running,
    phase_restart_fails_corrupt_exe,
    phase_visible_shell_quit_cancel,
    phase_visible_recheck_cancel,
    phase_visible_retry_continues,
    phase_visible_uninstall_recheck_cancel,
    phase_uninstall_delete_pending,
)


# ---------------------------------------------------------------- cleanup / main

def _run_owned(root: Path) -> bool:
    """この run が本体を始めたか（main が既存登録なしを確かめた後に印を書く）。"""
    try:
        ev = json.loads((root / "proof" / "evidence.json").read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001
        return False
    return ev.get("owned_run") is True


def cleanup() -> int:
    """この run が作ったサービスだけを止めて解除する。失敗しても最後まで続ける。"""
    owned = _run_owned(Path(os.environ[ROOT_ENV]))
    if not owned and not (service_key_exists() and service_marker_present()):
        print("cleanup: this run owns no service; nothing to do")
        return 0
    try:
        run(["sc", "stop", SERVICE])
        wait_until(lambda: sc_state()["state"] in (None, "STOPPED"), 30)
    except Exception as e:  # noqa: BLE001
        print(f"cleanup: stop {SERVICE}: {e!r}")
    for exe in EXES:
        try:
            run(["taskkill", "/F", "/IM", exe])
        except Exception as e:  # noqa: BLE001
            print(f"cleanup: taskkill {exe}: {e!r}")
    try:
        r = run(["sc", "delete", SERVICE])
        print(f"cleanup: sc delete {SERVICE} -> {r.returncode}")
    except Exception as e:  # noqa: BLE001
        print(f"cleanup: delete {SERVICE}: {e!r}")
    try:
        if os.path.isjunction(INSTDIR):
            os.rmdir(INSTDIR)
    except Exception as e:  # noqa: BLE001
        print(f"cleanup: junction: {e!r}")
    return 0


def main(argv=None) -> int:
    global PROOF_DIR
    ap = argparse.ArgumentParser()
    ap.add_argument("--cleanup-only", action="store_true")
    args = ap.parse_args(argv)
    problems = guard_problems(os.environ, sys.platform, is_admin() if sys.platform == "win32" else False)
    if problems:
        print("REFUSED (nothing changed): " + "; ".join(problems))
        return 3
    marker = root_marker_problem(os.environ)
    if marker:
        print(f"REFUSED: {marker}")
        return 3
    if args.cleanup_only:
        return cleanup()
    root = Path(os.environ[ROOT_ENV])
    PROOF_DIR = root / "proof"
    PROOF_DIR.mkdir(exist_ok=True)
    dist = Path(os.environ[DIST_ENV])
    setup, version = find_setup(dist)
    if sc_state()["state"] is not None:
        print(f"REFUSED: {SERVICE} already registered on this runner")
        return 3
    EVIDENCE["setup"] = {"name": setup.name, "sha256": sha256(setup), "version": version}
    EVIDENCE["owned_run"] = True  # 以降に登録されるサービスはこの run のもの（cleanup が見る）
    save_evidence()
    ctx = Ctx(setup, version, dist)
    try:
        for ph in PHASES:
            print(f"== {ph.__name__}")
            EVIDENCE["phases"].setdefault(ph.__name__, {})
            ph(ctx)
            EVIDENCE["phases"][ph.__name__]["result"] = "pass"
            save_evidence()
    except PhaseFailure as e:
        EVIDENCE["failure"] = {"phase": e.phase, "message": e.message, "observation": e.observation}
        save_evidence()
        print(f"FAILED {e.phase}: {e.message} observation={e.observation}")
        return 1
    except Exception as e:  # noqa: BLE001
        EVIDENCE["failure"] = {"phase": "unexpected", "message": repr(e)}
        save_evidence()
        print(f"FAILED unexpected: {e!r}")
        return 1
    EVIDENCE["result"] = "pass"
    save_evidence()
    print("ALL PHASES PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
