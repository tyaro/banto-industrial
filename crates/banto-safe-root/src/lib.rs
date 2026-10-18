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
//! - [`root`] モジュール（#623 PR-1b）: ルートと管理するフォルダを辿らずに開いて**握り**
//!   （[`open_root`]・[`SafeRoot::open_managed`]）、その中の名前 1 要素を握ったハンドル基準で
//!   作る・追記する・読む・原子的に置き換える・消す・列挙する（[`ManagedDir`]）。パスで開き直す
//!   相手（SQLite など）へは確かめてから [`ReopenPath`] を渡す。[`Mode::Plain`] は同じ API を
//!   `std::fs` で（デスクトップ・非 Windows）。
//! - [`nofollow`] モジュール（#623 PR-1b）: パスの版の小さな部品。ファイル 1 つを辿らずに開いて、
//!   ハンドルで reparse point でない・ファイル・リンク数 1・本当のパスが渡されたパスと同じ、を
//!   確かめる（`banto-instance-lock` の診断ファイル）。
//! - [`mode`] モジュール（#623 PR-1c）: 設定モード ⇔ 運用モードの切り替えで、サービス自身が
//!   木の全体の ACL を締める・緩める部品（`PinnedDir::{scan_tree, close_tree, relax_tree}`、
//!   印のファイル `ensure_marker`、確かめ直し `recheck`）と、ルートの DACL からモードを
//!   見分ける純関数（[`classify_mode`]・[`observe_root`]）。2 つの形は [`ModeAccess`]。
//! - [`redirection`] モジュール（#623 の (3b)）: プロセスの Redirection Guard を有効にする
//!   （[`enable_redirection_guard`]。配線は各アプリ）。
//! - [`error`] モジュール: [`SafeRootError`]・[`Refusal`]。文言は製品に依らない中立の形で、
//!   置き場の名前（「データのルート」「profile」など）は持たない。
//!
//! # 対象の OS（Windows）
//!
//! POSIX の名前の付け替え・削除（`FileRenameInfoEx`・`FileDispositionInfoEx`）は Windows 10 1607
//! 以降（1809 以降を推奨）、ハードリンクの mitigation（作るのにリンク先への書き込み権が要る）は
//! 1903 以降、Redirection Guard は Windows 11 22H2 以降・Server 2022 の更新以降。
//!
//! # 依存の向き
//!
//! ```text
//! windows-sys / thiserror
//!         ▲
//!  banto-safe-root   <- この crate
//!    ▲        ▲
//!    │        └── banto-instance-lock（診断ファイルの確かめは [`nofollow`]。#623 PR-1b）
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
#[cfg(windows)]
mod handle_io;
pub mod mode;
pub mod nofollow;
pub mod redirection;
pub mod root;

pub use acl::{DirAccess, InstallUserSid, UserRight};
pub use error::{AclMismatch, IoOp, Refusal, Result, SafeRootError};
pub use mode::{
    classify_mode, observe_root, AbortReason, CloseAbort, MarkerError, MarkerOutcome, ModeAccess,
    ModeObservation, Progress, RecheckFailure, RelaxReport, RootObservation, ScanRefusal,
    ScanReport, StopSignal, TreeProblem, MARKER_NAME,
};
pub use redirection::{enable_redirection_guard, RedirectionGuard};
pub use root::{
    open_root, ManagedDir, ManagedSet, ManagedSpec, ManagedStop, Mode, ReopenPath, RootRefusal,
    RootSpec, SafeRoot,
};
