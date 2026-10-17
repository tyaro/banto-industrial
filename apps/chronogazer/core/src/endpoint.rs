//! `run\endpoint.json` の読み書き（#392 段階 B の B1-4、設計書 §3.1(b)）。
//!
//! 待ち受け中のエンジン（`chronogazer-server`。B1-5 が書く）が、自分の所在を
//! 1 つのファイルに残す。利用者として動くシェル（B2-4）は SCM の状態に加えて
//! このファイルを読み、**どの port に `GET /api/host` の probe を打つか**を決める
//! （port は設定で変えられるため）。書くのは `chronogazer-server`（#392 B1-5、
//! `crate::server_host::start` が起動の後に書き、終了の最初に消す）。
//! ファイルの ACL（Users に読み取り可）は B3-3 の担当で、ここは読み書きだけ。
//!
//! ```json
//! {"schema":1,"port":8721,"pid":4321,"scope":"0123456789abcdef","version":"0.1.0","hostKind":"service"}
//! ```
//!
//! - `port`: loopback の listener の port（常に開く方。LAN の listener ではない）。
//! - `scope`: [`crate::instance_lock::InstanceGuard::scope`]（DB のパスのハッシュ。
//!   パスそのものは載せない）。シェルは `/api/host` の `scope` と突き合わせる。
//! - `hostKind`: [`crate::instance_lock::ChronoGazerHost::as_str`]。
//!
//! # 書き込みは原子的
//!
//! 同じフォルダに一時ファイルを作って書き出し（`sync_all`）、`rename` で置き換える
//! （Windows の `std::fs::rename` は既存の宛先を置き換える）。読む側（シェル）が
//! 書きかけの JSON を見ることはない。途中で失敗した一時ファイルは消す。
//!
//! # 読む側は「無い」と「壊れた・古い」を区別しない
//!
//! [`read_endpoint`] は、ファイルが無い・読めない・JSON として壊れている・必須の値が
//! 不正（`port == 0`、`pid == 0`）のどれでも `None`。[`read_live_endpoint`] は更に、
//! 書いたプロセス（`pid`）が生きていなければ `None`（クラッシュで消し損ねた古い
//! ファイル）。pid は再利用されるので、生きていても**そのプロセスが書いたとは
//! 限らない** - 最終的な確認は probe（`/api/host` の product・scope）が行う。
//! 終了時は [`clear_endpoint`] が**自分の pid のファイルだけ**消す（別のプロセスが
//! 書き直したものを消さない）。消せなかった・読めなかった・壊れていたときは
//! [`EndpointClear`] で「消した・無い・別のプロセスのもの」と区別して返し、終了の
//! 側がログとコンソールに 1 行残す（#599。終了コードは変えない）。残ったファイルは
//! 書いたプロセスが生きていないので、読む側は古いものとして読み飛ばし、次の起動が
//! 置き換えを試みる。原因が属性・ACL のように続くものなら置き換えも失敗するので、
//! 1 行には手で消す案内を入れる（[`EndpointClear::diagnostic`]）。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// `run` フォルダの中のファイル名。
pub const ENDPOINT_FILE_NAME: &str = "endpoint.json";

/// 形式の版。読む側は知らない版でも既知のフィールドが揃っていれば読む。
pub const ENDPOINT_SCHEMA: u32 = 1;

/// 読む側が受け付ける大きさの上限（本物は 200 バイト程度）。
const MAX_ENDPOINT_BYTES: u64 = 64 * 1024;

/// `endpoint.json` の中身。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointInfo {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub port: u16,
    pub pid: u32,
    pub scope: String,
    pub version: String,
    pub host_kind: String,
}

fn default_schema() -> u32 {
    ENDPOINT_SCHEMA
}

impl EndpointInfo {
    /// 今のプロセス（pid）の所在。
    pub fn for_current_process(
        port: u16,
        scope: impl Into<String>,
        host_kind: impl Into<String>,
    ) -> Self {
        Self {
            schema: ENDPOINT_SCHEMA,
            port,
            pid: std::process::id(),
            scope: scope.into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            host_kind: host_kind.into(),
        }
    }

    fn is_plausible(&self) -> bool {
        self.port != 0 && self.pid != 0
    }
}

/// `path`（`run\endpoint.json`）へ原子的に書く。`run` フォルダが無ければ作る。
pub fn write_endpoint(path: &Path, info: &EndpointInfo) -> std::io::Result<()> {
    write_endpoint_with(path, info, |_| Ok(()))
}

/// 書き込みの段階。テストが途中の失敗を注入するための印。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    TempWritten,
}

fn write_endpoint_with(
    path: &Path,
    info: &EndpointInfo,
    hook: impl Fn(Stage) -> std::io::Result<()>,
) -> std::io::Result<()> {
    static NONCE: AtomicU64 = AtomicU64::new(0);
    let dir = match path.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?
        .to_string_lossy();
    let temp: PathBuf = dir.join(format!(
        ".{file_name}.{}-{}.tmp",
        std::process::id(),
        NONCE.fetch_add(1, Ordering::Relaxed)
    ));
    let bytes = serde_json::to_vec(info).map_err(std::io::Error::other)?;

    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        hook(Stage::TempWritten)?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// `path` を読んだ結果（[`clear_endpoint`] が「無い」「読めない」「所有者を
/// 確かめられない」を分けるため）。
enum Loaded {
    /// ファイルが無い。
    Missing,
    /// 読めたが、大きすぎる・JSON として壊れている・値が不正。
    Unrecognized,
    Info(EndpointInfo),
}

/// `path` を読む。無いのは `Ok(Missing)`、開けない・読めないのは `Err`。
fn load_endpoint(path: &Path) -> std::io::Result<Loaded> {
    use std::io::Read;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Loaded::Missing),
        Err(err) => return Err(err),
    };
    let mut bytes = Vec::new();
    file.take(MAX_ENDPOINT_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_ENDPOINT_BYTES {
        return Ok(Loaded::Unrecognized);
    }
    Ok(match serde_json::from_slice::<EndpointInfo>(&bytes) {
        Ok(info) if info.is_plausible() => Loaded::Info(info),
        _ => Loaded::Unrecognized,
    })
}

/// `path` を読む。無い・読めない・壊れている・値が不正なら `None`。
pub fn read_endpoint(path: &Path) -> Option<EndpointInfo> {
    match load_endpoint(path) {
        Ok(Loaded::Info(info)) => Some(info),
        _ => None,
    }
}

/// [`read_endpoint`] に加え、書いたプロセスが生きていなければ `None`
/// （[`banto_instance_lock::process_is_alive`]）。
pub fn read_live_endpoint(path: &Path) -> Option<EndpointInfo> {
    read_live_endpoint_with(path, banto_instance_lock::process_is_alive)
}

/// 生存確認を差し替えられる版（テスト用に公開）。
pub fn read_live_endpoint_with(
    path: &Path,
    is_alive: impl Fn(u32) -> bool,
) -> Option<EndpointInfo> {
    read_endpoint(path).filter(|info| is_alive(info.pid))
}

/// [`clear_endpoint`] の結果（#599）。「書いていない・既に無い」「別のプロセスのもの
/// なので残した」と、「消せなかった」「読めなかった」を分ける。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointClear {
    /// 自分のファイルを消した。
    Removed,
    /// ファイルが無い（書いていない・既に消えている）。
    Absent,
    /// 別のプロセス（`pid`）が書いたものなので残した。
    OtherOwner { pid: u32 },
    /// 中身を読み取れない（大きすぎる・JSON として壊れている・値が不正）。所有者を
    /// 確かめられないので消さずに残した（次に書く側が置き換える）。
    Unrecognized,
    /// 開けない・読めない（権限・共有違反など）。所有者を確かめられないので消して
    /// いない。中身は OS のエラーの文。
    ReadFailed(String),
    /// 自分のファイルだったが消せなかった（読み取り専用・ACL・共有違反など）。
    RemoveFailed(String),
}

impl EndpointClear {
    /// 終了の後にファイルを残したことを知らせる 1 行（`path` は表示用）。知らせなくて
    /// よい結果（消した・無い・別のプロセスのもの）は `None`。
    ///
    /// 残ったファイルは、読む側（[`read_live_endpoint`]）が書いたプロセスの pid が
    /// 生きていないので古いものとして読み飛ばし、次の起動の [`write_endpoint`] が
    /// 置き換えを試みる。そのため終了コードは変えない
    /// （`server_host::ServerShutdownSummary::exit_code`）。
    ///
    /// **自動で直るとは約束しない**（PR #649 の Codex P2）: 消せなかった・読めなかった
    /// 原因が読み取り専用の属性や ACL のように続くものなら、次の起動の置き換え（rename）
    /// も同じ理由で失敗し、サーバーは起動時に「書けなかった」の行を出して動き続けるが、
    /// シェルは古い pid のファイルを断るので生きているポートを見つけられない。そのため
    /// 文言は「置き換えを試みる・できなければ属性と権限を確かめて手で消す」にし、OS の
    /// エラーがあれば同じ 1 行に入れる。
    pub fn diagnostic(&self, path: &Path) -> Option<String> {
        let path = path.display();
        let advice = format!(
            "次の起動で置き換えを試みます。置き換えられないときは、ファイルの属性・権限を確かめて、{path} を手で消してください"
        );
        match self {
            Self::Removed | Self::Absent | Self::OtherOwner { .. } => None,
            Self::Unrecognized => Some(format!(
                "接続先のファイル（{path}）の中身を読み取れないため、消さずに残しました。{advice}"
            )),
            Self::ReadFailed(err) => Some(format!(
                "接続先のファイル（{path}）を読めないため、消さずに残しました（{err}）。{advice}"
            )),
            Self::RemoveFailed(err) => Some(format!(
                "接続先のファイル（{path}）を消せませんでした（{err}）。{advice}"
            )),
        }
    }
}

/// 終了時の後始末: ファイルの `pid` が `own_pid` のときだけ消す（[`EndpointClear`]）。
/// 無い・壊れている・読めない・他のプロセスのものなら消さない（所有者を確かめられない
/// ファイルは消さない。壊れたファイルは次に書く側が置き換える）。
pub fn clear_endpoint(path: &Path, own_pid: u32) -> EndpointClear {
    clear_endpoint_with(path, own_pid, |path| std::fs::remove_file(path))
}

/// 削除を差し替えられる版（テストが削除の失敗を注入する）。
fn clear_endpoint_with(
    path: &Path,
    own_pid: u32,
    remove: impl FnOnce(&Path) -> std::io::Result<()>,
) -> EndpointClear {
    match load_endpoint(path) {
        Err(err) => EndpointClear::ReadFailed(err.to_string()),
        Ok(Loaded::Missing) => EndpointClear::Absent,
        Ok(Loaded::Unrecognized) => EndpointClear::Unrecognized,
        Ok(Loaded::Info(info)) if info.pid != own_pid => {
            EndpointClear::OtherOwner { pid: info.pid }
        }
        Ok(Loaded::Info(_)) => match remove(path) {
            Ok(()) => EndpointClear::Removed,
            // 読んだ後に誰かが消した。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => EndpointClear::Absent,
            Err(err) => EndpointClear::RemoveFailed(err.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> EndpointInfo {
        EndpointInfo {
            schema: ENDPOINT_SCHEMA,
            port: 8721,
            pid: std::process::id(),
            scope: "0123456789abcdef".to_string(),
            version: "0.1.0".to_string(),
            host_kind: "service".to_string(),
        }
    }

    fn path_in(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("run").join(ENDPOINT_FILE_NAME)
    }

    #[test]
    fn write_then_read_round_trips_and_creates_the_run_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        assert_eq!(read_endpoint(&path), Some(sample()));
    }

    #[test]
    fn the_file_uses_the_documented_camel_case_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for key in ["schema", "port", "pid", "scope", "version", "hostKind"] {
            assert!(json.get(key).is_some(), "{key} が無い: {json}");
        }
        assert_eq!(json["hostKind"], "service");
        assert_eq!(json["port"], 8721);
    }

    #[test]
    fn writing_again_replaces_the_previous_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        let mut next = sample();
        next.port = 9000;
        write_endpoint(&path, &next).unwrap();
        assert_eq!(read_endpoint(&path).unwrap().port, 9000);
    }

    #[test]
    fn a_failed_write_keeps_the_old_file_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        let mut next = sample();
        next.port = 9000;
        let err = write_endpoint_with(&path, &next, |_| Err(std::io::Error::other("注入した失敗")))
            .unwrap_err();
        assert!(err.to_string().contains("注入"));
        // 読む側は古い完全なファイルを見続ける（書きかけを見ない）。
        assert_eq!(read_endpoint(&path).unwrap().port, 8721);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, vec![ENDPOINT_FILE_NAME.to_string()]);
    }

    #[test]
    fn missing_corrupt_or_implausible_files_read_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        assert_eq!(read_endpoint(&path), None, "無い");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        for (label, body) in [
            ("空", ""),
            ("途中で切れた", r#"{"port":8721,"pid":1,"sc"#),
            ("配列", "[]"),
            (
                "型が違う",
                r#"{"port":"x","pid":1,"scope":"s","version":"v","hostKind":"h"}"#,
            ),
            ("必須が無い", r#"{"port":8721,"pid":1}"#),
            (
                "port 0",
                r#"{"port":0,"pid":1,"scope":"s","version":"v","hostKind":"h"}"#,
            ),
            (
                "pid 0",
                r#"{"port":1,"pid":0,"scope":"s","version":"v","hostKind":"h"}"#,
            ),
            (
                "port 範囲外",
                r#"{"port":70000,"pid":1,"scope":"s","version":"v","hostKind":"h"}"#,
            ),
        ] {
            std::fs::write(&path, body).unwrap();
            assert_eq!(read_endpoint(&path), None, "{label}");
        }
    }

    #[test]
    fn an_oversized_file_is_rejected_without_being_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::from(r#"{"port":1,"pid":1,"scope":""#);
        body.push_str(&"a".repeat(70_000));
        body.push_str(r#"","version":"v","hostKind":"h"}"#);
        std::fs::write(&path, body).unwrap();
        assert_eq!(read_endpoint(&path), None);
    }

    #[test]
    fn unknown_fields_and_a_missing_schema_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"port":8721,"pid":7,"scope":"s","version":"v","hostKind":"server","future":1}"#,
        )
        .unwrap();
        let info = read_endpoint(&path).unwrap();
        assert_eq!(info.schema, ENDPOINT_SCHEMA);
        assert_eq!(info.host_kind, "server");
    }

    #[test]
    fn a_stale_file_whose_process_is_gone_reads_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        assert!(read_live_endpoint_with(&path, |_| true).is_some());
        assert!(read_live_endpoint_with(&path, |_| false).is_none());
        // 実際の生存確認でも、自分の pid のファイルは生きている。
        assert!(read_live_endpoint(&path).is_some());
    }

    #[test]
    fn a_file_left_by_a_finished_process_is_stale_with_the_real_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        let mut command = if cfg!(windows) {
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "exit 0"]);
            c
        } else {
            std::process::Command::new("true")
        };
        let mut child = command.spawn().unwrap();
        let mut info = sample();
        info.pid = child.id();
        child.wait().unwrap();
        write_endpoint(&path, &info).unwrap();
        assert!(read_endpoint(&path).is_some(), "内容自体は正しい");
        assert!(read_live_endpoint(&path).is_none());
    }

    #[test]
    fn clear_removes_only_our_own_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        let mut other = sample();
        other.pid = std::process::id().wrapping_add(1).max(1);
        write_endpoint(&path, &other).unwrap();
        assert_eq!(
            clear_endpoint(&path, std::process::id()),
            EndpointClear::OtherOwner { pid: other.pid }
        );
        assert!(path.exists(), "他のプロセスのファイルは消さない");

        write_endpoint(&path, &sample()).unwrap();
        assert_eq!(
            clear_endpoint(&path, std::process::id()),
            EndpointClear::Removed
        );
        assert!(!path.exists());
        assert_eq!(
            clear_endpoint(&path, std::process::id()),
            EndpointClear::Absent,
            "冪等"
        );
    }

    /// #599: 消せなかったことを「消した・無い」と区別し、知らせる 1 行を作る。
    /// 知らせなくてよい結果は 1 行を作らない。
    #[test]
    fn a_failed_removal_is_reported_and_not_mistaken_for_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        let result = clear_endpoint_with(&path, std::process::id(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
        });
        assert!(
            matches!(result, EndpointClear::RemoveFailed(_)),
            "{result:?}"
        );
        assert!(path.exists());
        let line = result.diagnostic(&path).expect("知らせる");
        assert!(line.contains("消せませんでした"), "{line}");
        assert!(line.contains(&path.display().to_string()), "{line}");

        // 削除の直前に誰かが消した（NotFound）は「無い」。
        let raced = clear_endpoint_with(&path, std::process::id(), |_| {
            Err(std::io::Error::from(std::io::ErrorKind::NotFound))
        });
        assert_eq!(raced, EndpointClear::Absent);

        for quiet in [
            EndpointClear::Removed,
            EndpointClear::Absent,
            EndpointClear::OtherOwner { pid: 1 },
        ] {
            assert_eq!(quiet.diagnostic(&path), None, "{quiet:?}");
        }
    }

    /// PR #649 の Codex P2: 知らせる 1 行は「次の起動で置き換えます」と約束しない。
    /// 置き換えを試みること・できなければ属性と権限を確かめて手で消すこと・OS の
    /// エラー（あれば）を同じ 1 行に入れる（golden）。
    #[test]
    fn diagnostics_do_not_promise_an_automatic_fix() {
        let path = PathBuf::from("root").join("run").join(ENDPOINT_FILE_NAME);
        let shown = path.display().to_string();
        let advice = format!(
            "次の起動で置き換えを試みます。置き換えられないときは、ファイルの属性・権限を確かめて、{shown} を手で消してください"
        );
        assert_eq!(
            EndpointClear::RemoveFailed("アクセスが拒否されました。 (os error 5)".to_string())
                .diagnostic(&path)
                .unwrap(),
            format!(
                "接続先のファイル（{shown}）を消せませんでした（アクセスが拒否されました。 (os error 5)）。{advice}"
            )
        );
        assert_eq!(
            EndpointClear::ReadFailed("os error 5".to_string())
                .diagnostic(&path)
                .unwrap(),
            format!("接続先のファイル（{shown}）を読めないため、消さずに残しました（os error 5）。{advice}")
        );
        assert_eq!(
            EndpointClear::Unrecognized.diagnostic(&path).unwrap(),
            format!(
                "接続先のファイル（{shown}）の中身を読み取れないため、消さずに残しました。{advice}"
            )
        );
        for state in [
            EndpointClear::RemoveFailed("x".to_string()),
            EndpointClear::ReadFailed("x".to_string()),
            EndpointClear::Unrecognized,
        ] {
            let line = state.diagnostic(&path).unwrap();
            assert!(!line.contains("置き換えます"), "{line}");
        }
    }

    /// #599: 壊れた・読めないファイルは所有者を確かめられないので消さず、残したことを
    /// 知らせる。読めないのは「無い」と区別する（同じ名前のフォルダを置いて読めなく
    /// する。Windows は開けない・それ以外は読めない、のどちらも `ReadFailed`）。
    #[test]
    fn unreadable_or_corrupt_files_are_kept_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json").unwrap();
        let corrupt = clear_endpoint(&path, std::process::id());
        assert_eq!(corrupt, EndpointClear::Unrecognized);
        assert!(path.exists(), "所有者を確かめられないファイルは消さない");
        assert!(corrupt.diagnostic(&path).is_some());

        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let unreadable = clear_endpoint(&path, std::process::id());
        assert!(
            matches!(unreadable, EndpointClear::ReadFailed(_)),
            "{unreadable:?}"
        );
        assert!(path.exists());
        assert!(unreadable
            .diagnostic(&path)
            .is_some_and(|line| line.contains("読めない")));
    }

    /// #599: Windows の本物の共有違反（削除の共有を許さずに開いたまま）で消せない。
    #[cfg(windows)]
    #[test]
    fn a_sharing_violation_is_a_failed_removal_on_windows() {
        use std::os::windows::fs::OpenOptionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = path_in(&dir);
        write_endpoint(&path, &sample()).unwrap();
        // 読みの共有だけ許す（FILE_SHARE_READ = 1）。削除は共有違反になる。
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&path)
            .unwrap();
        let result = clear_endpoint(&path, std::process::id());
        drop(held);
        assert!(
            matches!(result, EndpointClear::RemoveFailed(_)),
            "{result:?}"
        );
        assert!(path.exists());
        assert_eq!(
            clear_endpoint(&path, std::process::id()),
            EndpointClear::Removed
        );
    }

    #[test]
    fn for_current_process_fills_pid_and_version() {
        let info = EndpointInfo::for_current_process(8721, "abc", "server");
        assert_eq!(info.pid, std::process::id());
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.schema, ENDPOINT_SCHEMA);
    }
}
