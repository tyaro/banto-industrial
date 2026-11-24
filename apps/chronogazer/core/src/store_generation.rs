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
//!   その DB の id に戻り、同じ保存先をそのまま使う（下の「限界」）。
//! - 保存先のフォルダには、使い始めるときに印のファイル [`MARKER_FILE_NAME`]（中身は世代 id）
//!   を置く。
//! - 起動（`crate::engine` の 6.、書き込みの確かめと収集より前）に [`claim_dir`]
//!   （サービスは握ったフォルダで [`claim_managed_dir`]）で確かめる:
//!   - 無いフォルダ・空のフォルダ: 印を書いて使う;
//!   - 印の id が DB と同じ: 使う;
//!   - 印が無いのに中身がある（v8 より前の版の保存先・関係の無いフォルダ）: [`StoreRefusal::Unmarked`];
//!   - 印の id が DB と違う（作り直す前の DB の保存先・別の DB の保存先）:
//!     [`StoreRefusal::OtherGeneration`];
//!   - 印が空・書きかけ（改行で終わっていない）: [`StoreRefusal::IncompleteMarker`]。印を置く
//!     途中で落ちた残りと見えても**自動では書き直さない**（オーナーのレビュー P2）。別々の DB の
//!     プロセスは profile の排他を共有しないので、2 つが同時に書き直すと両方が同じ保存先で
//!     収集を始め、`grp:<id>`・`tag:<id>` の値が混ざる。ほかに ChronoGazer が使っていないことを
//!     人が確かめてから印のファイルを消すか、空の保存先を選ぶ。
//!
//!   断るときは、そのフォルダに**一切書かず**、印以外の中身は開かずに（一覧だけ）名前と対処を
//!   書いたエラーで止める（`EngineStartError::StoreGeneration`）。旧い履歴は消さずに残る。
//! - 既定の保存先は v8 から `./data/history`（[`crate::settings::default_data_dir`]）。v8 より前の
//!   既定 `./data` の直下の旧い日付ファイルには触れず、既定のままの利用者は新しい場所で
//!   動き始める。
//!
//! # 限界（見分けるのは DB の作り直しだけ）
//!
//! 世代 id は「どの DB が作った保存先か」を見分けるだけで、同じ DB の**いつの時点か**は見分け
//! ない。接続・グループ・タグの id は `INTEGER PRIMARY KEY AUTOINCREMENT`（banto-tags の
//! migration）なので、1 つの DB の中では消した id を使い回さない（`sqlite_sequence` の最大値の
//! 先から振る）。ところが同じ DB の**旧いバックアップを戻す**と `sqlite_sequence` も戻るので、
//! バックアップの後に作って消した（または戻した時点では無い）タグの id が、戻した後に作る
//! 別のタグに再び付きうる。保存先にはその id の鍵（`tag:<id>`）の時系列ファイルが残っている
//! ので、**その期間の旧い値が新しいタグの履歴に混ざりうる**。これは v8 より前からある経路で、
//! 世代の印では防げない（同じ世代）。戻した後に新しいタグを足すなら、保存先を新しいフォルダに
//! 変えるか、戻す前の保存先を退避する。
//!
//! 「中身」に数えないのは、safe-root の印（[`banto_safe_root::mode::MARKER_NAME`]。サービスが
//! フォルダを握るときに置く）と、書き込みの確かめの一時ファイルの残り
//! （`.chronogazer-write-check-*`）、印の書きかけの一時ファイルの残り
//! （`.chronogazer-store-generation.tmp`）だけ。それ以外は何でも中身として断る（関係の無い
//! ファイルのあるフォルダを保存先にしない）。

use std::io::Write;
use std::path::{Path, PathBuf};

use banto_core::BantoError;
use banto_safe_root::root::ManagedDir;

use crate::settings::SettingsService;

/// 設定 DB の世代 id のキー。
pub const GENERATION_KEY: &str = "data.store_generation";

/// 保存先のフォルダに置く印のファイル（中身は世代 id）。
pub const MARKER_FILE_NAME: &str = ".chronogazer-store-generation";

/// 前の版が印を書くときに使った一時ファイル（一時ファイル → 付け替え）。書きかけで落ちた
/// 残りは中身に数えない（数えると、次の起動が「印の無いファイルがある」と止まる）。
const MARKER_TEMP_FILE_NAME: &str = ".chronogazer-store-generation.tmp";

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
    /// 印が空か書きかけ（改行で終わっていない）。印を置く途中で落ちた残りか、別のプロセスが
    /// いま書いている最中。自動では書き直さない（モジュール doc「形」）。
    IncompleteMarker,
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

/// 断ったときの、窓の無いサービス（`chronogazer-server`）向けの対処（独立レビュー P3。
/// デスクトップは起動時の窓で保存先を選び直せるが、サービスには窓が無い）。
const SERVICE_HINT: &str =
    "サービス（chronogazer-server）では、このフォルダを退避するか名前を変えて\
     から起動し直してください（退避した旧い履歴は残ります）";

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
                 保存先を空のフォルダか新しいフォルダに変えてください（旧い履歴はそのまま残せます）。{SERVICE_HINT}",
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
                 その保存先を作った DB に戻してください。{SERVICE_HINT}",
                dir.display()
            ),
            Self::Refused {
                dir,
                refusal: StoreRefusal::IncompleteMarker,
            } => write!(
                f,
                "時系列データの保存先 {} の印のファイル {MARKER_FILE_NAME} が壊れています（空か書きかけ。\
                 印を置く途中で止まった残りか、別の ChronoGazer がいま使い始めているところです）。\
                 2 つの設定 DB が同じ保存先を使って履歴が混ざらないよう、このフォルダには書き込まずに\
                 止めました（中身は変えていません）。ほかに ChronoGazer（デスクトップ・サービス）が\
                 このフォルダを使っていないことを確かめてから印のファイルを消すか、保存先を空の\
                 フォルダか新しいフォルダに変えてください。{SERVICE_HINT}",
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
        || name == MARKER_TEMP_FILE_NAME
        || name == banto_safe_root::mode::MARKER_NAME
        || name.starts_with(WRITE_CHECK_PREFIX)
}

/// 一覧と印の中身から決める（純関数）。`Ok(true)` = 印を書いて使う、`Ok(false)` = そのまま使う。
fn decide(names: &[String], marker: Option<&str>, generation: &str) -> Result<bool, StoreRefusal> {
    let has_content = names.iter().any(|name| !ignored(name));
    match marker {
        Some(raw) => judge_marker(raw, generation).map(|()| false),
        None if has_content => Err(StoreRefusal::Unmarked),
        None => Ok(true),
    }
}

/// 読んだ印の中身を比べる（純関数）。`Ok(())` = この DB の印。空・改行で終わらない印は
/// 書きかけとして [`StoreRefusal::IncompleteMarker`]（自動では書き直さない。モジュール doc「形」）。
fn judge_marker(raw: &str, generation: &str) -> Result<(), StoreRefusal> {
    let found = raw.trim();
    if found == generation {
        Ok(())
    } else if found.is_empty() || !raw.ends_with('\n') {
        Err(StoreRefusal::IncompleteMarker)
    } else {
        Err(StoreRefusal::OtherGeneration {
            found: found.to_string(),
        })
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
    let marker = dir.join(MARKER_FILE_NAME);
    put_marker(
        dir,
        generation,
        &mut || match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(file) => Ok(Some(file)),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => Ok(None),
            Err(err) => Err(err.to_string()),
        },
        &mut || match std::fs::read_to_string(&marker) {
            Ok(contents) => Ok(Some(contents)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err.to_string()),
        },
    )?;
    // 前の版（一時ファイル → 付け替え）の書きかけの残りは、使い始めたら消す（無ければ何もしない）。
    let _ = std::fs::remove_file(dir.join(MARKER_TEMP_FILE_NAME));
    Ok(())
}

/// 印の書きかけを待つ回数と間隔（[`put_marker`]）。`create_new` から中身を書き終えるまでは
/// 一瞬なので、待っても書きかけのままなのは書きかけで落ちた残り（断る）。
const EMPTY_MARKER_RETRIES: u32 = 20;
const EMPTY_MARKER_WAIT: std::time::Duration = std::time::Duration::from_millis(50);

/// 印を「無ければ作る（`create_new`）、あれば読み直して比べる」で置く（独立レビュー P3）。
///
/// [`inspect_dir`] で空と見てから印を置くまでの間に、別の DB（別のプロセス）が同じ空の
/// フォルダを使い始めると、両方が通っていた（どちらも一時ファイル → 付け替えで上書き）。
/// `create_new` は 1 つしか成功しないので、負けた側は勝った側の印を読み、世代が違えば
/// [`StoreRefusal::OtherGeneration`] で断る（何も書かない）。
///
/// 読んだ印が空・書きかけなら、勝った側が書いている最中か、書きかけで落ちた残り。最大 1 秒
/// 待って読み直し（勝った側の世代で正しく断る・通すため）、書きかけのままなら
/// [`StoreRefusal::IncompleteMarker`] で断る。**自動では書き直さない**（オーナーのレビュー P2。
/// 書き直すと、落ちた残りに同時に当たった 2 つの DB が両方通りうる）。
///
/// - `create_new`: 作れたら書き込みで開いたファイル、既にあれば `None`。
/// - `read`: 印の中身（無ければ `None`）。
fn put_marker(
    shown: &Path,
    generation: &str,
    create_new: &mut dyn FnMut() -> Result<Option<std::fs::File>, String>,
    read: &mut dyn FnMut() -> Result<Option<String>, String>,
) -> Result<(), ClaimError> {
    let io = |reason: String| ClaimError::Io {
        dir: shown.to_path_buf(),
        reason,
    };
    if let Some(mut file) = create_new().map_err(|err| io(format!("印を書けません: {err}")))?
    {
        file.write_all(&marker_contents(generation))
            .and_then(|()| file.sync_all())
            .map_err(|err| io(format!("印を書けません: {err}")))?;
        return Ok(());
    }
    let mut attempt = 0;
    loop {
        let found = read()
            .map_err(|err| io(format!("印を読めません: {err}")))?
            .ok_or_else(|| io("印が読む前に消えました".to_string()))?;
        match judge_marker(&found, generation) {
            Ok(()) => return Ok(()),
            Err(StoreRefusal::IncompleteMarker) if attempt < EMPTY_MARKER_RETRIES => {
                attempt += 1;
                std::thread::sleep(EMPTY_MARKER_WAIT);
            }
            Err(refusal) => {
                return Err(ClaimError::Refused {
                    dir: shown.to_path_buf(),
                    refusal,
                })
            }
        }
    }
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
        // パスの版と同じく「無ければ作る、あれば読み直して比べる」（[`put_marker`]）。
        // 作るのも読むのも握ったハンドル基準。
        put_marker(
            path,
            generation,
            &mut || {
                dir.create_new(MARKER_FILE_NAME)
                    .map_err(|err| format!("{err:?}"))
            },
            &mut || {
                let Some(mut file) = dir
                    .open_read(MARKER_FILE_NAME)
                    .map_err(|err| format!("{err:?}"))?
                else {
                    return Ok(None);
                };
                let mut contents = String::new();
                std::io::Read::read_to_string(&mut file, &mut contents)
                    .map_err(|err| err.to_string())?;
                Ok(Some(contents))
            },
        )?;
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
        // 印の一時ファイル（書きかけで落ちた残り）は中身に数えない。
        assert_eq!(
            decide(&names(&[MARKER_TEMP_FILE_NAME]), None, gen),
            Ok(true)
        );
        // 空・書きかけ（改行で終わらない）の印は、ほかに中身が無くても書き直さずに断る
        // （オーナーのレビュー P2）。
        for (list, marker) in [
            (&[MARKER_FILE_NAME, MARKER_TEMP_FILE_NAME][..], ""),
            (&[MARKER_FILE_NAME][..], ""),
            (&[MARKER_FILE_NAME][..], "  \n"),
            (&[MARKER_FILE_NAME][..], "oth"),
            (&[MARKER_FILE_NAME, "x"][..], ""),
        ] {
            assert_eq!(
                decide(&names(list), Some(marker), gen),
                Err(StoreRefusal::IncompleteMarker),
                "{list:?} {marker:?}"
            );
        }
        // 同じ世代なら、改行が欠けていても通す（中身は揃っている）。
        assert_eq!(
            decide(&names(&[MARKER_FILE_NAME]), Some("abc"), gen),
            Ok(false)
        );
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

    /// 独立レビュー P3: 空と見た後に別の DB が先に印を置いていたら（`create_new` に負けたら）、
    /// 読み直した印の世代で断り、何も書かない。
    #[test]
    fn losing_the_marker_race_to_another_generation_refuses() {
        let shown = Path::new("store");
        let err = put_marker(shown, "gen-b", &mut || Ok(None), &mut || {
            Ok(Some("gen-a\n".to_string()))
        })
        .unwrap_err();
        assert!(matches!(
            err,
            ClaimError::Refused {
                refusal: StoreRefusal::OtherGeneration { ref found },
                ..
            } if found == "gen-a"
        ));
        // 同じ世代（同じ DB の 2 回目）なら通る。
        put_marker(shown, "gen-a", &mut || Ok(None), &mut || {
            Ok(Some("gen-a\n".to_string()))
        })
        .unwrap();
    }

    /// 勝った側の書きかけ（空の印）は待って読み直し、書き終えた印の世代で決める。待っても
    /// 書きかけのままなら（落ちた残り）断る。どちらでも書き直さない（オーナーのレビュー P2）。
    #[test]
    fn an_incomplete_marker_is_waited_for_then_refused() {
        let shown = Path::new("store");
        let mut reads = 0;
        let err = put_marker(shown, "gen-b", &mut || Ok(None), &mut || {
            reads += 1;
            Ok(Some(if reads < 3 { "" } else { "gen-a\n" }.to_string()))
        })
        .unwrap_err();
        assert!(matches!(
            err,
            ClaimError::Refused {
                refusal: StoreRefusal::OtherGeneration { .. },
                ..
            }
        ));
        let mut reads = 0;
        put_marker(shown, "gen-b", &mut || Ok(None), &mut || {
            reads += 1;
            Ok(Some(if reads < 3 { "gen-" } else { "gen-b\n" }.to_string()))
        })
        .unwrap();
        let err = put_marker(shown, "gen-b", &mut || Ok(None), &mut || {
            Ok(Some(String::new()))
        })
        .unwrap_err();
        assert!(matches!(
            err,
            ClaimError::Refused {
                refusal: StoreRefusal::IncompleteMarker,
                ..
            }
        ));
        assert!(err.to_string().contains("印のファイル"));
    }

    /// 中断で空の印が残った保存先: 単独でも、別の世代の 2 つが同時に使い始めても、どちらも
    /// 通さず、印も中身も変えない（オーナーのレビュー P2）。
    #[test]
    fn a_leftover_empty_marker_is_never_claimed() {
        let dir = TempDir::new();
        let store = dir.path().join("history");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join(MARKER_FILE_NAME), b"").unwrap();
        std::fs::write(store.join("20260101-001.sqlite3"), b"rows").unwrap();
        let snapshot = || {
            let mut listed: Vec<(String, Vec<u8>)> = std::fs::read_dir(&store)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        std::fs::read(entry.path()).unwrap(),
                    )
                })
                .collect();
            listed.sort();
            listed
        };
        let before = snapshot();
        let refused = |result: Result<(), ClaimError>| {
            matches!(
                result,
                Err(ClaimError::Refused {
                    refusal: StoreRefusal::IncompleteMarker,
                    ..
                })
            )
        };
        assert!(refused(claim_dir(&store, "gen-a")));
        assert!(matches!(
            inspect_dir(&store, "gen-a"),
            Err(ClaimError::Refused {
                refusal: StoreRefusal::IncompleteMarker,
                ..
            })
        ));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["gen-a", "gen-b"]
            .into_iter()
            .map(|generation| {
                let (store, barrier) = (store.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_dir(&store, generation)
                })
            })
            .collect();
        for handle in handles {
            assert!(refused(handle.join().unwrap()));
        }
        assert_eq!(snapshot(), before);
    }

    /// 2 つの DB が同じ空のフォルダを同時に使い始めても、通るのは 1 つだけ。
    #[test]
    fn two_generations_claiming_one_empty_folder_at_once_admit_one() {
        let dir = TempDir::new();
        let store = dir.path().join("history");
        std::fs::create_dir_all(&store).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = ["gen-a", "gen-b"]
            .into_iter()
            .map(|generation| {
                let (store, barrier) = (store.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    claim_dir(&store, generation).is_ok()
                })
            })
            .collect();
        let admitted = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|ok| *ok)
            .count();
        assert_eq!(admitted, 1);
    }

    /// 印の書きかけの一時ファイルが残っていても（書きかけで落ちた後）、起動は止まらず、次の
    /// `claim_dir` が印を書き直して一時ファイルも残さない。
    #[test]
    fn a_leftover_marker_temp_file_does_not_stop_the_start() {
        let dir = TempDir::new();
        let store = dir.path().join("history");
        std::fs::create_dir_all(&store).unwrap();
        std::fs::write(store.join(MARKER_TEMP_FILE_NAME), b"gen-").unwrap();
        assert!(inspect_dir(&store, "gen-a").unwrap());
        claim_dir(&store, "gen-a").unwrap();
        assert_eq!(
            std::fs::read_to_string(store.join(MARKER_FILE_NAME)).unwrap(),
            "gen-a\n"
        );
        assert!(!store.join(MARKER_TEMP_FILE_NAME).exists());
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
