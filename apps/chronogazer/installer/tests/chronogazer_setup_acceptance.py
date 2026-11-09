#!/usr/bin/env python3
"""ChronoGazer インストーラ（本物の setup.exe）の実 SCM 受入試験（#973 残件A、silent の通常系だけ）。

!!! 開発 PC では絶対に実行しない !!!
このスクリプトは実 `C:\\Program Files\\ChronoGazer`・`C:\\ProgramData\\ChronoGazer`・
ChronoGazer サービスを書き換え、アンインストールまで行う。
使い捨ての GitHub-hosted Windows ランナーでだけ
`.github/workflows/installer-chronogazer-acceptance.yml` から走らせる。ローカルで走らせて
よいのは `test_chronogazer_setup_acceptance.py`（純粋部分のユニットテスト）だけ。

フェーズ（各 `phase_*` は独立の関数。すべて silent の setup / uninstall）:
  1 拒否 / 2 新規 / 3 起動と種まき（DB の番兵表＋番兵ファイル） /
  4 ACL を弱めて稼働中の上書き / 5 停止中の上書き（AUTO_START を保つ） /
  6 稼働中のアンインストール / 7 再インストール
各フェーズの観測を証拠 JSON（proof/evidence.json）に残し、期待と違えばフェーズ名と
観測を出して失敗する。

対象外（このスクリプトは確かめない）: 故障系（使用中 exe・ACL・事前確認での中止、
アンインストールの中止、DeleteFlag 保留）・service-install の失敗・破損 exe・
可視 UI とシェル 90 秒の問い（MessageBox）・停止 timeout・再開失敗・実 collector・
#392 B3-5。silent なので MessageBox と二重再開は未確認。

標準ライブラリのみ。
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import secrets
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

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
    """製品（シェルの起動判定）と同じ要求で Healthy を待つ: endpoint.json の port へ
    `GET /api/host`（X-Banto-Client 付き）が 200 かつ product/hostKind/scope が一致。"""
    last: list = []

    def p():
        try:
            ep = parse_endpoint(ENDPOINT_FILE.read_text(encoding="utf-8"))
            code, body = http_get_json(f"http://127.0.0.1:{ep['port']}{HOST_PATH}")
            last[:] = [ep, code, body]
            return {"endpoint": ep, "body": body} if host_healthy(code, body, ep["scope"]) else None
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


def expect(phase: str, cond: bool, message: str, observation=None) -> None:
    if not cond:
        raise PhaseFailure(phase, message, observation)


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


PHASES = (
    phase_reject,
    phase_fresh,
    phase_start_seed,
    phase_acl_overwrite,
    phase_stopped_overwrite,
    phase_uninstall_running,
    phase_reinstall,
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
