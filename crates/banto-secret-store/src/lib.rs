//! `banto-secret-store`（#392 B1-3、H10）: 秘密（Hub の API キーなど）を、
//! DPAPI で守ったファイルとして保護されたディレクトリに保存する共有 crate。
//!
//! 設計は docs/chronogazer-service-design.md §3.1(c)・§3.3・§5 の決定 4・§6。
//!
//! # なぜ OS キーリングではないか
//!
//! OS キーリング（Windows の資格情報マネージャー）は**利用者ごと**。ChronoGazer を
//! サービス（LocalSystem）として動かすと、利用者のキーリングに入れたキーを
//! サービスが読めず、サービスのキーをデスクトップ（利用者）が読めない。
//! 同じ Hub のキーをサービスとデスクトップの両方で使い続ける（決定 4）ため、
//! **PC で 1 つ**の保存先にする。
//!
//! # 構成
//!
//! - [`Protector`]: 平文 ⇔ blob。製品は [`DpapiMachine`]（Windows）、テストは
//!   `PlainProtector`（`test-support` feature、暗号化しない）。
//! - [`FileSecretStore`]: 1 アカウント 1 ファイル。ファイル名は
//!   `sha256(account)` の hex + `.bin`（[`file_name_for`]）、中身はマジック `BSS1`・
//!   版数・アカウント・blob（[`format`]）。entropy は
//!   `"banto-secret-store/v1:" + account`（[`entropy_for`]）なので、ファイルを
//!   別のアカウントの名前に入れ替えても（中のアカウントの欄を書き換えても）
//!   復号できない。読むときはファイルの中のアカウントの一致も確かめる。
//!   書き込みは同じディレクトリの一時ファイル（nonce を含む名前・`create_new`）→
//!   `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`（非 Windows は `rename`）で
//!   原子的に置き換え、一時ファイルは失敗・取消しでも消す。削除は「無ければ Ok」。
//!   プロセスの中は `Mutex` で 1 つずつ。
//! - [`acl`]: 保存先ディレクトリの保護された DACL（SYSTEM・Administrators に
//!   フル、指定したインストール利用者に「変更」、Users などには何も付けない）。
//!   無ければその DACL で作り、あれば確かめ、違えば失敗する（自動では直さない。
//!   詳細と理由は [`acl`] の doc）。
//! - `impl banto_hub_bootstrap::KeyStore for FileSecretStore<_>`（`src/keystore.rs`）。
//!
//! # DPAPI machine スコープの意味と限界（計画書 §6）
//!
//! `CRYPTPROTECT_LOCAL_MACHINE` で守った blob は、**同じ PC の上なら、ファイルを
//! 読めるすべてのプロセスと利用者が復号できる**。DPAPI が守るのは、別の PC への
//! コピー（バックアップ・ディスクの持ち出し）とオフラインの盗難だけ。
//! **同じ PC の中での本当の境界はファイル（ディレクトリ）の ACL** で、だから
//! この crate は ACL を設定できない環境（非 Windows）では
//! [`SecretStoreError::UnsupportedPlatform`] で fail closed する（保存先として
//! 使えない）。そのほかの限界:
//!
//! - OS キーリング（利用者スコープ）より広い: 管理者・SYSTEM と、ACL に載せた
//!   インストール利用者が読める。
//! - VM の複製先でも復号できる（machine の鍵ごと複製されるため）。
//! - OS を入れ直すと machine の鍵が変わって復号できなくなる（Hub のキーの
//!   再発行が要る）。
//!
//! # 秘密を外へ出さない
//!
//! キーの平文と blob は、ファイルの中（blob）と、呼び出し側へ返す値の中だけに
//! ある。この crate はログを一切出さない。[`SecretStoreError`] は平文・blob・
//! アカウントを持たず（操作の名前・`io::ErrorKind`・OS のエラー番号・SID だけ）、
//! [`FileSecretStore`] の `Debug` は手で実装してディレクトリ・ACL の指定・
//! protector の型名だけを出す。復号した平文は `Zeroizing` に入れて返し、
//! DPAPI が返したバッファは消してから `LocalFree` する。
//!
//! # この crate がしないこと
//!
//! - アプリへの配線（ChronoGazer の `chronogazer-server` は B1-5、デスクトップは
//!   B2-4 で使う）。
//! - 親ディレクトリ（例 `%ProgramData%\ChronoGazer\secrets\`）の ACL（B3-3 の
//!   昇格ヘルパの担当）。
//! - 孤児のファイルの掃除（DB の `HubRecord` とキーファイルは別々に書くので、
//!   片方だけ残ることはある。害は無い - 計画書 §6）。

pub mod acl;
pub mod error;
pub mod format;
mod keystore;
mod protector;
mod store;

#[cfg(test)]
mod tests;

pub use acl::{DirAccess, InstallUserSid};
pub use error::{Result, SecretStoreError};
#[cfg(windows)]
pub use protector::DpapiMachine;
#[cfg(any(test, feature = "test-support"))]
pub use protector::PlainProtector;
pub use protector::Protector;
pub use store::{entropy_for, file_name_for, FileSecretStore};
