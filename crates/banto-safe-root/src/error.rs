//! 失敗の分類。**製品に依らない**（どの製品のどの置き場かは文言に出さない）。
//!
//! 持つのは操作の名前・`io::ErrorKind`・OS のエラー番号・SID（秘密ではない）だけ。
//! `banto-secret-store` の `SecretStoreError::Root` が包んで返す。

use std::fmt;
use std::io;

/// どの I/O 操作で失敗したか。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IoOp {
    /// 親フォルダ・対象のフォルダの作成。
    CreateDir,
    /// 対象のフォルダの属性（種類・reparse point）の確認。
    InspectDir,
    /// ファイルの読み込み。
    ReadFile,
    /// ファイルの削除。
    RemoveFile,
    /// ファイルを辿らずに開いて属性・記述子を読む（[`crate::acl::inspect_file`]・
    /// `PinnedDir::open_child_file`、#630）。
    OpenFile,
    /// 確かめ済みのフォルダの中にファイルを作る（`PinnedDir::create_child_file`、#630）。
    CreateFile,
    /// 作ったファイルへの書き込みとディスクへの書き出し（同上）。
    WriteFile,
    /// 書いた一時ファイルの名前を本来の名前へ付け替える（`ManagedDir::write_atomic`、#623 PR-1b）。
    RenameFile,
    /// フォルダの中の一覧（`ManagedDir::list`、#623 PR-1b）。
    ListDir,
}

impl fmt::Display for IoOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::CreateDir => "フォルダの作成",
            Self::InspectDir => "フォルダの確認",
            Self::ReadFile => "ファイルの読み込み",
            Self::RemoveFile => "ファイルの削除",
            Self::OpenFile => "ファイルの確認",
            Self::CreateFile => "ファイルの作成",
            Self::WriteFile => "ファイルへの書き込み",
            Self::RenameFile => "ファイルの名前の付け替え",
            Self::ListDir => "フォルダの一覧",
        })
    }
}

/// フォルダの ACL が期待と違う理由（[`crate::acl`]）。
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

/// 辿らずに開いた物が、その用途に使えない理由（#623 PR-1b。`ManagedDir` の操作・
/// [`crate::nofollow::open_file_verified`]・`open_root`／`open_managed`）。どれも**開いたハンドルから
/// 読んだ**属性で決める（名前・列挙の時点の属性では決めない）。
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Refusal {
    /// reparse point（ジャンクション・シンボリックリンクのほか、WOF（`compact`）・重複除去・
    /// クラウドのプレースホルダも - tag を問わず断る。fail closed）。`tag` は reparse の tag
    /// （`IO_REPARSE_TAG_*`。ログで見分けるため。Fable B-4-4）。読めなかったときは 0。
    ReparsePoint { tag: u32 },
    /// ファイルを期待したのにフォルダだった。
    NotAFile,
    /// フォルダを期待したのにファイルだった。
    NotADirectory,
    /// ファイルにハードリンクの別名がある（リンク数が 1 でない）。
    HardLinked { links: u32 },
    /// 開いた物の本当のパスが、期待の場所と違う（途中の要素がリンクで外へ出た・木の外へ
    /// 移された）。
    Elsewhere { actual: std::path::PathBuf },
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReparsePoint { tag } => {
                write!(
                    f,
                    "リンク（reparse point、tag {tag:#010x}）なので辿りません"
                )
            }
            Self::NotAFile => f.write_str("ファイルではありません（フォルダです）"),
            Self::NotADirectory => f.write_str("フォルダではありません"),
            Self::HardLinked { links } => {
                write!(f, "ハードリンクの別名があります（リンク数 {links}）")
            }
            Self::Elsewhere { actual } => write!(
                f,
                "本当の場所が期待と違います（途中にリンク: {}）",
                actual.display()
            ),
        }
    }
}

/// この crate のすべての失敗。
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum SafeRootError {
    #[error("{op}に失敗しました（{kind}{}）", os_suffix(*.os_error))]
    Io {
        op: IoOp,
        kind: io::ErrorKind,
        os_error: Option<i32>,
    },
    #[error("インストール利用者の SID が使えません: {0}")]
    InvalidUserSid(&'static str),
    #[error("フォルダの権限が想定と違います: {0}")]
    AclMismatch(AclMismatch),
    #[error("フォルダの権限の操作に失敗しました（{call}、os error {os_error}）")]
    AclApi { call: &'static str, os_error: u32 },
    #[error("この OS ではフォルダの権限を設定できないため使えません")]
    UnsupportedPlatform,
    /// `.`・`..`（点だけの名前）を含むパス。作るときに OS が正規化して、確かめた
    /// 場所と別の場所を作りうるため断る（#392 B2-4a、PR #604 のレビュー）。
    #[error("フォルダのパスに「.」や「..」が含まれているため使えません")]
    DotSegmentInPath,
    /// 作ったファイルを読み戻したら保護されていなかった（`PinnedDir::create_child_file`、
    /// #630）。作ったファイルは消してある。中身は [`crate::acl::exposure`] の理由。
    #[error("作ったファイルが保護されていません（{0:?}）")]
    Exposed(Vec<crate::acl::Exposure>),
    /// 握ったフォルダの中の名前（または握ったフォルダそのもの）が、その用途に使えない
    /// （#623 PR-1b）。`path` は表示用（開き直すのには使わない）。
    #[error("{} は使えません: {refusal}", path.display())]
    Refused {
        path: std::path::PathBuf,
        refusal: Refusal,
    },
}

impl SafeRootError {
    // 使うのは Windows の実装だけ（非 Windows の `imp` は UnsupportedPlatform を返す）。
    #[cfg_attr(not(windows), allow(dead_code))]
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

pub type Result<T> = std::result::Result<T, SafeRootError>;
