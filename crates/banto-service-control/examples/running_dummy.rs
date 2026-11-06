//! Disposable GitHub-hosted Windows runner only. Never run locally.
//! #973 lock-abort 試験用: 実際に Running になり、停止の合図が来るまで動き続けるダミーサービス。
//!
//! `delete_pending_probe`（「起動しない」契約。run-service は断る）とは別物で、こちらは
//! `run-service` で SCM から起動され、Running を報告し、500ms ごとに heartbeat を書く。
//! 試験の観測（PID の交代・heartbeat の断続・停止の合図）は `<root>\dummy\*.jsonl` に残す。
//! install/uninstall の guard は probe のものを複製している（共有化は別 PR）。
//! SCM 起動の子には試験の環境変数が継承されないため、`run-service` は ImagePath の引数
//! （run-id・sha）と `<root>\.banto-973-ci`・`binding.json`・自身の SHA-256 の束縛で
//! 使い捨て runner の試験ビルドであることを確かめ、満たさなければ Running を報告せず終わる。

use serde::{Deserialize, Serialize};

/// `binding.json` の中身。未知キーは拒否し、重複キーは serde が `duplicate field` で拒否する。
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(not(windows), allow(dead_code))]
struct Binding {
    run_id: String,
    sha: String,
    root: String,
    exe_sha256: String,
    service_name: String,
    image_path: String,
}

/// `binding.json` と完全一致で比べる期待値。
#[cfg_attr(not(windows), allow(dead_code))]
struct Expected<'a> {
    run_id: &'a str,
    sha: &'a str,
    root: &'a str,
    exe_sha256: &'a str,
    service_name: &'a str,
    image_path: &'a str,
}

/// 型付きで解析し、トップレベルの各項目を完全一致で比べる（部分文字列では見ない）。
#[cfg_attr(not(windows), allow(dead_code))]
fn check_binding(text: &str, expected: &Expected) -> Result<(), String> {
    let b: Binding = serde_json::from_str(text).map_err(|e| format!("binding.json parse: {e}"))?;
    for (key, got, want) in [
        ("run_id", &b.run_id, expected.run_id),
        ("sha", &b.sha, expected.sha),
        ("root", &b.root, expected.root),
        ("exe_sha256", &b.exe_sha256, expected.exe_sha256),
        ("service_name", &b.service_name, expected.service_name),
        ("image_path", &b.image_path, expected.image_path),
    ] {
        if got != want {
            return Err(format!("binding.json mismatch: {key}"));
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("running_dummy requires a disposable GitHub-hosted Windows runner");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    if let Err(error) = windows::run() {
        eprintln!("running-dummy-error: {error}");
        std::process::exit(2);
    }
}

#[cfg(windows)]
mod windows {
    use super::{check_binding, Binding, Expected};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use banto_service_control::{
        dispatch_failure_lines, install_service, run_service, uninstall_service, InstallError,
        InstallKind, ServiceContext, ServiceExit, ServiceProgram, ServiceSpec, ServiceStartKind,
        StopSignal,
    };
    use sha2::{Digest, Sha256};

    const SERVICE_NAME: &str = "ChronoGazer";
    const HEARTBEAT: Duration = Duration::from_millis(500);
    /// 試験の暴走を止める見張り。起動からこの時間で自分から止まる。
    const WATCHDOG: Duration = Duration::from_secs(20 * 60);

    #[link(name = "shell32")]
    unsafe extern "system" {
        fn IsUserAnAdmin() -> i32;
    }

    fn env(name: &str) -> Result<String, String> {
        std::env::var(name).map_err(|_| format!("missing {name}"))
    }

    /// `canonicalize` が返す `\\?\C:\...` を `C:\...` に戻す（`\\?\UNC\` は触らない）。
    /// SCM の ImagePath・binding.json・実行時の比較をすべて同じ形に揃えるために使う。
    fn plain(path: PathBuf) -> PathBuf {
        match path.to_str() {
            Some(s) if s.starts_with(r"\\?\") && !s.starts_with(r"\\?\UNC\") => {
                PathBuf::from(&s[4..])
            }
            _ => path,
        }
    }

    fn within(path: &Path, root: &Path) -> Result<PathBuf, String> {
        let resolved = plain(std::fs::canonicalize(path).map_err(|error| error.to_string())?);
        if resolved == root || !resolved.starts_with(root) || !resolved.is_file() {
            return Err("executable outside disposable root".into());
        }
        Ok(resolved)
    }

    /// probe の `guard()` の複製。加えて実行ファイルの親ディレクトリ名が `dummy` であること。
    fn guard() -> Result<PathBuf, String> {
        for (key, value) in [
            ("GITHUB_ACTIONS", "true"),
            ("RUNNER_ENVIRONMENT", "github-hosted"),
            ("RUNNER_OS", "Windows"),
            ("BANTO_973_SCM_TEST", "github-hosted-disposable-only-v1"),
        ] {
            if env(key)? != value {
                return Err(format!("refused environment: {key}"));
            }
        }
        // SAFETY: read-only check of this process's token; no elevation is requested.
        if unsafe { IsUserAnAdmin() } == 0 {
            return Err("administrator token required".into());
        }
        let temp = plain(std::fs::canonicalize(env("RUNNER_TEMP")?).map_err(|e| e.to_string())?);
        let raw = PathBuf::from(env("BANTO_973_SCM_ROOT")?);
        let root = plain(std::fs::canonicalize(&raw).map_err(|e| e.to_string())?);
        if root == temp || !root.starts_with(&temp) || !root.is_dir() {
            return Err("invalid disposable root".into());
        }
        use std::os::windows::fs::MetadataExt;
        let mut ancestor = raw.as_path();
        loop {
            if std::fs::symlink_metadata(ancestor)
                .map_err(|e| e.to_string())?
                .file_attributes()
                & 0x400
                != 0
            {
                return Err("reparse point in disposable root".into());
            }
            if plain(std::fs::canonicalize(ancestor).map_err(|e| e.to_string())?) == temp {
                break;
            }
            ancestor = ancestor.parent().ok_or("root ancestor missing")?;
        }
        let marker = format!(
            "{}\n{}\n",
            env("GITHUB_RUN_ID")?,
            env("BANTO_973_EXPECTED_SHA")?
        );
        if std::fs::read_to_string(root.join(".banto-973-ci")).map_err(|e| e.to_string())? != marker
        {
            return Err("disposable marker mismatch".into());
        }
        let me = within(&std::env::current_exe().map_err(|e| e.to_string())?, &root)?;
        if me
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            != Some("dummy")
        {
            return Err("executable not in <root>\\dummy".into());
        }
        Ok(root)
    }

    fn wall_ms() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }

    /// JSON 文字列リテラル（パスの `\` と `"` だけ気にすればよい入力）。
    fn js(value: &str) -> String {
        let mut out = String::from("\"");
        for c in value.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// 1 行を追記して即 flush（Python が並行に読める共有モード。std の既定は READ|WRITE|DELETE）。
    fn append(dir: &Path, file: &str, line: &str) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(file))?;
        f.write_all(line.as_bytes())?;
        f.write_all(b"\n")?;
        f.flush()
    }

    fn sha256_file(path: &Path) -> Result<String, String> {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        Ok(Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect())
    }

    fn image_path(exe: &Path, run_id: &str, sha: &str) -> String {
        format!(
            "\"{}\" run-service --run-id {run_id} --sha {sha}",
            exe.display()
        )
    }

    /// 引数 `--run-id <id> --sha <sha>` の取り出し（これ以外の形は断る）。
    fn parse_run_args(rest: &[String]) -> Result<(String, String), String> {
        match rest {
            [a, id, b, sha] if a == "--run-id" && b == "--sha" => Ok((id.clone(), sha.clone())),
            _ => Err("usage: run-service --run-id <id> --sha <sha>".into()),
        }
    }

    struct Dummy {
        run_id: String,
        sha: String,
        dir: PathBuf,
    }

    impl Dummy {
        fn note_error(&self, line: &str) {
            let _ = append(&self.dir, "errors.log", &format!("{} {line}", wall_ms()));
        }

        fn refuse(&self, reason: &str) -> ServiceExit {
            self.note_error(&format!("refused: {reason}"));
            let _ = append(
                &self.dir,
                "starts.jsonl",
                &format!(
                    "{{\"event\":\"refused\",\"pid\":{},\"reason\":{},\"wall_ms\":{}}}",
                    std::process::id(),
                    js(reason),
                    wall_ms()
                ),
            );
            ServiceExit::Specific(3)
        }

        /// 束縛検証 1〜4（5 は `run_service` の戻り値で `run()` の外）。
        /// 通れば (root, 自身の SHA-256)。
        fn verify(&self, ctx: &ServiceContext) -> Result<(PathBuf, String), String> {
            // 1. current_exe 正規化 → 親 dir 名 dummy、root 名が Banto973- で始まる。
            let me = plain(
                std::fs::canonicalize(std::env::current_exe().map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?,
            );
            let dir = me.parent().ok_or("exe has no parent")?;
            if dir.file_name().and_then(|n| n.to_str()) != Some("dummy") || dir != self.dir {
                return Err("exe parent is not <root>\\dummy".into());
            }
            let root = dir.parent().ok_or("dummy has no parent")?.to_path_buf();
            if !root
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("Banto973-"))
            {
                return Err("root name does not start with Banto973-".into());
            }
            // 2. marker の 1 行目 == --run-id、2 行目 == --sha。
            let marker =
                std::fs::read_to_string(root.join(".banto-973-ci")).map_err(|e| e.to_string())?;
            let mut lines = marker.lines();
            if lines.next() != Some(self.run_id.as_str()) || lines.next() != Some(self.sha.as_str())
            {
                return Err("marker does not match run-id/sha".into());
            }
            // 3. binding.json の run_id/sha/root/exe_sha256 が 1〜2 と自身の SHA-256 に一致。
            let exe_sha = sha256_file(&me)?;
            let binding = std::fs::read_to_string(dir.join("binding.json"))
                .map_err(|e| format!("binding.json: {e}"))?;
            let root_text = root.display().to_string();
            let image = image_path(&me, &self.run_id, &self.sha);
            check_binding(
                &binding,
                &Expected {
                    run_id: &self.run_id,
                    sha: &self.sha,
                    root: &root_text,
                    exe_sha256: &exe_sha,
                    service_name: SERVICE_NAME,
                    image_path: &image,
                },
            )?;
            // 4. start_args[0] == サービス名。
            match ctx.start_args().first() {
                Some(first) if first == SERVICE_NAME => {}
                _ => return Err("start_args[0] is not the service name".into()),
            }
            Ok((root, exe_sha))
        }
    }

    impl ServiceProgram for Dummy {
        fn service_name(&self) -> &str {
            SERVICE_NAME
        }

        fn label(&self) -> &str {
            "running-dummy"
        }

        fn log_error(&self, line: &str) {
            self.note_error(line);
        }

        fn run(&self, ctx: &ServiceContext) -> ServiceExit {
            let (root, exe_sha) = match self.verify(ctx) {
                Ok(v) => v,
                Err(reason) => return self.refuse(&reason),
            };
            let pid = std::process::id();
            let args: Vec<String> = ctx
                .start_args()
                .iter()
                .map(|a| js(&a.to_string_lossy()))
                .collect();
            let _ = append(
                &self.dir,
                "starts.jsonl",
                &format!(
                    "{{\"event\":\"start\",\"pid\":{pid},\"run_id\":{},\"sha\":{},\"root\":{},\"exe_sha256\":{},\"args\":[{}],\"wall_ms\":{}}}",
                    js(&self.run_id),
                    js(&self.sha),
                    js(&root.display().to_string()),
                    js(&exe_sha),
                    args.join(","),
                    wall_ms()
                ),
            );
            if !ctx.report_running() {
                self.note_error("report_running returned false");
            }
            let started = Instant::now();
            let mut seq: u64 = 0;
            loop {
                std::thread::sleep(HEARTBEAT);
                seq += 1;
                let _ = append(
                    &self.dir,
                    "heartbeat.jsonl",
                    &format!(
                        "{{\"seq\":{seq},\"pid\":{pid},\"run_id\":{},\"sha\":{},\"wall_ms\":{}}}",
                        js(&self.run_id),
                        js(&self.sha),
                        wall_ms()
                    ),
                );
                if let Some(notice) = ctx.stop_notice() {
                    let signal = match notice.signal {
                        StopSignal::ServiceStop => "Stop",
                        StopSignal::OsPreshutdown => "Preshutdown",
                    };
                    let _ = append(
                        &self.dir,
                        "stops.jsonl",
                        &format!(
                            "{{\"event\":\"stop\",\"pid\":{pid},\"signal\":\"{signal}\",\"seq_at_stop\":{seq},\"wall_ms\":{}}}",
                            wall_ms()
                        ),
                    );
                    return ServiceExit::Success;
                }
                if started.elapsed() >= WATCHDOG {
                    let _ = append(
                        &self.dir,
                        "starts.jsonl",
                        &format!(
                            "{{\"event\":\"watchdog\",\"pid\":{pid},\"seq\":{seq},\"wall_ms\":{}}}",
                            wall_ms()
                        ),
                    );
                    return ServiceExit::Specific(4);
                }
            }
        }
    }

    pub fn run() -> Result<(), String> {
        let args: Vec<String> = std::env::args().skip(1).collect();
        match args.split_first() {
            // SCM が ImagePath から起動する。試験の環境変数は無いので、束縛は run() 側で検証する。
            Some((action, rest)) if action == "run-service" => {
                let (run_id, sha) = parse_run_args(rest)?;
                let me = std::env::current_exe().map_err(|e| e.to_string())?;
                // verify と同じ形（正規化 → plain）。失敗時は非正規の親で、verify が断る。
                let me = std::fs::canonicalize(&me).map(plain).unwrap_or(me);
                let dir = me.parent().ok_or("exe has no parent")?.to_path_buf();
                if let Err(error) = run_service(Dummy { run_id, sha, dir }) {
                    // 5. SCM の外で起動された（runner の既存の形: 2 行を stderr、exit 1）。
                    for line in dispatch_failure_lines("running-dummy", &error) {
                        eprintln!("{line}");
                    }
                    std::process::exit(1);
                }
                Ok(())
            }
            Some((action, [])) if action == "install" || action == "uninstall" => {
                let root = guard()?;
                let owned = std::fs::read_to_string(root.join("owned-services.txt"))
                    .map_err(|e| e.to_string())?;
                if !owned.lines().any(|line| line == SERVICE_NAME) {
                    return Err("service not in this run's ownership ledger".into());
                }
                if action == "install" {
                    install(&root)
                } else {
                    let result = uninstall_service(SERVICE_NAME);
                    println!("uninstall-result={result:?}");
                    if result.is_err() {
                        std::process::exit(1);
                    }
                    Ok(())
                }
            }
            _ => Err("usage: install | uninstall | run-service --run-id <id> --sha <sha>".into()),
        }
    }

    fn install(root: &Path) -> Result<(), String> {
        let run_id = env("GITHUB_RUN_ID")?;
        let sha = env("BANTO_973_EXPECTED_SHA")?;
        let exe = within(&std::env::current_exe().map_err(|e| e.to_string())?, root)?;
        let binding = serde_json::to_string(&Binding {
            run_id: run_id.clone(),
            sha: sha.clone(),
            root: root.display().to_string(),
            exe_sha256: sha256_file(&exe)?,
            service_name: SERVICE_NAME.into(),
            image_path: image_path(&exe, &run_id, &sha),
        })
        .map_err(|e| e.to_string())?
            + "
";
        // 登録より先に束縛を書く（run-service が起動時に読む）。
        std::fs::write(root.join("dummy").join("binding.json"), binding)
            .map_err(|e| format!("binding.json: {e}"))?;
        let result = install_service(&ServiceSpec {
            name: SERVICE_NAME,
            display_name: "ChronoGazer (Banto973 running dummy)",
            description: Some("Disposable #973 running dummy; restart proof"),
            executable: exe,
            launch_arguments: vec![
                "run-service".into(),
                "--run-id".into(),
                run_id.into(),
                "--sha".into(),
                sha.into(),
            ],
            start_kind: ServiceStartKind::OnDemand,
            preshutdown_timeout: Duration::from_secs(30),
        });
        println!("install-result={result:?}");
        println!(
            "{}",
            match &result {
                Ok(outcome) => match outcome.kind {
                    InstallKind::Created => "install-kind=Created",
                    InstallKind::AlreadyRegistered => "install-kind=AlreadyRegistered",
                },
                Err(InstallError::DeletePending) => "install-error=DeletePending",
                Err(_) => "install-error=Other",
            }
        );
        if result.is_err() {
            std::process::exit(1);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"run_id":"r1","sha":"s1","root":"C:/R","exe_sha256":"abc","service_name":"ChronoGazer","image_path":"\"x\" run-service"}"#;

    fn exp() -> Expected<'static> {
        Expected {
            run_id: "r1",
            sha: "s1",
            root: "C:/R",
            exe_sha256: "abc",
            service_name: "ChronoGazer",
            image_path: "\"x\" run-service",
        }
    }

    #[test]
    fn accepts_exact_binding() {
        check_binding(GOOD, &exp()).unwrap();
    }

    #[test]
    fn rejects_truncated_json() {
        let cut = GOOD.find("\"abc\"").unwrap() + 5;
        assert!(check_binding(&GOOD[..cut], &exp()).is_err());
    }

    #[test]
    fn rejects_expected_only_in_nested_value() {
        let t = r#"{"run_id":"r1","sha":"s1","root":"C:/R","exe_sha256":{"exe_sha256":"abc"},"service_name":"ChronoGazer","image_path":"\"x\" run-service"}"#;
        assert!(check_binding(t, &exp()).is_err());
    }

    #[test]
    fn rejects_missing_field() {
        let t = GOOD.replace("\"sha\":\"s1\",", "");
        assert!(check_binding(&t, &exp()).is_err());
    }

    #[test]
    fn rejects_duplicate_key() {
        let t = GOOD.replace(
            "{\"run_id\":\"r1\",",
            "{\"run_id\":\"r1\",\"run_id\":\"r1\",",
        );
        let e = check_binding(&t, &exp()).unwrap_err();
        assert!(e.contains("duplicate field"), "{e}");
    }

    #[test]
    fn rejects_unknown_key() {
        let t = GOOD.replace("{", "{\"extra\":\"1\",");
        assert!(check_binding(&t, &exp()).is_err());
    }

    #[test]
    fn rejects_each_mismatch() {
        for (key, field) in [
            ("run_id", "r1"),
            ("sha", "s1"),
            ("exe_sha256", "abc"),
            ("service_name", "ChronoGazer"),
        ] {
            let t = GOOD.replace(
                &format!("\"{key}\":\"{field}\""),
                &format!("\"{key}\":\"zz\""),
            );
            let e = check_binding(&t, &exp()).unwrap_err();
            assert!(e.contains(key), "{key}: {e}");
        }
        let t = GOOD.replace("C:/R", "C:/Z");
        assert!(check_binding(&t, &exp()).unwrap_err().contains("root"));
        let t = GOOD.replace("run-service", "run-other");
        assert!(check_binding(&t, &exp())
            .unwrap_err()
            .contains("image_path"));
    }
}
