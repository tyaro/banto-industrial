//! 失敗の分類。**どの variant も秘密（平文・blob）を持たない**。
//!
//! 持つのは操作の名前・`io::ErrorKind`・OS のエラー番号・SID（秘密ではない）
//! だけで、`Display` と `Debug`（derive）にもそれ以上は出ない。アカウント
//! 文字列も持たない（秘密ではないが、エラーを画面やログへそのまま流す呼び出し
//! 側で、接続先の一覧を不用意に並べないため）。

use std::fmt;
use std::io;

use banto_safe_root::SafeRootError;

/// どの I/O 操作で失敗したか。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoOp {
    /// 親ディレクトリ・保存先ディレクトリの作成。
    CreateDir,
    /// 保存先ディレクトリの属性（種類・reparse point）の確認。
    InspectDir,
    /// 秘密のファイルの読み込み。
    ReadFile,
    /// 一時ファイルの作成。
    CreateTemp,
    /// 一時ファイルへの書き込み。
    WriteTemp,
    /// 一時ファイルのディスクへの書き出し（`sync_all`）。
    SyncTemp,
    /// 一時ファイルで本体を置き換える（`MoveFileExW` / `rename`）。
    Replace,
    /// 秘密のファイルの削除。
    RemoveFile,
}

impl fmt::Display for IoOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CreateDir => "保存先のフォルダの作成",
            Self::InspectDir => "保存先のフォルダの確認",
            Self::ReadFile => "読み込み",
            Self::CreateTemp => "一時ファイルの作成",
            Self::WriteTemp => "一時ファイルへの書き込み",
            Self::SyncTemp => "一時ファイルの書き出し",
            Self::Replace => "ファイルの置き換え",
            Self::RemoveFile => "削除",
        })
    }
}

/// ファイルの中身が形式に合わない理由（[`crate::format`]）。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorruptKind {
    /// 先頭が `BSS1` ではない。
    BadMagic,
    /// この版が知らない版数。
    UnsupportedVersion(u16),
    /// 長さの欄が指す分のバイトが無い。
    Truncated,
    /// 末尾に余分なバイトがある。
    TrailingBytes,
    /// アカウントの長さが上限を超える。
    AccountTooLong,
    /// アカウントが UTF-8 ではない。
    AccountNotUtf8,
    /// blob の長さが上限を超える。
    BlobTooLarge,
    /// ファイル全体が上限を超える。
    FileTooLarge,
    /// 復号した平文が UTF-8 ではない（[`crate::FileSecretStore::load`] は文字列を返す）。
    SecretNotUtf8,
}

impl fmt::Display for CorruptKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => f.write_str("先頭の識別子が違います"),
            Self::UnsupportedVersion(v) => write!(f, "知らない版数です（{v}）"),
            Self::Truncated => f.write_str("途中で切れています"),
            Self::TrailingBytes => f.write_str("末尾に余分なデータがあります"),
            Self::AccountTooLong => f.write_str("アカウントの長さが上限を超えます"),
            Self::AccountNotUtf8 => f.write_str("アカウントが UTF-8 ではありません"),
            Self::BlobTooLarge => f.write_str("暗号文の長さが上限を超えます"),
            Self::FileTooLarge => f.write_str("ファイルが大きすぎます"),
            Self::SecretNotUtf8 => f.write_str("復号した値が UTF-8 ではありません"),
        }
    }
}

/// アカウント文字列が使えない理由。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InvalidAccount {
    Empty,
    TooLong,
}

/// この crate のすべての失敗。秘密は含まない（モジュール doc）。
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SecretStoreError {
    #[error("秘密の保存先の{op}に失敗しました（{kind}{}）", os_suffix(*.os_error))]
    Io {
        op: IoOp,
        kind: io::ErrorKind,
        os_error: Option<i32>,
    },
    #[error("秘密の暗号化に失敗しました{}", os_suffix_u32(*.os_error))]
    Protect { os_error: Option<u32> },
    #[error(
        "秘密の復号に失敗しました（別の PC からのコピー・別のアカウントのファイル・改ざんなど）{}",
        os_suffix_u32(*.os_error)
    )]
    Unprotect { os_error: Option<u32> },
    #[error("秘密のファイルが壊れています: {0}")]
    Corrupt(CorruptKind),
    #[error("秘密のファイルが別のアカウントのものです")]
    AccountMismatch,
    #[error("アカウントの名前が使えません（{0:?}）")]
    InvalidAccount(InvalidAccount),
    #[error("保存する値が大きすぎます")]
    SecretTooLarge,
    /// 保存先のフォルダの ACL・SID・パスの確かめの失敗（`banto-safe-root`）。
    /// 中身（`AclMismatch`・`InvalidUserSid`・`UnsupportedPlatform` など）は
    /// [`SafeRootError`] の variant。
    #[error("秘密の保存先: {0}")]
    Root(#[from] SafeRootError),
}

impl SecretStoreError {
    pub(crate) fn io(op: IoOp, error: &io::Error) -> Self {
        Self::Io {
            op,
            kind: error.kind(),
            os_error: error.raw_os_error(),
        }
    }
}

fn os_suffix(os_error: Option<i32>) -> String {
    os_error
        .map(|e| format!("、os error {e}"))
        .unwrap_or_default()
}

fn os_suffix_u32(os_error: Option<u32>) -> String {
    os_error
        .map(|e| format!("（os error {e}）"))
        .unwrap_or_default()
}

pub type Result<T> = std::result::Result<T, SecretStoreError>;
