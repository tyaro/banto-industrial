#!/usr/bin/env python3
"""Hub 一体インストーラ（本物の setup.exe）の実 SCM 受入試験（#973 残件A、通常系＋故障系）。

!!! 開発 PC では絶対に実行しない !!!
このスクリプトは実 `C:\\Program Files\\BantoHub`・`C:\\ProgramData\\BantoHub`・
BantoHub / BantoHubSink サービスを書き換え、アンインストールまで行う。
使い捨ての GitHub-hosted Windows ランナーでだけ
`.github/workflows/installer-hub-acceptance.yml` から走らせる。ローカルで走らせて
よいのは `test_hub_setup_acceptance.py`（純粋部分のユニットテスト）だけ。

フェーズ（各 `phase_*` は独立の関数）:
  通常系: 1 拒否 / 2 新規 / 3 起動と種まき / 4 ACL を弱めて稼働中の上書き /
          5 停止中の上書き / 6 稼働中のアンインストール / 7 再インストール
  故障系: 8 使用中 exe でのインストール中止 / 9 事前確認（サブフォルダ・直下ジャンクション）での中止 /
          10 使用中 exe でのアンインストール中止 / 11 Sink 解除失敗 / 12 Hub 解除失敗（Sink の戻し）/
          13 Sink の DeleteFlag 保留 / 14 Hub の DeleteFlag 保留（拒否、解消、完了まで）
  故障系は、中止後に止めたサービスが再開され、ファイル・登録・データが変わらず、
  setup の再実行で回復できることを確かめる。アンインストールの故障系は `_?=` を付けて
  同じ本体をその場で走らせ、完了と終了コードを得る（フェーズ 14 の最後だけ本物の呼び方）。
  可視 UI（setup / uninstall.exe を /P で実画面に出し、窓を読んでボタンを押す。駆動部品は
  ChronoGazer 側の chronogazer_setup_visible.py を共有）:
          8b 使用中の sink exe で `File` の窓（[再試行]/[キャンセル]）が出る道: 窓と失敗の頁の間は
             BantoHub が STOPPED のまま、頁を閉じた後の .onInstFailed で RUNNING に戻る /
          10b `_?=` 付きの /P のアンインストールが使用中の sink exe で中止する道: 主 MessageBox より先に
             BantoHub を再開し、その結果が本文に入り、un.onUninstFailed で二重に再開しない
             （二重再開は PID の不変でしか見ていない）。uninstall.exe は保存された言語が無いと先に
             言語選択の窓（Installer Language）を出すので、OK を押してから案内を待つ
  対象外: 上の 2 つ以外の可視 UI（MessageBox の表示と二重再開は silent では見ていない）・icacls の非 0
  終了そのもの・`File` の書込み途中の失敗・消し残り（RequireBantoHubExesDeleted）・
  サービス停止 timeout・再開失敗・Sink 稼働中・実 collector。
各フェーズの観測を証拠 JSON（proof/evidence.json）に残し、期待と違えばフェーズ名と
観測を出して失敗する。テスト用の管理者パスワードは実行ごとの乱数で、ログにも証拠にも出さない。

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
import string
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

# 可視 UI の駆動部品は ChronoGazer 側と共有する（複製しない）。import は Win32 に触れない（関数の中でだけ読む）。
_CG_TESTS_DIR = Path(__file__).resolve().parents[3] / "chronogazer" / "installer" / "tests"
if str(_CG_TESTS_DIR) not in sys.path:
    sys.path.insert(0, str(_CG_TESTS_DIR))
import chronogazer_setup_visible as vis  # noqa: E402

GUARD_ENV = "BANTO_973_HUB_SETUP_TEST"
GUARD_VALUE = "github-hosted-disposable-only-v1"
ROOT_ENV = "BANTO_973_HUB_ROOT"  # 証拠ルート（マーカー .banto-973-ci を持つ）
DIST_ENV = "BANTO_973_HUB_DIST"  # build-release.ps1 の -OutDir

INSTDIR = Path(r"C:\Program Files\BantoHub")
ELSEWHERE = Path(r"C:\BantoHubElsewhere")
DATA_ROOT = Path(os.environ.get("ProgramData", r"C:\ProgramData")) / "BantoHub"
PROFILE = DATA_ROOT / "profiles" / "default"
SQLITE = PROFILE / "config" / "banto-hub.sqlite3"
SENTINEL = PROFILE / "data" / "banto-973-sentinel.txt"
SINK_DIR = DATA_ROOT / "sink"
SINK_TOML = SINK_DIR / "banto-hub-sink.toml"
SINK_EXAMPLE = SINK_DIR / "banto-hub-sink.toml.example"
HUB_SVC = "BantoHub"
SINK_SVC = "BantoHubSink"
SERVICES = (HUB_SVC, SINK_SVC)
# 製品が停止の待ち切り（30 秒 + 追加 60 秒）で中止する道は最短でも 90 秒掛かる。
# それより短ければ、停止の待ち切りではない理由で中止したと分かる。
STOP_TIMEOUT_ABORT_SECONDS = 90.0
EXES = ("banto-hub.exe", "banto-hub-shell.exe", "banto-hub-elev.exe", "banto-hub-sink.exe")
STATUS_URL = "http://127.0.0.1:8722/api/auth/status"
SETUP_URL = "http://127.0.0.1:8722/api/auth/setup"
USERS_SID = "*S-1-5-32-545"


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


def random_password() -> str:
    alphabet = string.ascii_letters + string.digits
    return "Bt9-" + "".join(secrets.choice(alphabet) for _ in range(28))


def parse_sc_query(text: str) -> dict:
    """`sc queryex` の出力から state（RUNNING 等）と pid。サービス無しは state=None。"""
    m = re.search(r"STATE\s*:\s*\d+\s+([A-Z_]+)", text)
    pid = re.search(r"PID\s*:\s*(\d+)", text)
    return {"state": m.group(1) if m else None, "pid": int(pid.group(1)) if pid else None}


def root_marker_problem(env) -> str | None:
    """証拠ルートの目印（<root>/.banto-973-ci がファイル）の確かめ。問題なら理由、無ければ None。"""
    raw = env.get(ROOT_ENV)
    if not raw:
        return f"{ROOT_ENV} is not set"
    if not (Path(raw) / ".banto-973-ci").is_file():
        return "proof root marker missing"
    return None


def parse_sc_qc(text: str) -> dict:
    """`sc qc` の出力から起動種別（DEMAND_START 等）と BINARY_PATH_NAME。"""
    st = re.search(r"START_TYPE\s*:\s*\d+\s+([A-Z_]+)", text)
    bp = re.search(r"BINARY_PATH_NAME\s*:\s*(.+)", text)
    return {"start_type": st.group(1) if st else None, "image_path": bp.group(1).strip() if bp else None}


def image_path_exe(image_path: str) -> str:
    """ImagePath から exe のパス（クォートあり・なしの両方を受ける。引数は落とす）。"""
    p = image_path.strip()
    if p.startswith('"'):
        end = p.find('"', 1)
        return p[1:end] if end > 0 else p[1:]
    m = re.match(r"(.+?\.exe)(\s|$)", p, re.IGNORECASE)
    return m.group(1) if m else p


def image_path_inside(image_path: str, instdir: Path) -> bool:
    exe = os.path.normcase(os.path.normpath(image_path_exe(image_path)))
    base = os.path.normcase(os.path.normpath(str(instdir))) + os.sep
    return exe.startswith(base)


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
        raise RuntimeError(
            f"owner 取得に失敗（returncode={returncode}）: {stderr[-300:]}"
        )
    return stdout.strip()


def parse_sc_sdshow(returncode: int, stdout: str, stderr: str) -> str:
    """`sc sdshow` の SDDL 行（`D:` か `O:` で始まる最初の行）。失敗・空は例外（空の観測を「変わっていない」と誤判定させない）。"""
    if returncode == 0:
        for line in stdout.splitlines():
            t = line.strip()
            if t.startswith(("D:", "O:")):
                return t
    raise RuntimeError(f"sc sdshow に失敗（returncode={returncode}）: {(stdout + stderr)[-300:]}")


_SDDL_DACL_HEAD = re.compile(
    r"(?:O:(?:S-[\d-]+|[A-Z]{2}))?(?:G:(?:S-[\d-]+|[A-Z]{2}))?D:(?:P|AI|AR)*"
)


def sddl_with_deny_delete(sddl: str) -> str:
    """DACL の先頭に「Administrators に DELETE 拒否」の ACE を入れる。O:/G: の中の文字や SACL は壊さない。"""
    sddl = sddl.strip()
    m = _SDDL_DACL_HEAD.match(sddl)
    if not m:
        raise RuntimeError(f"DACL が無い SDDL: {sddl!r}")
    return sddl[: m.end()] + "(D;;SD;;;BA)" + sddl[m.end() :]


def parse_icacls_output(returncode: int, stdout: str, stderr: str) -> str:
    """`icacls <path>` の出力。失敗・空は例外。"""
    if returncode != 0 or not stdout.strip():
        raise RuntimeError(f"icacls に失敗（returncode={returncode}）: {stderr[-300:]}")
    return stdout


def uninstall_in_place_command(instdir, mode_flag: str = "/S") -> str:
    """`_?=` 付きでその場で走らせる呼び方。`_?=` は最後の引数でクォートしない（NSIS の規則）ので文字列で渡す。"""
    return f'"{instdir}\\uninstall.exe" {mode_flag} _?={instdir}'


FILE_LOCKED_EXE = "banto-hub-sink.exe"
UNINSTALL_LOCKED_PREFIX = "使用中の実行ファイルがあります（"
UNINSTALL_LOCKED_ABORTED = "アンインストールを中止しました"
UNINSTALL_LOCKED_RESUMED = "その前に止めていたサービス（BantoHub）は再開し、RUNNING になったことを確かめました。"


def file_locked_dialog_matches(text: str) -> bool:
    """NSIS 自身の `File` の使用中の窓。文言は OS の言語で変わるので、書けないファイル名だけで照合する。"""
    return FILE_LOCKED_EXE in text


def uninstall_locked_notice_matches(text: str) -> bool:
    """使用中 exe でアンインストールを中止する主 MessageBox。再開の結果（$ServiceRestoreNote）が本文に入っている。"""
    return (
        text.startswith(UNINSTALL_LOCKED_PREFIX)
        and FILE_LOCKED_EXE in text
        and UNINSTALL_LOCKED_ABORTED in text
        and UNINSTALL_LOCKED_RESUMED in text
    )


def find_setup(dist: Path) -> tuple[Path, str]:
    cands = sorted(dist.glob("BantoHub_*_x64-setup.exe"))
    if len(cands) != 1:
        raise PhaseFailure("prepare", f"expected exactly one setup in {dist}", [c.name for c in cands])
    m = re.match(r"BantoHub_(.+)_x64-setup\.exe$", cands[0].name)
    return cands[0], m.group(1)


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def tree_hashes(root: Path) -> dict[str, str]:
    return {
        str(p.relative_to(root)): sha256(p) for p in sorted(root.rglob("*")) if p.is_file()
    }


def status_initialized(body: dict) -> bool:
    return body.get("initialized") is True


def evidence_has_secret(evidence, secret: str) -> bool:
    return secret in json.dumps(evidence, ensure_ascii=False)


def request_headers(has_body: bool) -> dict:
    # Hub の /api/auth/* は CSRF のミドルウェアの内側にあるため、GET・POST とも付ける。
    headers = {"X-Banto-Client": "banto"}
    if has_body:
        headers["Content-Type"] = "application/json"
    return headers


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


def sc_state(name: str) -> dict:
    r = run(["sc", "queryex", name])
    d = parse_sc_query(r.stdout)
    d["exit"] = r.returncode
    return d


def sc_config(name: str) -> dict:
    return parse_sc_qc(run(["sc", "qc", name]).stdout)


def service_key_exists(name: str) -> bool:
    import winreg

    try:
        winreg.CloseKey(
            winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, rf"SYSTEM\CurrentControlSet\Services\{name}")
        )
        return True
    except FileNotFoundError:
        return False


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


def http_json(url: str, body: dict | None = None, timeout: float = 10):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers=request_headers(data is not None))
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status, json.loads(resp.read() or b"{}")
    except urllib.error.HTTPError as e:
        raw = e.read()
        try:
            return e.code, json.loads(raw or b"{}")
        except ValueError:
            return e.code, {"raw": raw[:200].decode("utf-8", "replace")}


def wait_status(timeout: float = 60) -> dict:
    last = []

    def p():
        try:
            code, body = http_json(STATUS_URL, timeout=3)
            last[:] = [code, body]
            return body if code == 200 else None
        except Exception as e:  # noqa: BLE001 - 接続できるまでの待ち
            last[:] = [repr(e)]
            return None

    r = wait_until(p, timeout, 1.0)
    if r is None:
        raise RuntimeError(f"8722 did not answer: {last}")
    return r


def process_running(image: str) -> bool:
    r = run(["tasklist", "/FI", f"IMAGENAME eq {image}", "/FO", "CSV", "/NH"])
    return image.lower() in r.stdout.lower()


def run_setup(setup: Path, extra: list[str], phase: str, label: str) -> int:
    # /D= は最後の引数・クォートなし（NSIS の規則）。
    r = run([str(setup), "/S", *extra], timeout=900)
    obs(phase, f"{label}_exit", r.returncode)
    return r.returncode


def owner_of(path: Path) -> str:
    r = run(
        [
            "pwsh",
            "-NoProfile",
            "-Command",
            f"(Get-Acl -LiteralPath '{path}').Owner",
        ]
    )
    return parse_owner_output(r.returncode, r.stdout, r.stderr)


def icacls_output(path) -> str:
    r = run(["icacls", str(path)])
    return parse_icacls_output(r.returncode, r.stdout, r.stderr)


def acl_snapshot(path) -> dict:
    return {"owner": owner_of(path), "icacls": icacls_output(path)}


def read_delete_flag(name: str) -> int | None:
    import winreg

    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, rf"SYSTEM\CurrentControlSet\Services\{name}") as k:
            value, typ = winreg.QueryValueEx(k, "DeleteFlag")
    except FileNotFoundError:
        return None
    if typ != winreg.REG_DWORD:
        raise RuntimeError(f"DeleteFlag の型が REG_DWORD でない: {typ}")
    return int(value)


SERVICE_MARKER = "Banto973Marker"


def set_service_marker(name: str) -> None:
    """サービスのキーに目印の値を置く。解除と再登録でキーが作り直されると消える。"""
    run(
        ["reg", "add", rf"HKLM\SYSTEM\CurrentControlSet\Services\{name}", "/v", SERVICE_MARKER, "/t", "REG_SZ", "/d", "1", "/f"],
        check=True,
    )


def service_marker_present(name: str) -> bool:
    import winreg

    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, rf"SYSTEM\CurrentControlSet\Services\{name}") as k:
            winreg.QueryValueEx(k, SERVICE_MARKER)
        return True
    except FileNotFoundError:
        return False


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


class DenyServiceDelete:
    """サービスの SDDL に Administrators の DELETE 拒否を入れる（DeleteService を失敗させる）。抜けるとき元に戻す。"""

    def __init__(self, name: str):
        self.name = name
        self.original = None

    def __enter__(self):
        self.original = _sc_sdshow(self.name)
        run(["sc", "sdset", self.name, sddl_with_deny_delete(self.original)], check=True)
        now = _sc_sdshow(self.name)
        if "(D;;SD;;;BA)" not in now:
            run(["sc", "sdset", self.name, self.original])
            raise RuntimeError(f"DELETE 拒否が入っていない: {now}")
        return self

    def __exit__(self, exc_type, exc, tb):
        if self.original is not None:
            r = run(["sc", "sdset", self.name, self.original])
            self.original = None
            if r.returncode != 0:
                message = f"sc sdset の戻しに失敗: {self.name} -> {r.returncode}: {r.stdout}{r.stderr}"
                print(message)
                if exc_type is None:
                    raise RuntimeError(message)
        return False


def tree_snapshot(root) -> dict[str, str]:
    """中身のハッシュ。空は例外（観測が空のときに通さない）。再解析ポイントの中は辿らないよう、置く前に取り、外した後に比べる。"""
    snap = tree_hashes(Path(root))
    if not snap:
        raise RuntimeError(f"tree_hashes が空: {root}")
    return snap


def uninstall_registration() -> list[str]:
    """Apps（Uninstall キー）のうち INSTDIR を指す登録。"""
    import winreg

    needle = str(INSTDIR).lower()
    found = []
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
                if needle in str(us).lower():
                    found.append(sub)
            except OSError:
                continue
    return sorted(set(found))


def expect(phase: str, cond: bool, message: str, observation=None) -> None:
    if not cond:
        raise PhaseFailure(phase, message, observation)


def timed(p: str, label: str, fn):
    """fn() の所要秒を証拠に残して (結果, 秒) を返す。"""
    start = time.monotonic()
    result = fn()
    elapsed = round(time.monotonic() - start, 1)
    obs(p, f"{label}_elapsed_seconds", elapsed)
    return result, elapsed


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
    return outside, targets, before


def expect_outside_unchanged(p: str, targets, before) -> None:
    """targets[0] は外部フォルダ。中身のハッシュと所有者・ACL の不変を確かめる。"""
    after = {"hashes": tree_hashes(targets[0]), "acl": {str(t): acl_snapshot(t) for t in targets}}
    obs(p, "outside_before", before)
    obs(p, "outside_after", after)
    expect(p, before["hashes"] == after["hashes"], "outside folder content changed", after["hashes"])
    expect(p, before["acl"] == after["acl"], "outside owner/ACL changed", after["acl"])


def expect_services_registered_stopped(phase: str) -> None:
    for name in SERVICES:
        st = sc_state(name)
        qc = sc_config(name)
        obs(phase, f"{name}_state", st)
        obs(phase, f"{name}_config", qc)
        expect(phase, st["state"] == "STOPPED", f"{name} not STOPPED", st)
        expect(phase, qc["start_type"] == "DEMAND_START", f"{name} not DEMAND_START", qc)
        expect(
            phase,
            bool(qc["image_path"]) and image_path_inside(qc["image_path"], INSTDIR),
            f"{name} ImagePath outside INSTDIR",
            qc,
        )


def expect_no_shell(phase: str) -> None:
    running = process_running("banto-hub-shell.exe")
    obs(phase, "shell_running", running)
    expect(phase, not running, "banto-hub-shell.exe is running (service would exit 2 on profile lock)")


# ---------------------------------------------------------------- フェーズ

class Ctx:
    def __init__(self, setup: Path, version: str, dist: Path, password: str):
        self.setup = setup
        self.version = version
        self.dist = dist
        self.password = password
        self.hashes: dict[str, str] = {}
        self.pid_before: int | None = None


def phase_reject(c: Ctx) -> None:
    p = "1_reject"
    expect(p, not INSTDIR.exists() and not ELSEWHERE.exists(), "targets pre-exist")
    rc = run_setup(c.setup, [f"/D={ELSEWHERE}"], p, "elsewhere")
    expect(p, rc != 0, "setup with foreign /D= must fail", rc)
    expect(p, not ELSEWHERE.exists(), "foreign dir was created")
    expect(p, not INSTDIR.exists(), "INSTDIR was created")
    for name in SERVICES:
        expect(p, sc_state(name)["state"] is None, f"{name} registered after refusal")

    outside, targets, before = weakened_outside(p, "hub-junction-outside")
    run(["cmd", "/c", "mklink", "/J", str(INSTDIR), str(outside)], check=True)
    try:
        rc = run_setup(c.setup, [], p, "junction")
        expect(p, rc != 0, "setup over a junction must fail", rc)
        expect_outside_unchanged(p, targets, before)
        for name in SERVICES:
            expect(p, sc_state(name)["state"] is None, f"{name} registered after junction refusal")
    finally:
        if INSTDIR.exists() or os.path.lexists(INSTDIR):
            os.rmdir(INSTDIR)  # ジャンクションだけ消す（中身は触らない）
    expect(p, not os.path.lexists(INSTDIR), "junction not removed")


def phase_fresh(c: Ctx) -> None:
    p = "2_fresh"
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "fresh setup failed", rc)
    for exe in (*EXES, "uninstall.exe"):
        expect(p, (INSTDIR / exe).is_file(), f"{exe} missing")
    expect_services_registered_stopped(p)
    expect_no_shell(p)
    reg = uninstall_registration()
    obs(p, "apps_registration", reg)
    expect(p, bool(reg), "Apps registration not found")


def _set_allow_setup_env() -> None:
    # サービスの Environment（REG_MULTI_SZ）で初回セットアップを許可する。
    run(
        [
            "reg", "add", rf"HKLM\SYSTEM\CurrentControlSet\Services\{HUB_SVC}",
            "/v", "Environment", "/t", "REG_MULTI_SZ", "/d", "BANTO_ALLOW_SETUP=1", "/f",
        ],
        check=True,
    )


def phase_start_seed(c: Ctx) -> None:
    p = "3_start_seed"
    _set_allow_setup_env()
    run(["sc", "start", HUB_SVC])
    obs(p, "running", wait_state(HUB_SVC, "RUNNING"))
    st = wait_status()
    obs(p, "status_before", st)
    code, body = http_json(
        SETUP_URL, {"username": "acceptance", "password": c.password, "displayName": "Acceptance"}
    )
    obs(p, "setup_http", {"code": code, "keys": sorted(body) if isinstance(body, dict) else None})
    expect(p, code == 200, "first-run setup failed", {"code": code})
    code, st = http_json(STATUS_URL)
    obs(p, "status_after", st)
    expect(p, status_initialized(st), "status not initialized after setup", st)
    SENTINEL.parent.mkdir(parents=True, exist_ok=True)
    SENTINEL.write_text("banto-973 sentinel\n", encoding="utf-8")
    c.hashes["sentinel"] = sha256(SENTINEL)
    expect(p, SINK_EXAMPLE.is_file(), "sink toml example missing", str(SINK_EXAMPLE))
    shutil.copyfile(SINK_EXAMPLE, SINK_TOML)
    r = run([str(INSTDIR / "banto-hub-elev.exe"), "prepare-sink-config"])
    obs(p, "prepare_sink_config", {"exit": r.returncode, "stderr_tail": r.stderr[-300:]})
    expect(p, r.returncode == 0, "prepare-sink-config failed", r.returncode)
    expect(p, SINK_TOML.is_file(), "sink toml missing after prepare")
    c.hashes["sink_toml"] = sha256(SINK_TOML)
    obs(p, "hashes", dict(c.hashes))


def _expect_data_intact(p: str, c: Ctx) -> None:
    expect(p, SENTINEL.is_file() and sha256(SENTINEL) == c.hashes["sentinel"], "sentinel changed")
    expect(p, SINK_TOML.is_file() and sha256(SINK_TOML) == c.hashes["sink_toml"], "sink toml changed")


def phase_acl_overwrite(c: Ctx) -> None:
    p = "4_acl_overwrite"
    for target in (INSTDIR, INSTDIR / "banto-hub.exe"):
        run(["icacls", str(target), "/grant", f"{USERS_SID}:(M)"], check=True)
        weak = icacls_explicit_users_aces(run(["icacls", str(target)]).stdout)
        obs(p, f"weakened_{target.name}", weak)
        expect(p, bool(weak), f"could not weaken ACL on {target}")
        run(["icacls", str(target), "/setowner", os.environ["USERNAME"]], check=True)
        weak_owner = owner_of(target)
        obs(p, f"weakened_owner_{target.name}", weak_owner)
        expect(p, "Administrators" not in weak_owner, f"could not change owner of {target}", weak_owner)
    with open(INSTDIR / "banto-hub-sink.exe", "ab") as f:
        f.write(b"\0")
    expect(p, sha256(INSTDIR / "banto-hub-sink.exe") != sha256(c.dist / f"banto-hub-sink-v{c.version}-windows-x86_64.exe"),
           "sink exe tamper did not change hash")
    c.pid_before = sc_state(HUB_SVC)["pid"]
    obs(p, "pid_before", c.pid_before)
    expect(p, sc_state(HUB_SVC)["state"] == "RUNNING" and bool(c.pid_before), "BantoHub not running before overwrite")
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "overwrite with running service failed", rc)
    st = wait_state(HUB_SVC, "RUNNING")
    obs(p, "pid_after", st["pid"])
    expect(p, st["pid"] != c.pid_before, "BantoHub PID unchanged (was not restarted)", st)
    status = wait_status()
    expect(p, status_initialized(status), "status not initialized after overwrite", status)
    sink = sc_state(SINK_SVC)
    obs(p, "sink_state", sink)
    expect(p, sink["state"] == "STOPPED", "Sink was started by the overwrite", sink)
    for target in (INSTDIR, INSTDIR / "banto-hub.exe"):
        aces = icacls_explicit_users_aces(run(["icacls", str(target)]).stdout)
        owner = owner_of(target)
        obs(p, f"after_{target.name}", {"explicit_users": aces, "owner": owner})
        expect(p, not aces, f"Users explicit ACE remains on {target}", aces)
        expect(p, "Administrators" in owner, f"owner of {target} is not Administrators", owner)
    want = sha256(c.dist / f"banto-hub-sink-v{c.version}-windows-x86_64.exe")
    got = sha256(INSTDIR / "banto-hub-sink.exe")
    obs(p, "sink_hash", {"dist": want, "installed": got})
    expect(p, want == got, "installed sink exe differs from dist")
    _expect_data_intact(p, c)
    expect_no_shell(p)


def phase_stopped_overwrite(c: Ctx) -> None:
    p = "5_stopped_overwrite"
    run(["sc", "stop", HUB_SVC])
    wait_state(HUB_SVC, "STOPPED")
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "overwrite with stopped service failed", rc)
    expect_services_registered_stopped(p)
    _expect_data_intact(p, c)


def _wait_uninstall_done(p: str) -> None:
    """親が戻った後の子（temp 本体）の完了待ち。サービス・exe・Apps 登録がすべて消えること。"""

    def done():
        keys = [n for n in SERVICES if service_key_exists(n) or sc_state(n)["state"] is not None]
        files = [e for e in (*EXES, "uninstall.exe") if (INSTDIR / e).exists()]
        reg = uninstall_registration()
        return {"keys": keys, "files": files} if not keys and not files and not reg else None

    ok = wait_until(done, 240, 2.0)
    obs(p, "services_registry", {n: service_key_exists(n) for n in SERVICES})
    obs(p, "files_left", [e for e in (*EXES, "uninstall.exe") if (INSTDIR / e).exists()])
    expect(p, ok is not None, "uninstall did not finish (services/files remain)")
    reg = uninstall_registration()
    obs(p, "apps_registration", reg)
    expect(p, not reg, "Apps registration remains", reg)


def phase_uninstall_running(c: Ctx) -> None:
    p = "6_uninstall_running"
    run(["sc", "start", HUB_SVC])
    wait_state(HUB_SVC, "RUNNING")
    wait_status()
    # `_?=` を付けない実際の呼び方。親は子（temp にコピーされた本体）を残してすぐ戻る。
    r = run([str(INSTDIR / "uninstall.exe"), "/S"], timeout=600)
    obs(p, "uninstall_parent_exit", r.returncode)

    _wait_uninstall_done(p)
    expect(p, SQLITE.is_file(), "sqlite was removed")
    _expect_data_intact(p, c)


def phase_reinstall(c: Ctx) -> None:
    p = "7_reinstall"
    rc = run_setup(c.setup, [], p, "setup")
    expect(p, rc == 0, "reinstall failed", rc)
    expect_services_registered_stopped(p)
    expect(p, bool(uninstall_registration()), "Apps registration not restored")
    expect_no_shell(p)
    run(["sc", "start", HUB_SVC])
    wait_state(HUB_SVC, "RUNNING")
    status = wait_status()
    obs(p, "status", status)
    expect(p, status_initialized(status), "status not initialized after reinstall", status)
    _expect_data_intact(p, c)


# ---------------------------------------------------------------- 故障系

def _dist_sink(c: Ctx) -> Path:
    return c.dist / f"banto-hub-sink-v{c.version}-windows-x86_64.exe"


def _ensure_hub_running(p: str) -> int:
    if sc_state(HUB_SVC)["state"] != "RUNNING":
        run(["sc", "start", HUB_SVC])
        wait_state(HUB_SVC, "RUNNING")
    status = wait_status()
    pid = sc_state(HUB_SVC)["pid"]
    obs(p, "pid_before", pid)
    expect(p, status_initialized(status), "status not initialized before fault", status)
    expect(p, bool(pid), "BantoHub has no PID")
    return pid


def _expect_hub_restarted(p: str, before_pid: int) -> None:
    st = wait_state(HUB_SVC, "RUNNING")
    status = wait_status()
    obs(p, "pid_after", st["pid"])
    expect(p, st["pid"] != before_pid, "BantoHub PID unchanged (stopped service was not resumed)", st)
    expect(p, status_initialized(status), "status not initialized after abort", status)


def _expect_hub_untouched(p: str, before_pid: int) -> None:
    st = sc_state(HUB_SVC)
    obs(p, "hub_untouched", st)
    expect(p, st["state"] == "RUNNING" and st["pid"] == before_pid, "BantoHub was stopped before the precheck", st)


def _expect_registered(p: str, name: str) -> None:
    st = sc_state(name)
    qc = sc_config(name)
    flag = read_delete_flag(name)
    obs(p, f"{name}_state", st)
    obs(p, f"{name}_config", qc)
    obs(p, f"{name}_delete_flag", flag)
    expect(p, st["state"] is not None, f"{name} not registered", st)
    expect(p, flag in (None, 0), f"{name} has DeleteFlag", flag)
    expect(p, qc["start_type"] == "DEMAND_START", f"{name} not DEMAND_START", qc)
    expect(
        p,
        bool(qc["image_path"]) and image_path_inside(qc["image_path"], INSTDIR),
        f"{name} ImagePath outside INSTDIR",
        qc,
    )


def _tamper_sink(p: str, c: Ctx) -> str:
    """同じ版を上書きするのでハッシュ比較では上書きを見分けられない。末尾の \\0 が残っているかで見る。"""
    with open(INSTDIR / "banto-hub-sink.exe", "ab") as f:
        f.write(b"\0")
    got = sha256(INSTDIR / "banto-hub-sink.exe")
    obs(p, "sink_tampered_hash", got)
    expect(p, got != sha256(_dist_sink(c)), "sink exe tamper did not change hash")
    return got


def _expect_sink_matches_dist(p: str, c: Ctx) -> None:
    want, got = sha256(_dist_sink(c)), sha256(INSTDIR / "banto-hub-sink.exe")
    obs(p, "sink_hash", {"dist": want, "installed": got})
    expect(p, want == got, "installed sink exe differs from dist")


def _expect_sink_still(p: str, tampered: str) -> None:
    got = sha256(INSTDIR / "banto-hub-sink.exe")
    obs(p, "sink_hash_after_abort", {"tampered": tampered, "now": got})
    expect(p, got == tampered, "sink exe was overwritten despite the abort")


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
    expect(p, SQLITE.is_file(), "sqlite was removed")
    _expect_data_intact(p, c)


def _recover_by_setup(p: str, c: Ctx) -> None:
    rc = run_setup(c.setup, [], p, "recover")
    expect(p, rc == 0, "recovery setup failed", rc)
    wait_state(HUB_SVC, "RUNNING")
    status = wait_status()
    obs(p, "recover_status", status)
    expect(p, status_initialized(status), "status not initialized after recovery", status)
    for name in SERVICES:
        _expect_registered(p, name)
    _expect_sink_matches_dist(p, c)
    _expect_data_intact(p, c)


def phase_install_locked_exe(c: Ctx) -> None:
    p = "8_install_locked_exe"
    pid = _ensure_hub_running(p)
    tampered = _tamper_sink(p, c)
    with ExclusiveFileLock(INSTDIR / "banto-hub-sink.exe"):
        rc, elapsed = timed(p, "locked_setup", lambda: run_setup(c.setup, [], p, "locked_setup"))
    expect(p, rc != 0, "setup over a locked exe must fail", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_hub_restarted(p, pid)
    sink = sc_state(SINK_SVC)
    obs(p, "sink_state", sink)
    expect(p, sink["state"] == "STOPPED", "Sink was started by the aborted setup", sink)
    _expect_registered(p, SINK_SVC)
    _expect_sink_still(p, tampered)
    _expect_data_intact(p, c)
    expect_no_shell(p)
    _recover_by_setup(p, c)


def phase_install_acl_refused(c: Ctx) -> None:
    p = "9_install_acl_refused"
    pid = _ensure_hub_running(p)
    tampered = _tamper_sink(p, c)
    # サブフォルダ（ACL の段階はサービスを止める前に中止する）
    before = tree_snapshot(INSTDIR)
    subdir = INSTDIR / "banto-973-subdir"
    subdir.mkdir()
    (subdir / "keep.txt").write_text("keep", encoding="utf-8")
    try:
        rc = run_setup(c.setup, [], p, "subdir_setup")
        expect(p, rc != 0, "setup with a subfolder must fail", rc)
        _expect_hub_untouched(p, pid)
        expect(p, subdir.is_dir() and (subdir / "keep.txt").is_file(), "subfolder was removed by the setup")
    finally:
        shutil.rmtree(subdir, ignore_errors=True)
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the refused setup (subfolder)")
    _expect_sink_still(p, tampered)
    # 直下のジャンクション
    outside, targets, outside_before = weakened_outside(p, "hub-nested-junction-outside")
    link = INSTDIR / "banto-973-link"
    run(["cmd", "/c", "mklink", "/J", str(link), str(outside)], check=True)
    try:
        rc = run_setup(c.setup, [], p, "junction_setup")
        expect(p, rc != 0, "setup with a nested junction must fail", rc)
        _expect_hub_untouched(p, pid)
        expect_outside_unchanged(p, targets, outside_before)
    finally:
        if os.path.lexists(link):
            os.rmdir(link)  # ジャンクションだけ消す
    expect(p, not os.path.lexists(link), "nested junction not removed")
    expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed by the refused setup (junction)")
    _expect_sink_still(p, tampered)
    _recover_by_setup(p, c)


def _visible_preamble(p: str) -> int:
    environment, reasons = vis.gui_check()
    obs(p, "gui_environment", environment)
    expect(p, not reasons, "gui_unsupported", reasons)
    expect(p, PROOF_DIR is not None, "proof dir is not initialised")
    pid = _ensure_hub_running(p)
    expect_no_shell(p)
    return pid


def _cleanup_visible(p: str, child) -> None:
    """例外時も setup を必ず kill して待つ（ロックを解く前に片付け、上書きへ進ませない）。"""
    if child is not None and child.poll() is None:
        child.kill()
        child.wait(timeout=30)
        obs(p, "cleanup_setup", "killed")


def _wait_exit_within_30s(p: str, child) -> int:
    try:
        rc = child.wait(timeout=30)
    except subprocess.TimeoutExpired as e:
        raise PhaseFailure(p, "setup did not exit within 30 s of closing the failed page") from e
    obs(p, "setup_exit", rc)
    return rc


def phase_visible_file_locked_cancel(c: Ctx) -> None:
    p = "8b_visible_file_locked_cancel"
    # /P の本物の setup: 使用中の sink exe に `File` が書けず NSIS 自身の窓（[再試行]/[キャンセル]）が出る。
    # フックが割り込めない道。窓の間と失敗の頁の間は止めたサービスが STOPPED のまま、
    # 頁を閉じた後の .onInstFailed で RUNNING に戻る。
    pid = _visible_preamble(p)
    tampered = _tamper_sink(p, c)
    # 失敗の道で setup が生きたままロックが消えて上書きへ進まないよう、setup を片付けてからロックを解く順にする。
    with ExclusiveFileLock(INSTDIR / FILE_LOCKED_EXE):
        child = None
        try:

            def on_shown(lbl: str, elapsed: float) -> None:
                st = sc_state(HUB_SVC)
                obs(p, f"{lbl}_hub", st)
                expect(p, st["state"] == "STOPPED", f"BantoHub not STOPPED while the {lbl} window is shown", st)

            def while_shown(lbl: str) -> None:
                st = sc_state(HUB_SVC)
                expect(p, st["state"] == "STOPPED", f"BantoHub changed while the {lbl} window is shown", st)

            def ready_to_close() -> bool:
                st = sc_state(HUB_SVC)
                obs(p, "failed_page_hub", st)
                expect(p, st["state"] == "STOPPED", "BantoHub resumed before the failed page was closed", st)
                return True

            child = subprocess.Popen([str(c.setup), "/P"])
            started = time.monotonic()
            try:
                vis.drive_file_locked_cancel(
                    child,
                    started,
                    PROOF_DIR,
                    lambda k, v: obs(p, k, v),
                    file_locked_dialog_matches,
                    on_shown,
                    while_shown,
                    ready_to_close,
                )
            except vis.VisibleFailure as e:
                raise PhaseFailure(p, e.reason, e.observation) from e
            rc = _wait_exit_within_30s(p, child)
            expect(p, rc != 0, "cancelled setup must exit non-zero", rc)
            _expect_hub_restarted(p, pid)
            resumed = sc_state(HUB_SVC)["pid"]
            time.sleep(3)
            st = sc_state(HUB_SVC)
            obs(p, "hub_after_wait", st)
            expect(p, st["state"] == "RUNNING" and st["pid"] == resumed, "BantoHub restarted again after the resume", st)
            sink = sc_state(SINK_SVC)
            obs(p, "sink_state", sink)
            expect(p, sink["state"] == "STOPPED", "Sink was started by the cancelled setup", sink)
            for name in SERVICES:
                _expect_registered(p, name)
            _expect_sink_still(p, tampered)
            _expect_data_intact(p, c)
            expect_no_shell(p)
        finally:
            _cleanup_visible(p, child)
    _recover_by_setup(p, c)


def phase_uninstall_locked_exe(c: Ctx) -> None:
    p = "10_uninstall_locked_exe"
    pid = _ensure_hub_running(p)
    before = tree_snapshot(INSTDIR)
    with ExclusiveFileLock(INSTDIR / "banto-hub-sink.exe"):
        rc, elapsed = timed(p, "locked_uninstall", lambda: _uninstall_in_place(p, "locked_uninstall"))
    expect(p, rc != 0, "uninstall over a locked exe must fail", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_hub_restarted(p, pid)
    for name in SERVICES:
        _expect_registered(p, name)
    _expect_still_installed(p, c, before)


def phase_visible_uninstall_locked_abort(c: Ctx) -> None:
    p = "10b_visible_uninstall_locked_abort"
    # `_?=` 付きの /P のアンインストーラ: 使用中の sink exe でフックが中止する道。主 MessageBox より先に
    # 止めたサービスを再開し（その結果が本文に入る）、un.onUninstFailed で二重に再開しない。
    pid = _visible_preamble(p)
    before = tree_snapshot(INSTDIR)
    resumed: dict = {}
    with ExclusiveFileLock(INSTDIR / FILE_LOCKED_EXE):
        child = None
        try:

            def same_pid_running() -> bool:
                st = sc_state(HUB_SVC)
                return st["state"] == "RUNNING" and st["pid"] == resumed.get("pid")

            def on_shown(lbl: str, elapsed: float) -> None:
                st = sc_state(HUB_SVC)
                obs(p, f"{lbl}_hub", st)
                expect(
                    p,
                    st["state"] == "RUNNING" and bool(st["pid"]) and st["pid"] != pid,
                    "BantoHub not resumed before the uninstall notice",
                    st,
                )
                resumed["pid"] = st["pid"]
                expect(p, tree_snapshot(INSTDIR) == before, "INSTDIR changed while the uninstall notice is shown")

            def while_shown(lbl: str) -> None:
                expect(p, same_pid_running(), "BantoHub changed while the uninstall notice is shown", sc_state(HUB_SVC))

            child = subprocess.Popen(uninstall_in_place_command(INSTDIR, "/P"))
            # uninstall.exe は保存された言語が無いと先に言語選択の窓を出す（MUI_UNGETLANGUAGE）。
            try:
                vis.dismiss_language_dialog(child, PROOF_DIR, lambda k, v: obs(p, k, v))
            except vis.VisibleFailure as e:
                raise PhaseFailure(p, e.reason, e.observation) from e
            started = time.monotonic()
            notices = [("uninstall_locked", uninstall_locked_notice_matches, on_shown, while_shown)]
            try:
                vis.drive_notices(child, started, PROOF_DIR, lambda k, v: obs(p, k, v), notices, same_pid_running)
            except vis.VisibleFailure as e:
                raise PhaseFailure(p, e.reason, e.observation) from e
            rc = _wait_exit_within_30s(p, child)
            expect(p, rc != 0, "aborted uninstall must exit non-zero", rc)
            st = sc_state(HUB_SVC)
            obs(p, "hub_after", st)
            expect(p, same_pid_running(), "BantoHub was restarted again after the resume (un.onUninstFailed)", st)
            status = wait_status()
            obs(p, "status_after", status)
            expect(p, status_initialized(status), "status not initialized after abort", status)
            time.sleep(3)
            st = sc_state(HUB_SVC)
            expect(p, same_pid_running(), "BantoHub pid changed after the status check", st)
            for name in SERVICES:
                _expect_registered(p, name)
            _expect_still_installed(p, c, before)
            expect_no_shell(p)
        finally:
            _cleanup_visible(p, child)


def phase_uninstall_sink_delete_fail(c: Ctx) -> None:
    p = "11_uninstall_sink_delete_fail"
    pid = _ensure_hub_running(p)
    before = tree_snapshot(INSTDIR)
    set_service_marker(SINK_SVC)
    expect(p, service_marker_present(SINK_SVC), "could not set the Sink marker")
    with DenyServiceDelete(SINK_SVC):
        rc, elapsed = timed(p, "uninstall", lambda: _uninstall_in_place(p, "uninstall"))
    expect(p, rc != 0, "uninstall must fail when the Sink cannot be deleted", rc)
    expect_not_stop_timeout(p, elapsed)
    _expect_hub_restarted(p, pid)
    # Sink の削除は拒まれたので、キーは作り直されていない。
    marker = service_marker_present(SINK_SVC)
    obs(p, "sink_marker_after", marker)
    expect(p, marker, "Sink registration was recreated although its deletion was denied")
    for name in SERVICES:
        _expect_registered(p, name)
    _expect_still_installed(p, c, before)


def phase_uninstall_hub_delete_fail(c: Ctx) -> None:
    p = "12_uninstall_hub_delete_fail"
    pid = _ensure_hub_running(p)
    before = tree_snapshot(INSTDIR)
    set_service_marker(SINK_SVC)
    expect(p, service_marker_present(SINK_SVC), "could not set the Sink marker")
    with DenyServiceDelete(HUB_SVC):
        rc = _uninstall_in_place(p, "uninstall")
    expect(p, rc != 0, "uninstall must fail when the Hub cannot be deleted", rc)
    _expect_hub_restarted(p, pid)
    # 目印が消えていれば、Sink は実際に解除されて install で戻された（解除の前に中止していない）。
    marker = service_marker_present(SINK_SVC)
    obs(p, "sink_marker_after", marker)
    expect(p, not marker, "Sink registration was not recreated (aborted before the unregister step?)")
    for name in SERVICES:
        _expect_registered(p, name)  # Sink は一度解除され、install + grant-service-acl で戻された登録
    sink = sc_state(SINK_SVC)
    obs(p, "sink_state_after", sink)
    expect(p, sink["state"] == "STOPPED", "restored Sink was started", sink)
    obs(p, "sink_sdshow", _sc_sdshow(SINK_SVC))
    _expect_still_installed(p, c, before)


def phase_uninstall_sink_delete_pending(c: Ctx) -> None:
    p = "13_uninstall_sink_delete_pending"
    pid = _ensure_hub_running(p)
    before = tree_snapshot(INSTDIR)
    with ServiceHandleHold(SINK_SVC):
        rc = _uninstall_in_place(p, "uninstall")
        flag = read_delete_flag(SINK_SVC)
        obs(p, "sink_delete_flag", flag)
        expect(p, rc != 0, "uninstall must abort on a pending Sink deletion", rc)
        expect(p, flag == 1, "Sink DeleteFlag not set", flag)
        _expect_hub_restarted(p, pid)
        _expect_registered(p, HUB_SVC)
        _expect_still_installed(p, c, before)
    # service_key_exists はレジストリだけを見るので、追加のハンドルを持たない
    gone = wait_until(lambda: not service_key_exists(SINK_SVC), 60)
    obs(p, "sink_key_gone", bool(gone))
    expect(p, bool(gone), "Sink registration did not disappear after the handle closed")
    _recover_by_setup(p, c)


def phase_uninstall_hub_delete_pending(c: Ctx) -> None:
    p = "14_uninstall_hub_delete_pending"
    _ensure_hub_running(p)  # 保留中の Hub は再開されないので PID の比較はしない
    before = tree_snapshot(INSTDIR)
    with ServiceHandleHold(HUB_SVC):
        rc = _uninstall_in_place(p, "uninstall")
        flag = read_delete_flag(HUB_SVC)
        obs(p, "hub_delete_flag", flag)
        expect(p, rc != 0, "uninstall must abort on a pending Hub deletion", rc)
        expect(p, flag == 1, "Hub DeleteFlag not set", flag)
        _expect_registered(p, SINK_SVC)
        hub = sc_state(HUB_SVC)
        obs(p, "hub_state_pending", hub)
        expect(p, hub["state"] != "RUNNING", "Hub with a pending deletion was resumed", hub)
        _expect_still_installed(p, c, before)
        r = run([str(INSTDIR / "banto-hub-elev.exe"), "service-install"])
        obs(p, "service_install_while_pending", {"exit": r.returncode, "stderr_tail": r.stderr[-300:]})
        expect(p, r.returncode != 0, "service-install must refuse a pending deletion", r.returncode)
        flag = read_delete_flag(HUB_SVC)
        obs(p, "hub_delete_flag_after_install", flag)
        expect(p, flag == 1, "Hub DeleteFlag changed by service-install", flag)
    gone = wait_until(lambda: not service_key_exists(HUB_SVC), 60)
    obs(p, "hub_key_gone", bool(gone))
    expect(p, bool(gone), "Hub registration did not disappear after the handle closed")
    # `_?=` なしの本物の呼び方。Hub の未登録を飛ばして最後まで進む。
    r = run([str(INSTDIR / "uninstall.exe"), "/S"], timeout=600)
    obs(p, "uninstall_parent_exit", r.returncode)
    _wait_uninstall_done(p)
    expect(p, SQLITE.is_file(), "sqlite was removed")
    _expect_data_intact(p, c)
    rc = run_setup(c.setup, [], p, "reinstall")
    expect(p, rc == 0, "reinstall after the pending deletion failed", rc)
    expect_services_registered_stopped(p)
    _ensure_hub_running(p)
    status = wait_status()
    obs(p, "status", status)
    expect(p, status_initialized(status), "status not initialized after reinstall", status)
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
    phase_visible_file_locked_cancel,
    phase_uninstall_locked_exe,
    phase_visible_uninstall_locked_abort,
    phase_uninstall_sink_delete_fail,
    phase_uninstall_hub_delete_fail,
    phase_uninstall_sink_delete_pending,
    phase_uninstall_hub_delete_pending,
)


# ---------------------------------------------------------------- cleanup / main

def cleanup() -> int:
    """このランのサービスを止めて解除する。失敗しても最後まで続ける。"""
    for name in SERVICES:
        try:
            run(["sc", "stop", name])
            wait_until(lambda n=name: sc_state(n)["state"] in (None, "STOPPED"), 30)
        except Exception as e:  # noqa: BLE001
            print(f"cleanup: stop {name}: {e!r}")
    for exe in EXES:
        try:
            run(["taskkill", "/F", "/IM", exe])
        except Exception as e:  # noqa: BLE001
            print(f"cleanup: taskkill {exe}: {e!r}")
    for name in SERVICES:
        try:
            r = run(["sc", "delete", name])
            print(f"cleanup: sc delete {name} -> {r.returncode}")
        except Exception as e:  # noqa: BLE001
            print(f"cleanup: delete {name}: {e!r}")
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
    for name in SERVICES:
        if sc_state(name)["state"] is not None:
            print(f"REFUSED: {name} already registered on this runner")
            return 3
    password = random_password()
    EVIDENCE["setup"] = {"name": setup.name, "sha256": sha256(setup), "version": version}
    ctx = Ctx(setup, version, dist, password)
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
    assert not evidence_has_secret(EVIDENCE, password)
    EVIDENCE["result"] = "pass"
    save_evidence()
    print("ALL PHASES PASSED")
    return 0


if __name__ == "__main__":
    sys.exit(main())
