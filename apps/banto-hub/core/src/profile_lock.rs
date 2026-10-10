//! T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P2、
//! docs/banto-hub-desktop-plan.md §16.2「mutex 命名決定」）:
//! `HubRuntime::start`冒頭で取る profile 単位の排他。3層のうち (b)/(c) を
//! ここに実装する（(a) SCM `query_status`は`crate::service_manager`側）。
//!
//! #392 A1（2026-10-10）: ロックの**仕組み**（Windows named mutex・
//! `profile.lock` 診断ファイル・非 Windows の `flock`）は共有 crate
//! [`banto_instance_lock`] へ移した（ChronoGazer も同じ保護を使うため、
//! 複製せず寄せた）。このモジュールは banto-hub 固有の部分だけを持つ薄い
//! ラッパー:
//!
//! - 排他の単位は **profile-id**（mutex 名 `Global\BantoHub.<profile-id>`、
//!   [`crate::profile_paths::mutex_name`]）。命名・診断ファイル名
//!   （`{profile_dir}/profile.lock`）・JSON の形は移行前と同一で、既存の
//!   インストール（サービス版が持つ mutex、残っている `profile.lock`）と
//!   そのまま噛み合う。
//! - profile ディレクトリ一式（`config`/`data`/`logs`）の作成。
//! - 失敗に profile-id を載せる（[`ProfileLockError::AlreadyHeld`]）。
//!
//! 仕組みの詳細（3 層の役割、LocalSystem 保持時の `ERROR_ACCESS_DENIED`
//! 対策など）は `banto_instance_lock` の crate doc を参照。

use std::path::{Path, PathBuf};

use banto_instance_lock::{InstanceLockError, InstanceLockGuard};
use thiserror::Error;

use crate::profile_paths::ProfilePaths;

pub(crate) use banto_instance_lock::read_owner_info;

/// `profile.lock`の中身（診断用 JSON）。共有 crate の [`banto_instance_lock::OwnerInfo`]
/// と同一の型（フィールド名・JSON の形は移行前のまま）。
pub type ProfileOwnerInfo = banto_instance_lock::OwnerInfo;

/// profile lock の診断ファイル名（`{profile_dir}/profile.lock`）。
pub const LOCK_FILE_NAME: &str = "profile.lock";

/// profile を運転しようとしているホストの種別（[`ProfileOwnerInfo::host_kind`]
/// に文字列として書き込む診断用の値 - `crate::service_manager::ServiceManager`
/// が扱う SCM 状態とは無関係）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HubHostKind {
    /// `bin/banto-hub.rs`（引数なし、Ctrl-C で停止）。
    Console,
    /// `bin/banto_hub/win_service.rs`（Windows サービス）。
    Service,
    /// `apps/banto-hub/src-tauri`（デスクトップシェル）。
    Shell,
}

impl std::fmt::Display for HubHostKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HubHostKind::Console => write!(f, "console"),
            HubHostKind::Service => write!(f, "service"),
            HubHostKind::Shell => write!(f, "shell"),
        }
    }
}

/// [`try_acquire_profile_lock`]の失敗モード。
#[derive(Debug, Error)]
pub enum ProfileLockError {
    /// 既に別プロセスがこの profile を保持している（安全側で起動拒否）。
    /// `owner`は`profile.lock`から読めた場合のみ`Some`（診断用）。
    #[error("banto-hub: profile '{profile_id}' は既に別プロセスが使用中です（owner: {owner:?}）")]
    AlreadyHeld {
        profile_id: String,
        owner: Option<ProfileOwnerInfo>,
    },
    /// profile ディレクトリ作成・lock ファイル open/書き込みの I/O エラー。
    #[error("banto-hub: profile ロックの I/O に失敗しました: {0}")]
    Io(#[from] std::io::Error),
    /// `profile_id`自体が不正（`crate::profile_paths::validate_profile_id`
    /// が拒否する文字列）。`crate::runtime::HubRuntime::start`が
    /// `crate::profile_paths::resolve_profile_paths`の失敗をここへ変換して
    /// 使う（[`crate::runtime::HubStartError::ProfileLock`]が単一の変種で
    /// profile 関連の失敗を統一的に扱えるようにするため）。
    #[error("banto-hub: profile id が不正です: {0}")]
    InvalidProfile(#[from] crate::profile_paths::ProfileIdError),
}

/// `try_acquire_profile_lock`が成功した間だけ生存するガード。
/// `HubRuntime::start`が構築する`RunningHub`がこれを保持し、`Drop`で
/// 共有 crate のガードが OS レベルの排他を返す。
pub struct ProfileLockGuard {
    guard: InstanceLockGuard,
}

impl ProfileLockGuard {
    /// 診断ファイル（`profile.lock`）の絶対パス - fallback UI（T16-2）が
    /// 「mutex: 所有者不明」等の表示に使う所有者情報の在り処として参照する想定。
    pub fn lock_file_path(&self) -> &Path {
        self.guard.lock_file_path()
    }
}

/// `paths`が指す profile の排他を取得する。`HubRuntime::start`が DB 初期化
/// より前に呼ぶ。
///
/// - `profile_dir`/`config`/`data`/`logs`を`create_dir_all`する（初回起動時
///   はまだ存在しないため）。
/// - 排他の取得は [`banto_instance_lock::try_acquire`]（mutex 名は
///   `Global\BantoHub.<profile-id>`、診断ファイルは`{profile_dir}/profile.lock`）。
///   既に保持されていれば[`ProfileLockError::AlreadyHeld`]。
pub fn try_acquire_profile_lock(
    paths: &ProfilePaths,
    host_kind: HubHostKind,
) -> Result<ProfileLockGuard, ProfileLockError> {
    std::fs::create_dir_all(&paths.profile_dir)?;
    if let Some(config_dir) = paths.db_path.parent() {
        std::fs::create_dir_all(config_dir)?;
    }
    std::fs::create_dir_all(&paths.data_dir)?;
    std::fs::create_dir_all(&paths.logs_dir)?;

    let lock_path: PathBuf = paths.profile_dir.join(LOCK_FILE_NAME);
    let mutex_name = crate::profile_paths::mutex_name(&paths.profile_id);

    match banto_instance_lock::try_acquire(&mutex_name, &lock_path, &host_kind.to_string()) {
        Ok(guard) => Ok(ProfileLockGuard { guard }),
        Err(InstanceLockError::AlreadyHeld { owner }) => Err(ProfileLockError::AlreadyHeld {
            profile_id: paths.profile_id.clone(),
            owner,
        }),
        Err(InstanceLockError::Io(err)) => Err(ProfileLockError::Io(err)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile_paths::resolve_profile_paths;
    use crate::test_support::TempDir;

    #[test]
    fn try_acquire_profile_lock_creates_profile_directories() {
        let root = TempDir::new("profile-lock-layout");
        let paths = resolve_profile_paths(root.path(), "creates-dirs").expect("valid profile id");

        let _guard =
            try_acquire_profile_lock(&paths, HubHostKind::Console).expect("first acquire ok");

        assert!(paths.profile_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.logs_dir.is_dir());
        assert!(paths.db_path.parent().unwrap().is_dir());
        assert!(paths.profile_dir.join(LOCK_FILE_NAME).is_file());
    }

    #[test]
    fn try_acquire_profile_lock_writes_owner_diagnostics() {
        let root = TempDir::new("profile-lock-owner-info");
        let paths =
            resolve_profile_paths(root.path(), "owner-diagnostics").expect("valid profile id");

        let _guard =
            try_acquire_profile_lock(&paths, HubHostKind::Service).expect("first acquire ok");

        let owner =
            read_owner_info(&paths.profile_dir.join(LOCK_FILE_NAME)).expect("owner info written");
        assert_eq!(owner.pid, std::process::id());
        assert_eq!(owner.host_kind, "service");
    }

    /// このモジュール doc「非 Windows」節の受入条件そのもの: 同一プロセス内
    /// (Linux CI でも実行できる)で同じ profile を2重に`try_acquire`すると、
    /// 2回目は`AlreadyHeld`になる。
    #[test]
    fn second_acquire_on_the_same_profile_fails() {
        let root = TempDir::new("profile-lock-double-acquire");
        let paths = resolve_profile_paths(root.path(), "double-acquire").expect("valid profile id");

        let _first =
            try_acquire_profile_lock(&paths, HubHostKind::Console).expect("first acquire ok");

        match try_acquire_profile_lock(&paths, HubHostKind::Shell) {
            Err(ProfileLockError::AlreadyHeld { profile_id, owner }) => {
                assert_eq!(profile_id, "double-acquire");
                let owner = owner.expect("first owner diagnostics should be readable");
                assert_eq!(owner.host_kind, "console");
            }
            Err(other) => panic!("expected AlreadyHeld, got {other}"),
            Ok(_) => panic!("second acquire should fail while the first guard is still held"),
        }
    }

    /// 排他の単位は profile-id（desktop-plan §16.2 の
    /// `Global\BantoHub.<profile-id>`）であり、ディレクトリパスではない。
    /// 別 profile-id なら同じ root 配下でも同時取得できることを確認する
    /// （旧テストは「同 id・別 root」だったが、Windows では named mutex が
    /// profile-id だけを見るため `AlreadyHeld` になり CI windows-latest で
    /// 落ちる - 製品意味論とも食い違う）。
    #[test]
    fn different_profile_ids_can_both_acquire() {
        let root = TempDir::new("profile-lock-independent-ids");
        let paths_a = resolve_profile_paths(root.path(), "line-a").expect("valid profile id");
        let paths_b = resolve_profile_paths(root.path(), "line-b").expect("valid profile id");

        let _guard_a =
            try_acquire_profile_lock(&paths_a, HubHostKind::Console).expect("acquire a ok");
        let _guard_b =
            try_acquire_profile_lock(&paths_b, HubHostKind::Console).expect("acquire b ok");
    }

    #[test]
    fn lock_is_released_after_guard_drop_allowing_reacquire() {
        let root = TempDir::new("profile-lock-release-on-drop");
        let paths =
            resolve_profile_paths(root.path(), "release-on-drop").expect("valid profile id");

        {
            let _guard =
                try_acquire_profile_lock(&paths, HubHostKind::Console).expect("first acquire ok");
        }

        let _second =
            try_acquire_profile_lock(&paths, HubHostKind::Console).expect("reacquire after drop");
    }
}
