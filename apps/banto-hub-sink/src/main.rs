//! `banto-hub-sink.exe` の実行エントリポイント - CLI ディスパッチャ。
//! 実処理はすべて lib クレート（`banto_hub_sink`）側にある
//! （統合テストがプロセス内で同じ実行ループを回せるようにするため -
//! `banto_hub_sink::run` のモジュール doc 参照）。
//!
//! ## サブコマンド（banto-hub の `bin/banto-hub.rs` と同じ形）
//!
//! - 引数なし / `run`: コンソールモード（Ctrl-C で停止）
//! - `install`（Windows 専用）: Windows サービスとして登録
//! - `uninstall`（Windows 専用）: サービス登録を解除
//! - `run-service`（Windows 専用）: SCM がサービス開始時に呼ぶ内部
//!   エントリポイント（人間が直接叩く想定ではない）
//! - `grant-service-acl`（Windows 専用、S6 レビュー指摘の follow-up）:
//!   `BantoHubSink` サービスの DACL へ `BantoHub Operators` 向けの限定 ACE
//!   （query-config/query-status/start/stop のみ）を付与する - banto-hub が
//!   `banto-hub-elev.exe grant-service-acl` で行うのと同じ ACE
//!   （`service::grant_service_acl` のモジュール doc「サービス ACL の付与」
//!   節参照）。**`install` には含まれない** - banto-hub と同様、`install`
//!   の後に別の昇格ステップとして呼ぶ設計（アップグレード時に既存 ACL を
//!   無条件に触らないため）。**MSI/インストーラは `install` に続けてこの
//!   サブコマンドも呼ぶこと**（`sc sdshow BantoHubSink` で
//!   `BantoHub Operators` の ACE を確認できる）。
//!
//! MSI へのサービス登録の同梱は S6/S7 のスコープ（設計 §7）。
//!
//! ## ランタイムの構築について
//!
//! `#[tokio::main]` を使わず `main` を素の同期関数にしてあるのは
//! banto-hub と同じ理由 - サービスモードは `service_dispatcher::start`
//! という同期・ブロッキングな Win32 呼び出しの中で SCM が別スレッドから
//! 呼ぶ `service_main` の内側で独自のランタイムを `block_on` するため、
//! `main` をあらかじめランタイム化しておくとネストして panic する。

#[cfg(windows)]
use banto_hub_sink::service;
use banto_hub_sink::{load_config, run, SidecarOptions};

fn main() {
    match std::env::args().nth(1).as_deref() {
        None | Some("run") => run_console(),
        #[cfg(windows)]
        Some(arg) if arg == service::INSTALL_ARG => service::install(),
        #[cfg(windows)]
        Some(arg) if arg == service::UNINSTALL_ARG => service::uninstall(),
        #[cfg(windows)]
        Some(arg) if arg == service::RUN_SERVICE_ARG => service::run_service_dispatcher(),
        #[cfg(windows)]
        Some(arg) if arg == service::GRANT_SERVICE_ACL_ARG => service::grant_service_acl(),
        Some(other) => {
            eprintln!("banto-hub-sink: 不明な引数です: '{other}'");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!("使い方: banto-hub-sink.exe [run|install|uninstall|run-service|grant-service-acl]");
    eprintln!("  （引数なし）/ run  コンソールモードで起動（Ctrl-C で停止）");
    eprintln!(
        "  設定ファイル（{}）は BANTO_HUB_SINK_CONFIG → %ProgramData%\\BantoHub\\ → exe と\
         同じディレクトリ の順に探索します",
        banto_hub_sink::DEFAULT_CONFIG_FILE_NAME
    );
    #[cfg(windows)]
    {
        eprintln!("  install            Windows サービスとして登録（管理者権限が必要）");
        eprintln!("  uninstall          サービス登録を解除（管理者権限が必要）");
        eprintln!(
            "  run-service        SCM 専用の内部エントリポイント（直接実行しないでください）"
        );
        eprintln!(
            "  grant-service-acl  BantoHub Operators へのサービス ACL を付与（管理者権限が必要。\
             install の後に MSI/インストーラが呼ぶこと。banto-hub-elev.exe が同じディレクトリに必要）"
        );
    }
    #[cfg(not(windows))]
    {
        eprintln!(
            "  install / uninstall / run-service / grant-service-acl は Windows 専用です（このビルドでは無効）"
        );
    }
}

fn run_console() {
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            eprintln!("banto-hub-sink: {err}");
            eprintln!(
                "banto-hub-sink: 設定ファイルの雛形は docs/banto-hub-external-db-design.md §5.6、\
                 探索順は docs/banto-hub-installer-design.md §4.4 と \
                 このバイナリの README 相当（`banto_hub_sink::config` のモジュール doc）を参照してください"
            );
            std::process::exit(1);
        }
    };
    let runtime = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    let result = runtime.block_on(async {
        run(config, SidecarOptions::default(), async {
            // Ctrl-C を受けたら停止処理（残キューの flush → 最後の状態
            // push）へ入る。listen 自体に失敗するのは異常なので、その
            // 場合は「即停止」に倒す。
            if tokio::signal::ctrl_c().await.is_err() {
                eprintln!("banto-hub-sink: Ctrl-C の待受に失敗しました");
            }
        })
        .await
    });
    if let Err(err) = result {
        eprintln!("banto-hub-sink: 実行を開始できませんでした: {err}");
        std::process::exit(1);
    }
}
