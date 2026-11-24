//! 時系列データの保存先の世代（banto v8 への追従、docs/banto-v8-upgrade-plan.md §5 の 1）。
//!
//! # なぜ要るか
//!
//! 収集は設定 DB の連番 id から系列の鍵（`grp:<id>`・`tag:<id>`、`banto_collect::config`）を
//! 作り、履歴の読み手（`banto_tsquery`）は保存先の日付ファイルをその鍵で照合するだけ。
//! DB を作り直すと同じ id が別のタグに付くので、前の DB の時系列ファイルが残った保存先を
//! そのまま使うと、**旧い値が新しいタグの履歴・日報・CSV に混ざる**。v8 への追従は DB を
//! 作り直す（オーナー決定）ので、ここで保存先と DB の組を確かめる。
//!
//! # 形
//!
//! - 設定 DB は、最初の起動で乱数の「保存先の世代 id」を設定 [`GENERATION_KEY`] に書く
//!   （[`db_generation`]）。DB の作り直しでは新しい id になる。DB のバックアップから戻すと
//!   その DB の id に戻る（同じ DB の履歴なので、同じ保存先を使える）。
//! - 保存先のフォルダには、使い始めるときに印のファイル [`MARKER_FILE_NAME`]（中身は世代 id）
//!   を置く。
//! - 起動（`crate::engine` の 6.、書き込みの確かめと収集より前）に [`claim_dir`]
//!   （サービスは握ったフォルダで [`claim_managed_dir`]）で確かめる:
//!   - 無いフォルダ・空のフォルダ: 印を書いて使う;
//!   - 印の id が DB と同じ: 使う;
//!   - 印が無いのに中身がある（v8 より前の版の保存先・関係の無いフォルダ）: [`StoreRefusal::Unmarked`];
//!   - 印の id が DB と違う（作り直す前の DB の保存先・別の DB の保存先）:
//!     [`StoreRefusal::OtherGeneration`]。
//!
//!   断るときは、そのフォルダに**一切書かず**、印以外の中身は開かずに（一覧だけ）名前と対処を
//!   書いたエラーで止める（`EngineStartError::StoreGeneration`）。旧い履歴は消さずに残る。
//! - 既定の保存先は v8 から `./data/history`（[`crate::settings::default_data_dir`]）。v8 より前の
//!   既定 `./data` の直下の旧い日付ファイルには触れず、既定のままの利用者は新しい場所で
//!   動き始める。
//!
//! 「中身」に数えないのは、safe-root の印（[`banto_safe_root::mode::MARKER_NAME`]。サービスが
//! フォルダを握るときに置く）と、書き込みの確かめの一時ファイルの残り
//! （`.chronogazer-write-check-*`）だけ。それ以外は何でも中身として断る（関係の無いファイルの
//! あるフォルダを保存先にしない）。

use std::io::Write;
use std::path::{Path, PathBuf};

use banto_core::BantoError;
use banto_safe_root::root::ManagedDir;

use crate::settings::SettingsService;

/// 設定 DB の世代 id のキー。
pub const GENERATION_KEY: &str = "data.store_generation";

/// 保存先のフォルダに置く印のファイル（中身は世代 id）。
pub const MARKER_FILE_NAME: &str = ".chronogazer-store-generation";

/// 書き込みの確かめの一時ファイルの接頭辞（`startup_recovery::check_writable_dir` と
/// `service_data_dir::check_service_writable_dir`）。落ちたときの残りを中身に数えない。
const WRITE_CHECK_PREFIX: &str = ".chronogazer-write-check-";

/// 設定 DB の世代 id を読む。無ければ乱数で作って書く（新しい DB の最初の起動）。
pub async fn db_generation(settings: &SettingsService) -> Result<String, BantoError> {
    if let Some(existing) = settings.get(GENERATION_KEY).await? {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|err| BantoError::Other(format!("保存先の世代 id を作れません: {err}")))?;
    let generation: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    settings.set(GENERATION_KEY, &generation).await?;
    Ok(generation)
}

/// 保存先を使わない理由（[`claim_dir`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreRefusal {
    /// 印が無いのに中身がある（v8 より前の版の保存先か、関係の無いフォルダ）。
    Unmarked,
    /// 印の id がこの DB と違う（作り直す前の DB・別の DB の保存先）。
    OtherGeneration { found: String },
}

/// [`claim_dir`] の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// この DB の保存先として使えない（中身は変えていない）。
    Refused { dir: PathBuf, refusal: StoreRefusal },
    /// 確かめられなかった（一覧・印の読み書き・フォルダの作成の失敗）。
    Io { dir: PathBuf, reason: String },
}

impl ClaimError {
    /// 対象のフォルダ。
    pub fn dir(&self) -> &Path {
        match self {
            Self::Refused { dir, .. } | Self::Io { dir, .. } => dir,
        }
    }
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused {
                dir,
                refusal: StoreRefusal::Unmarked,
            } => write!(
                f,
                "時系列データの保存先 {} には、この版の印の無いファイルがあります（v8 より前の版の \
                 ChronoGazer の履歴か、関係の無いファイル）。旧い履歴が新しいタグに混ざらないよう、\
                 このフォルダには書き込まずに止めました（中身は変えていません）。\
                 保存先を空のフォルダか新しいフォルダに変えてください（旧い履歴はそのまま残せます）",
                dir.display()
            ),
            Self::Refused {
                dir,
                refusal: StoreRefusal::OtherGeneration { .. },
            } => write!(
                f,
                "時系列データの保存先 {} は、別の設定 DB（作り直す前の DB など）の保存先です。\
                 旧い履歴が新しいタグに混ざらないよう、このフォルダには書き込まずに止めました\
                 （中身は変えていません）。保存先を空のフォルダか新しいフォルダに変えるか、\
                 その保存先を作った DB に戻してください",
                dir.display()
            ),
            Self::Io { dir, reason } => write!(
                f,
                "時系列データの保存先 {} を確かめられませんでした: {reason}",
                dir.display()
            ),
        }
    }
}

impl std::error::Error for ClaimError {}

/// 中身に数えない名前か（モジュール doc の最後）。
fn ignored(name: &str) -> bool {
    name == MARKER_FILE_NAME
        || name == banto_safe_root::mode::MARKER_NAME
        || name.starts_with(WRITE_CHECK_PREFIX)
}

/// 一覧と印の中身から決める（純関数）。`Ok(true)` = 印を書いて使う、`Ok(false)` = そのまま使う。
fn decide(names: &[String], marker: Option<&str>, generation: &str) -> Result<bool, StoreRefusal> {
    let has_content = names.iter().any(|name| !ignored(name));
    match marker.map(str::trim) {
        Some(found) if found == generation => Ok(false),
        // 書きかけで落ちた空の印は、ほかに中身が無ければ書き直す。
        Some("") if !has_content => Ok(true),
        Some(found) => Err(StoreRefusal::OtherGeneration {
            found: found.to_string(),
        }),
        None if has_content => Err(StoreRefusal::Unmarked),
        None => Ok(true),
    }
}

fn marker_contents(generation: &str) -> Vec<u8> {
    format!("{generation}\n").into_bytes()
}

/// パスで扱う保存先（デスクトップ・`banto-serve`・コンソール）を、この DB の世代で使ってよいか
/// 確かめ、使い始めなら印を書く（モジュール doc「形」）。無いフォルダは作る（収集の開始も
/// 作るので同じ扱い）。断るときはフォルダに何も書かない。
pub fn claim_dir(dir: &Path, generation: &str) -> Result<(), ClaimError> {
    let io = |reason: String| ClaimError::Io {
        dir: dir.to_path_buf(),
        reason,
    };
    std::fs::create_dir_all(dir).map_err(|err| io(format!("フォルダを作れません: {err}")))?;
    if !inspect_dir(dir, generation)? {
        return Ok(());
    }
    write_marker_path(dir, generation).map_err(|err| io(format!("印を書けません: {err}")))
}

/// [`claim_dir`] の、書かない版。`Ok(true)` = 使える（印はまだ無い・無いフォルダ）、
/// `Ok(false)` = 使える（この DB の印がある）。起動時の対処の窓で保存先を選び直すときに、
/// 書き込みの確かめの前に使う（`startup_recovery::apply_store_dir`）。
pub fn inspect_dir(dir: &Path, generation: &str) -> Result<bool, ClaimError> {
    let io = |reason: String| ClaimError::Io {
        dir: dir.to_path_buf(),
        reason,
    };
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(io(format!("一覧を読めません: {err}"))),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|err| io(format!("一覧を読めません: {err}")))?;
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let marker = if names.iter().any(|name| name == MARKER_FILE_NAME) {
        Some(
            std::fs::read_to_string(dir.join(MARKER_FILE_NAME))
                .map_err(|err| io(format!("印を読めません: {err}")))?,
        )
    } else {
        None
    };
    decide(&names, marker.as_deref(), generation).map_err(|refusal| ClaimError::Refused {
        dir: dir.to_path_buf(),
        refusal,
    })
}

/// 印を一時ファイルに書いてから付け替える（書きかけの印を残さない）。
fn write_marker_path(dir: &Path, generation: &str) -> std::io::Result<()> {
    let temp = dir.join(format!("{MARKER_FILE_NAME}.tmp"));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&marker_contents(generation))?;
        file.sync_all()?;
    }
    let renamed = std::fs::rename(&temp, dir.join(MARKER_FILE_NAME));
    if renamed.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    renamed
}

/// [`claim_dir`] の、サービスが握ったフォルダ（`service_data_dir`）で扱う版。一覧・読み書きは
/// 握ったハンドル基準（辿らない）。`path` は表示用。
pub fn claim_managed_dir(
    dir: &ManagedDir,
    path: &Path,
    generation: &str,
) -> Result<(), ClaimError> {
    let io = |reason: String| ClaimError::Io {
        dir: path.to_path_buf(),
        reason,
    };
    let names: Vec<String> = dir
        .list()
        .map_err(|err| io(format!("一覧を読めません: {err:?}")))?
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    let marker = if names.iter().any(|name| name == MARKER_FILE_NAME) {
        let mut file = dir
            .open_read(MARKER_FILE_NAME)
            .map_err(|err| io(format!("印を読めません: {err:?}")))?
            .ok_or_else(|| io("印が読む前に消えました".to_string()))?;
        let mut contents = String::new();
        std::io::Read::read_to_string(&mut file, &mut contents)
            .map_err(|err| io(format!("印を読めません: {err}")))?;
        Some(contents)
    } else {
        None
    };
    let write =
        decide(&names, marker.as_deref(), generation).map_err(|refusal| ClaimError::Refused {
            dir: path.to_path_buf(),
            refusal,
        })?;
    if write {
        dir.write_atomic(MARKER_FILE_NAME, &marker_contents(generation))
            .map_err(|err| io(format!("印を書けません: {err:?}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn the_decision_follows_the_marker_and_the_content() {
        let gen = "abc";
        assert_eq!(decide(&[], None, gen), Ok(true));
        assert_eq!(
            decide(
                &names(&[".banto-safe-root", ".chronogazer-write-check-1.tmp"]),
                None,
                gen
            ),
            Ok(true)
        );
        assert_eq!(
            decide(&names(&["20260101-001.sqlite3"]), None, gen),
            Err(StoreRefusal::Unmarked)
        );
        assert_eq!(
            decide(&names(&[MARKER_FILE_NAME, "x"]), Some("abc\n"), gen),
            Ok(false)
        );
        assert_eq!(
            decide(&names(&[MARKER_FILE_NAME]), Some("other\n"), gen),
            Err(StoreRefusal::OtherGeneration {
                found: "other".to_string()
            })
        );
        assert_eq!(decide(&names(&[MARKER_FILE_NAME]), Some(""), gen), Ok(true));
        assert!(decide(&names(&[MARKER_FILE_NAME, "x"]), Some(""), gen).is_err());
    }

    /// 無いフォルダは作って印を書き、2 回目は同じ世代で通る。別の世代は断られ、印は変わらない。
    #[test]
    fn a_new_folder_is_claimed_and_another_generation_is_refused() {
        let dir = TempDir::new();
        let store = dir.path().join("data").join("history");
        claim_dir(&store, "gen-a").unwrap();
        assert_eq!(
            std::fs::read_to_string(store.join(MARKER_FILE_NAME)).unwrap(),
            "gen-a\n"
        );
        claim_dir(&store, "gen-a").unwrap();
        std::fs::write(store.join("20260101-001.sqlite3"), b"rows").unwrap();
        let err = claim_dir(&store, "gen-b").unwrap_err();
        assert!(matches!(
            err,
            ClaimError::Refused {
                refusal: StoreRefusal::OtherGeneration { .. },
                ..
            }
        ));
        assert!(err.to_string().contains(&store.display().to_string()));
        assert_eq!(
            std::fs::read_to_string(store.join(MARKER_FILE_NAME)).unwrap(),
            "gen-a\n"
        );
    }

    /// 印の無い中身のあるフォルダ（v8 より前の保存先）は断られ、何も足されない。
    #[test]
    fn an_unmarked_folder_with_content_is_refused_untouched() {
        let dir = TempDir::new();
        let store = dir.path().join("data");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join("20260101-001.sqlite3"), b"old").unwrap();
        let err = claim_dir(&store, "gen").unwrap_err();
        assert!(matches!(
            err,
            ClaimError::Refused {
                refusal: StoreRefusal::Unmarked,
                ..
            }
        ));
        let mut listed: Vec<String> = std::fs::read_dir(&store)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        listed.sort();
        assert_eq!(listed, vec!["20260101-001.sqlite3".to_string()]);
    }
}
