//! `banto-safe-root`（#623 PR-1a）: 利用者が触れるかもしれないフォルダの木を、
//! LocalSystem のサービス・昇格ヘルパーが**辿らずに**扱うための、製品に依らない部品。
//!
//! `banto-secret-store` の `acl`（と、その下の `guard`）を、挙動を変えずにここへ移した
//! （履歴は `git mv` で残している）。設計は docs の #623 の PR-1 の設計
//! （共有 crate `banto-safe-root` とモードの切り替えでの ACL の付け替え）。
//!
//! # 構成
//!
//! - [`acl`] モジュール: ディレクトリの保護された DACL（SYSTEM・Administrators にフル、指定した
//!   インストール利用者に「変更」）の作成・確認・書き直し（[`DirAccess`]・
//!   [`InstallUserSid`]）。辿らずに開いて握るフォルダ・ファイルのハンドル
//!   （`PinnedDir`・`PinnedFile`）と、その DACL・所有者・リンク数の判定
//!   （`check_dacl`・`exposure`）。unsafe な Win32 はここに閉じる。
//! - [`guard`] モジュール: 昇格ヘルパーが DACL を書き直す前の確かめ（既知のフォルダ・マシンの環境
//!   からのルート・本当のパスの比較・所有者の確かめ・引き継ぐ利用者の選び分け）。
//!   判断は純関数で、断る理由は型の付いた値で返す。
//! - [`error`] モジュール: [`SafeRootError`]。文言は製品に依らない中立の形で、置き場の名前
//!   （「データのルート」「profile」など）は持たない。
//!
//! # 依存の向き
//!
//! ```text
//! windows-sys / thiserror
//!         ▲
//!  banto-safe-root   <- この crate
//!    ▲        ▲
//!    │        └── banto-instance-lock（PR-1b 以降で診断ファイルの確かめを寄せる）
//!  banto-secret-store ── apps（chronogazer-core / chronogazer-elev / banto-hub-core / banto-hub-sink）
//! ```
//!
//! 本番の依存は `windows-sys` と `thiserror` だけに限る。dev-dependency の
//! `banto-service-elev`（昇格判定）と `tempfile` はテストのためだけ。
//!
//! # banto へ移す条件
//!
//! この crate は industrial に置く（Windows サービスまわりの部品がすべて industrial に
//! あり、upstream の banto にサービスの層は無いため。2026-10-18 オーナー決定）。
//! banto をもとにした industrial 以外のアプリも Windows サービスとして動かすとき、または
//! banto の backup／audit spool をハンドル基準に変えるときに、サービスの部品群と
//! まとめて banto へ移す。そのため**依存を増やさず**、industrial のアプリ固有のもの
//! （ChronoGazer・Hub の名前や置き場）を含めない。
//!
//! # この crate がしないこと
//!
//! どのフォルダをどの名前で管理するか、どの利用者に何を付けるか（SID の出どころ）は
//! 各アプリが決めて渡す。ログも出さない。

pub mod acl;
pub mod error;
pub mod guard;

pub use acl::{DirAccess, InstallUserSid};
pub use error::{AclMismatch, IoOp, Result, SafeRootError};
