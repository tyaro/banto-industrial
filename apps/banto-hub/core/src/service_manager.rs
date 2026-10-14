//! T17-0（docs/banto-hub-t17-design.md §3「T17-0」・§4「T16-2 への引き渡し
//! 契約」）: SCM（Service Control Manager）状態取得＋start/stop/restart/
//! autostart 操作を、`bin/banto_hub/win_service.rs` から**ホスト非依存の
//! ロジック層**として抽出したもの。
//!
//! **B2-3（#392 段階 B）: 型と SCM 操作の実装は共有 crate
//! [`banto_service_control`]（`crates/banto-service-control`）へ移った。**
//! このモジュールはそれを再公開（`pub use banto_service_control::*`）し、
//! banto-hub 固有の定数（[`SERVICE_NAME`]・[`RUN_SERVICE_ARG`]・
//! `BantoHubSink`の名前）と`BantoHub`向けのコンストラクタ
//! （[`hub_service_manager`]）だけを持つ。既存の
//! `banto_hub_core::service_manager::...`の参照は無改造で動く。以下の
//! 節は T17-0・S6 当時の経緯の記録（型の説明は移動先の crate 冒頭 doc 参照）。
//!
//! ## この T17-0 で行ったこと・行っていないこと
//!
//! - [`ServiceManager`] trait・[`ScmState`]/[`ServiceStatusSummary`]/
//!   [`ServiceManagerError`]/[`TransitionHandle`] という**ホスト非依存**の
//!   型を新設した - Windows API（`windows-service`クレート）に一切触れない
//!   ので非 Windows（このワークスペースの CI）でも単体テストできる
//!   （[`MockServiceManager`]、このファイル末尾の `tests` モジュール参照）。
//! - Windows 実 SCM 実装（[`WindowsServiceManager`]、`#[cfg(windows)]`）を
//!   同じ trait で提供する - `windows-service`クレートを直接叩くのは
//!   `win_service.rs`とこのファイルの2箇所になったが、サービス名
//!   （[`SERVICE_NAME`]）とサービス起動引数（[`RUN_SERVICE_ARG`]）は
//!   このファイル側の定数を`win_service.rs`が再利用する形にして重複を
//!   避けた。
//! - **T17-0 時点では既存の`install`/`uninstall`/`run-service`CLI
//!   （`win_service.rs`）を一切変更していなかった** - 起動種別が常に
//!   `AutoStart`+遅延自動開始のままなのも含め挙動不変（P4「Demand 化」は
//!   当時 T17-4 で扱うとして、このファイルでは着手しなかった）。P4 は
//!   その後 T17-4（2026-08-10、`service_install.rs`のモジュール doc・
//!   docs/banto-hub-t17-design.md §11 参照）で実装済み - `install`の既定
//!   起動種別は`OnDemand`（手動）になった。[`WindowsServiceManager`]は
//!   「これから T16-2 等が使う個別操作 API」であって、既存 CLI 経路を
//!   置き換えるものではない点は変わらない。
//! - `install`/`uninstall`自体は trait に含めていない（設計 §4 の契約が
//!   要求する最小面は query/start/stop/restart/set_auto_start のみ - 実装
//!   指示のとおり、必須ではない拡張は避けた）。
//!
//! ## S6（docs/banto-hub-external-db-design.md §5.5・§7 row S6、2026-09-07）:
//! サービス名の汎化
//!
//! デスクトップシェルの「サービス」一覧（`BantoHub`・`BantoHubSink`の両方の
//! SCM 状態を表示し、`BantoHubSink`は起動・停止も行う）のために、
//! [`WindowsServiceManager`]が対象にするサービス名・表示名・起動引数を
//! コンストラクタ引数へ持ち上げた（[`WindowsServiceManager::for_service`]）。
//! 既存の`WindowsServiceManager::new`は`for_service(SERVICE_NAME,
//! SERVICE_DISPLAY_NAME, executable_path, vec![RUN_SERVICE_ARG])`の薄い
//! ラッパーだった（B2-3 で[`hub_service_manager`]に移った）が、`BantoHub`向けの挙動は一切変えていない -
//! `win_service.rs`・トレイのサービス切替（`apps/banto-hub/src-tauri`）と
//! いった既存呼び出し元は無改造で動く。[`SINK_SERVICE_NAME`]/
//! [`SINK_SERVICE_DISPLAY_NAME`]は`apps/banto-hub-sink/src/service.rs`の
//! 同名定数と同じ値を複製したもの - Tauri シェルは`banto-hub-sink`クレートに
//! 依存させたくない（デスクトップシェルは`banto-hub-core`だけに依存する設計、
//! 本ファイル冒頭の doc 参照）ため、値の複製で済ませる
//! （`plcConnectionForm.ts`が`POSTGRES_PROTOCOL`を複製するのと同じ判断）。
//!
//! **既知の制限（このスライス時点、PR 本文にも記載）→ S6 レビュー指摘で
//! 解消済み**: この S6 スライス時点では`BantoHubSink`の SCM オブジェクトに
//! `banto-hub-elev.exe grant-service-acl`が付与する ACE
//! （`service_elevated.rs`参照、当時`BantoHub`固定）が付与されておらず、
//! `BantoHub Operators`グループのメンバー（管理者ではない一般オペレータ）が
//! `BantoHubSink`を起動・停止しようとすると Win32 の`ERROR_ACCESS_DENIED`
//! （[`ServiceManagerError::AccessDenied`]）になりうる gap があった。
//! 直後の follow-up（`service_elevated.rs`のモジュール doc「S6 申し送りの
//! follow-up」節）で`grant-service-acl`を`[service-name]`引数付きへ一般化し、
//! `apps/banto-hub-sink/src/service.rs::grant_service_acl`
//! （`banto-hub-sink.exe grant-service-acl`サブコマンド）が
//! `banto-hub-elev.exe grant-service-acl BantoHubSink`を呼ぶことで
//! `BantoHubSink`にも同じ ACE を付与できるようにした - フルの Windows
//! 管理者（既定で全サービスの SC_MANAGER_ALL_ACCESS 相当を持つ）は元々
//! 操作できていた。実機（Windows）で`BantoHubSink`を実際にインストールし
//! `sc sdshow BantoHubSink`で ACE の付与・Operators でのシェル起動停止を
//! 確認するのは、この worktree には実サービスをインストールしない制約が
//! あるため引き続き手動確認事項（PR 本文の手動確認手順参照）。

/// SCM 上のサービス名。`bin/banto_hub/win_service.rs`の`install`/
/// `uninstall`/`run-service`と[`WindowsServiceManager`]が共通で使う単一の
/// ソース（以前は`win_service.rs`だけが`"BantoHub"`を定義していた）。
pub const SERVICE_NAME: &str = "BantoHub";

/// `BantoHub`の SCM 表示名（`win_service.rs::install`と同じ値）。S6 の
/// [`WindowsServiceManager::for_service`]汎化に伴い、`windows_impl`内部
/// 専用だった定数をここへ公開した（モジュール doc「サービス名の汎化」節
/// 参照）。
pub const SERVICE_DISPLAY_NAME: &str = "banto-hub タグサーバー";

/// `BantoHubSink`（外部 DB 連携 S5 のサイドカー）の SCM サービス名。
/// `apps/banto-hub-sink/src/service.rs::SERVICE_NAME`と同じ値の複製（モジュール
/// doc「サービス名の汎化」節参照）。
pub const SINK_SERVICE_NAME: &str = "BantoHubSink";

/// `BantoHubSink`の SCM 表示名。`apps/banto-hub-sink/src/service.rs
/// ::SERVICE_DISPLAY_NAME`と同じ値の複製。
pub const SINK_SERVICE_DISPLAY_NAME: &str = "banto-hub DB Sink サイドカー";

/// SCM がサービス開始時に子プロセスへ渡す起動引数
/// （`bin/banto-hub.rs`の`run-service`サブコマンド）。[`WindowsServiceManager`]
/// が起動種別変更時にサービス登録を再構築する際、`win_service.rs::install`と
/// 同じ起動コマンドラインを再現するために使う。
pub const RUN_SERVICE_ARG: &str = "run-service";

pub use banto_service_control::*;

/// [`ServiceManagerError`]を banto-hub の利用者に見せる文言にする。
///
/// #392 B2-4a で共有 crate の`Display`が製品名を含まない中立な文言に
/// なった（先頭の`banto-hub: `が無く、権限不足は「管理者」を名指し
/// しない）。banto-hub の画面・ログ・`banto-hub-elev`の出力は B2-3 まで
/// と同じ文言を出し続けるため、利用者に見える箇所（シェルの Sink の
/// 起動・停止、[`crate::host_switch`]の`Faulted`の理由、
/// [`crate::service_elevated::ElevatedError::ServiceManager`]）はこの関数を
/// 通す。文言は B2-3 までの`Display`と 1 文字も変えない（golden テスト）。
pub fn describe_service_error(err: &ServiceManagerError) -> String {
    match err {
        ServiceManagerError::NotFound(name) => {
            format!("banto-hub: サービス '{name}' が見つかりません（未登録の可能性）")
        }
        ServiceManagerError::AccessDenied => {
            "banto-hub: サービス操作の権限がありません（管理者権限が必要です）".to_string()
        }
        ServiceManagerError::Timeout(timeout) => {
            format!("banto-hub: サービスの状態遷移が {timeout:?} 以内に完了しませんでした")
        }
        ServiceManagerError::Other(detail) => {
            format!("banto-hub: サービス操作に失敗しました: {detail}")
        }
    }
}

/// `BantoHub`向けの[`WindowsServiceManager`]を作る（B2-3 までの
/// `WindowsServiceManager::new(executable_path)`と同じ内容: サービス名・
/// 表示名・起動引数は[`SERVICE_NAME`]・[`SERVICE_DISPLAY_NAME`]・
/// [`RUN_SERVICE_ARG`]）。`executable_path`は`set_auto_start`での再登録に使う
/// 実行ファイルパスで、通常は`std::env::current_exe()`を渡す。
#[cfg(windows)]
pub fn hub_service_manager(executable_path: std::path::PathBuf) -> WindowsServiceManager {
    WindowsServiceManager::for_service(
        SERVICE_NAME,
        SERVICE_DISPLAY_NAME,
        executable_path,
        vec![std::ffi::OsString::from(RUN_SERVICE_ARG)],
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn constants_keep_their_values() {
        assert_eq!(super::SERVICE_NAME, "BantoHub");
        assert_eq!(super::RUN_SERVICE_ARG, "run-service");
        assert_eq!(super::SINK_SERVICE_NAME, "BantoHubSink");
    }

    /// banto-hub の利用者に見える文言は B2-3 までの`Display`と同じ
    /// （B2-4a で共有 crate の文言を中立にした後も変わらない）。
    #[test]
    fn describe_service_error_keeps_the_previous_hub_wording() {
        use super::{describe_service_error, ServiceManagerError};
        use std::time::Duration;
        assert_eq!(
            describe_service_error(&ServiceManagerError::NotFound("BantoHub".to_string())),
            "banto-hub: サービス 'BantoHub' が見つかりません（未登録の可能性）"
        );
        assert_eq!(
            describe_service_error(&ServiceManagerError::AccessDenied),
            "banto-hub: サービス操作の権限がありません（管理者権限が必要です）"
        );
        assert_eq!(
            describe_service_error(&ServiceManagerError::Timeout(Duration::from_millis(1500))),
            "banto-hub: サービスの状態遷移が 1.5s 以内に完了しませんでした"
        );
        assert_eq!(
            describe_service_error(&ServiceManagerError::Other("os error 1056".to_string())),
            "banto-hub: サービス操作に失敗しました: os error 1056"
        );
    }

    /// 共有 crate の型が`service_manager`経由で引き続き使えること
    /// （再公開の確認）。
    #[test]
    fn shared_types_are_reexported() {
        use super::{MockServiceManager, ScmState, ServiceManager};
        let manager = MockServiceManager::new();
        manager.start().expect("start");
        assert_eq!(manager.current_state(), ScmState::Running);
    }

    // `BantoHubSink`の名前定数がそのまま`for_service`に渡せる形
    // （`&'static str`）であることの型チェックを兼ねる - デスクトップ
    // シェルの「サービス」一覧が使う構築経路と同じ形。読み取り専用の
    // `query_status`だけを実 SCM に対して行う（インストール・起動・停止は
    // しない）。
    #[cfg(windows)]
    #[test]
    fn for_service_accepts_the_sink_service_constants() {
        use super::{
            ServiceManager, WindowsServiceManager, SINK_SERVICE_DISPLAY_NAME, SINK_SERVICE_NAME,
        };
        let manager = WindowsServiceManager::for_service(
            SINK_SERVICE_NAME,
            SINK_SERVICE_DISPLAY_NAME,
            std::path::PathBuf::new(),
            vec![],
        );
        // `BantoHubSink`が本当にこの環境へインストールされている場合
        // （通常は無い想定）でも、どちらの状態でも失敗しないことだけ
        // 確認する（インストール有無を前提にしない）。
        let status = manager.query_status();
        assert!(status.is_ok());
    }
}
