#!/usr/bin/env python3
"""Hub 一体インストーラ（本物の setup.exe）の実 SCM 受入試験（#973 残件A、通常系）。

!!! 開発 PC では絶対に実行しない !!!
このスクリプトは実 `C:\\Program Files\\BantoHub`・`C:\\ProgramData\\BantoHub`・
BantoHub / BantoHubSink サービスを書き換え、アンインストールまで行う。
使い捨ての GitHub-hosted Windows ランナーでだけ
`.github/workflows/installer-hub-acceptance.yml` から走らせる。ローカルで走らせて
よいのは `test_hub_setup_acceptance.py`（純粋部分のユニットテスト）だけ。

フェーズ（各 `phase_*` は独立の関数。故障系は後続 PR で関数を足す）:
  1 拒否 / 2 新規 / 3 起動と種まき / 4 ACL を弱めて稼働中の上書き /
  5 停止中の上書き / 6 稼働中のアンインストール / 7 再インストール
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
    req = urllib.request.Request(
        url, data=data, headers={"Content-Type": "application/json"} if data else {}
    )
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
            "powershell",
            "-NoProfile",
            "-Command",
            f"(Get-Acl -LiteralPath '{path}').Owner",
        ]
    )
    return r.stdout.strip()


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

    outside = Path(os.environ["RUNNER_TEMP"]) / "hub-junction-outside"
    outside.mkdir(parents=True, exist_ok=True)
    (outside / "keep.txt").write_text("keep", encoding="utf-8")
    before = tree_hashes(outside)
    run(["cmd", "/c", "mklink", "/J", str(INSTDIR), str(outside)], check=True)
    try:
        rc = run_setup(c.setup, [], p, "junction")
        expect(p, rc != 0, "setup over a junction must fail", rc)
        after = tree_hashes(outside)
        obs(p, "outside_before", before)
        obs(p, "outside_after", after)
        expect(p, before == after, "outside folder changed", after)
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


def phase_uninstall_running(c: Ctx) -> None:
    p = "6_uninstall_running"
    run(["sc", "start", HUB_SVC])
    wait_state(HUB_SVC, "RUNNING")
    wait_status()
    # `_?=` を付けない実際の呼び方。親は子（temp にコピーされた本体）を残してすぐ戻る。
    r = run([str(INSTDIR / "uninstall.exe"), "/S"], timeout=600)
    obs(p, "uninstall_parent_exit", r.returncode)

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
