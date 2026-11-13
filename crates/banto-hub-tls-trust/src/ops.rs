//! 置き場を読むために OS に対してする操作（読み取りだけ）。Windows の実装は
//! [`crate::win::WindowsTlsTrustOps`]、テストは偽物。書き込み・作成の操作は banto-hub-core の
//! `hub_tls_prepare::HubTlsOps`（この trait を親に持つ）にある。

use std::path::{Path, PathBuf};

use banto_safe_root::acl::{FileSecurity, PinnedDirInfo};
use banto_safe_root::guard::MachineEnvironment;

/// 辿らずに開いたフォルダ（Windows は `banto_safe_root::acl::PinnedDir` を
/// `banto_safe_root::ManagedDir` に包んだもの）。
pub trait TlsDir {
    /// 開いたハンドルから読んだ属性と所有者。
    fn info(&self) -> PinnedDirInfo;
    /// 本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 開いたハンドルから読んだ属性・所有者・DACL。
    fn security(&self) -> Result<FileSecurity, String>;
}

/// 置き場のハンドル基準に、読み取りの権利で辿らずに開いたファイル（Windows は
/// `banto_safe_root::acl::PinnedFile`）。持っている間はほかの誰も書き込みに開く・消す・名前を
/// 変えることができない（共有は読み取りだけ）。
pub trait TlsFile {
    /// 開いた時点でハンドルから読んだ属性・リンク数・所有者・DACL。
    fn security(&self) -> FileSecurity;
    /// 本当のパス。
    fn final_path(&self) -> PathBuf;
    /// 開いたハンドルから、`max_len` バイトまで読む（超えたら断る）。
    fn read_all(&mut self, max_len: usize) -> Result<Vec<u8>, String>;
}

/// [`TlsTrustOps::open_child_file`] の失敗。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileOpenError {
    /// 別のプロセスが書き込みに開いている（共有違反）。
    InUse,
    /// それ以外（理由）。
    Other(String),
}

/// 置き場を確かめて読むための、読み取りだけの操作。
pub trait TlsTrustOps {
    /// マシンの環境の読み取り（ProgramData・既知のフォルダ。マシンの環境変数は使わない）。
    type Env: MachineEnvironment;
    /// [`TlsTrustOps::pin_dir`] などが返すもの。
    type Dir: TlsDir;
    /// [`TlsTrustOps::open_child_file`] が返すもの。
    type File: TlsFile;

    fn machine_env(&self) -> &Self::Env;
    /// フォルダを辿らずに開く（無ければ `Ok(None)`）。
    fn pin_dir(&self, dir: &Path) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素の子を辿らずに開く。
    fn open_child_dir(&self, parent: &Self::Dir, name: &str) -> Result<Option<Self::Dir>, String>;
    /// 確かめ済みの `parent` のハンドル基準に、名前 1 要素の子を読み取りで辿らずに開く（共有は
    /// 読み取りだけ）。
    fn open_child_file(
        &self,
        parent: &Self::Dir,
        name: &str,
    ) -> Result<Option<Self::File>, FileOpenError>;
}
