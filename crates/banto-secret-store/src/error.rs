//! 失敗の分類。**どの variant も秘密（平文・blob）を持たない**。
//!
//! 持つのは操作の名前・`io::ErrorKind`・OS のエラー番号・SID（秘密ではない）
//! だけで、`Display` と `Debug`（derive）にもそれ以上は出ない。アカウント
//! 文字列も持たない（秘密ではないが、エラーを画面やログへそのまま流す呼び出し
//! 側で、接続先の一覧を不用意に並べないため）。

use std::fmt;
use std::io;

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
    /// ファイルを辿らずに開いて属性・記述子を読む（[`crate::acl::inspect_file`]・
    /// `PinnedDir::open_child_file`、#630）。
    OpenFile,
    /// 確かめ済みのフォルダの中にファイルを作る（`PinnedDir::create_child_file`、#630）。
    CreateFile,
    /// 作ったファイルへの書き込みとディスクへの書き出し（同上）。
    WriteFile,
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
            Self::OpenFile => "ファイルの確認",
            Self::CreateFile => "ファイルの作成",
            Self::WriteFile => "ファイルへの書き込み",
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

/// 保存先ディレクトリの ACL が期待と違う理由（[`crate::acl`]）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclMismatch {
    /// パスがディレクトリではない。
    NotADirectory,
    /// パスが reparse point（ジャンクション・シンボリックリンク）。
    ReparsePoint,
    /// DACL が無い（null DACL = 全員にフルアクセス）。
    NoDacl,
    /// DACL が保護されていない（親から継承する）。
    NotProtected,
    /// 継承された ACE がある。
    InheritedAce { sid: String },
    /// 期待に無い ACE がある（拒否の ACE、別の SID、別のアクセス権・フラグを含む）。
    UnexpectedAce {
        ace_type: u8,
        flags: u8,
        mask: u32,
        sid: String,
    },
    /// 期待した ACE が無い。
    MissingAce { sid: String, mask: u32 },
    /// 所有者が SYSTEM・Administrators・指定したインストール利用者のどれでもない。
    DisallowedOwner { sid: String },
}

impl fmt::Display for AclMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotADirectory => f.write_str("フォルダではありません"),
            Self::ReparsePoint => {
                f.write_str("ジャンクションまたはシンボリックリンクです")
            }
            Self::NoDacl => f.write_str("アクセス制御リストがありません"),
            Self::NotProtected => f.write_str("親フォルダから権限を継承しています"),
            Self::InheritedAce { sid } => write!(f, "継承された権限があります（{sid}）"),
            Self::UnexpectedAce {
                ace_type,
                flags,
                mask,
                sid,
            } => write!(
                f,
                "想定外の権限があります（{sid}、種類 {ace_type}、フラグ {flags:#x}、アクセス {mask:#x}）"
            ),
            Self::MissingAce { sid, mask } => {
                write!(f, "必要な権限がありません（{sid}、アクセス {mask:#x}）")
            }
            Self::DisallowedOwner { sid } => write!(f, "所有者が想定外です（{sid}）"),
        }
    }
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
    #[error("インストール利用者の SID が使えません: {0}")]
    InvalidUserSid(&'static str),
    #[error("秘密の保存先のフォルダの権限が想定と違います: {0}")]
    AclMismatch(AclMismatch),
    #[error("秘密の保存先のフォルダの権限の操作に失敗しました（{call}、os error {os_error}）")]
    AclApi { call: &'static str, os_error: u32 },
    #[error("この OS ではフォルダの権限を設定できないため、秘密の保存先を使えません")]
    UnsupportedPlatform,
    /// `.`・`..`（点だけの名前）を含むパス。作るときに OS が正規化して、確かめた
    /// 場所と別の場所を作りうるため断る（#392 B2-4a、PR #604 のレビュー）。
    #[error("フォルダのパスに「.」や「..」が含まれているため使えません")]
    DotSegmentInPath,
    /// 作ったファイルを読み戻したら保護されていなかった（`PinnedDir::create_child_file`、
    /// #630）。作ったファイルは消してある。中身は [`crate::acl::exposure`] の理由。
    #[error("作ったファイルが保護されていません（{0:?}）")]
    Exposed(Vec<crate::acl::Exposure>),
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
