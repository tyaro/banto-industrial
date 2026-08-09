//! banto-hub の実行エントリポイント (`banto-hub.exe`) — CLI ディスパッチャ。
//! `apps/chronogazer/core/src/bin/banto-serve.rs` を踏襲するが、Tauri
//! アプリのプレビュー用ではなく banto-hub 自身の**唯一の**起動経路である
//! （設計 §3.1: 「Tauri は使わない」）。
//!
//! T5-1（docs/tag-server-design.md §8「常駐」・docs/t5-handoff.md §3）:
//! v1 はコンソール起動のみだったが、Windows サービス化
//! （`windows-service`クレート、本実装）を追加した。起動〜シャットダウンの
//! 実処理シーケンス（DB初期化・各サービス構築・`CollectorManager::
//! rebuild`・MQTT/gRPC起動・axumサーバー起動・シャットダウン順序）は
//! T14-1（docs/banto-hub-t14-design.md §3「D1」）で lib クレート
//! （`banto_hub_core`）側の [`banto_hub_core::runtime::HubRuntime`]へ抽出
//! された - コンソールモード・サービスモードのどちらもそれを呼ぶ。この
//! ファイル自身は「どちらのモードで、何を停止トリガーにして呼ぶか」の
//! 分岐と、環境変数を読んで [`banto_hub_core::runtime::HubConfig`]を
//! 組み立てる役目だけを持つ（env 読み取りは T14-1 で composition root から
//! ここ（ホスト側）へ移った - `crate::build_hub_config`参照。読み取り結果は
//! 移設前と1バイトも変えていない）。
//!
//! ## サブコマンド（引数なしの既存挙動は一切変更していない）
//!
//! - 引数なし: 従来通りコンソールモードで起動（Ctrl-C で停止）
//! - `install`（Windows専用）: Windows サービスとして登録
//!   （[`win_service::install`]）
//! - `uninstall`（Windows専用）: サービス登録を解除
//!   （[`win_service::uninstall`]）
//! - `run-service`（Windows専用）: SCM がサービス開始時に呼ぶ内部
//!   エントリポイント（[`win_service::run_service_dispatcher`]）。人間が
//!   直接叩く想定ではない
//!
//! 実行手順は docs/banto-hub-operations.md 参照。
//!
//! ## クロスプラットフォームビルドについて
//!
//! banto-hub 自体は Windows 専用前提（設計 §8、tstore と同じ）だが、この
//! バイナリはワークスペースの一員として非 Windows でも`cargo check`/
//! `cargo build`が通る必要がある。`windows-service`依存
//! （`Cargo.toml`の`[target.'cfg(windows)'.dependencies]`）とそれを使う
//! [`win_service`]モジュールは`#[cfg(windows)]`で二重にガードしてあり、
//! 非 Windows ビルドにはどちらも一切含まれない - `install`/`uninstall`/
//! `run-service`サブコマンドは非 Windows では単に「Windows専用」エラーで
//! 案内するだけの分岐になる。
//!
//! ## ランタイムの構築について
//!
//! 以前は`#[tokio::main]`が単一の tokio ランタイムを自動生成していたが、
//! サービスモード（[`win_service::run_service_dispatcher`]が
//! `service_dispatcher::start`という**同期・ブロッキング**な Win32 API
//! 呼び出しを行い、SCM が別スレッドで呼ぶ`service_main`の中でさらに
//! 独自の tokio ランタイムを`block_on`する）と両立させるため、`main`自体は
//! 素の同期関数にし、コンソールモードだけがこのファイルで直接ランタイムに
//! 入る形にリファクタした（tokio はネストしたランタイムの`block_on`を
//! 許さないため、`main`をあらかじめ tokio ランタイム化しておくわけには
//! いかない）。

// このサポートモジュールは `src/bin/banto_hub/`（サブディレクトリ）に
// 置いてある - Cargo は `src/bin/*.rs`（直下のファイル）をそれぞれ独立した
// バイナリターゲットとして自動検出してしまう（`[[bin]]`で明示済みの
// `banto-hub`とは別に、`win_service.rs`単体にも`fn main()`を要求される）
// ため、直下ではなくサブディレクトリ + `#[path]`で読み込む。
#[cfg(windows)]
#[path = "banto_hub/win_service.rs"]
mod win_service;

use banto_hub_core::runtime::{HubConfig, HubRuntime, DEFAULT_DB_PATH};

fn main() {
    match std::env::args().nth(1).as_deref() {
        None => run_console(),
        #[cfg(windows)]
        Some(arg) if arg == win_service::INSTALL_ARG => win_service::install(),
        #[cfg(windows)]
        Some(arg) if arg == win_service::UNINSTALL_ARG => win_service::uninstall(),
        #[cfg(windows)]
        Some(arg) if arg == win_service::RUN_SERVICE_ARG => win_service::run_service_dispatcher(),
        Some(other) => {
            eprintln!("banto-hub: 不明な引数です: '{other}'");
            print_usage();
            std::process::exit(2);
        }
    }
}

fn print_usage() {
    eprintln!("使い方: banto-hub.exe [install|uninstall|run-service]");
    eprintln!("  （引数なし）  コンソールモードで起動（Ctrl-C で停止）");
    #[cfg(windows)]
    {
        eprintln!("  install       Windows サービスとして登録（管理者権限が必要）");
        eprintln!("  uninstall     サービス登録を解除（管理者権限が必要）");
        eprintln!("  run-service   SCM 専用の内部エントリポイント（直接実行しないでください）");
    }
    #[cfg(not(windows))]
    {
        eprintln!("  install / uninstall / run-service は Windows 専用です（このビルドでは無効）");
    }
}

/// 環境変数を読んで [`HubConfig`]を組み立てる - T14-1 より前は
/// composition root（旧 `hub_run::run`）自身がこの読み取りを行っていたが、
/// 将来のデスクトップホストが env なしで設定できるようにする設計判断
/// （docs/banto-hub-t14-design.md §3「D1」）で、ホスト側であるこのファイルへ
/// 移した。**読み取りロジック（既定値・レイヤー順）は移設前と1バイトも
/// 変えていない** - 各フィールドの doc comment に旧実装との対応を記す
/// （[`HubConfig`]自身のフィールド doc も参照）。コンソール・サービス
/// 両モードから共有する（`win_service`側は `crate::build_hub_config`と
/// して参照する - `mod win_service`はこのファイルの子モジュールなので
/// `crate::`で届く）。
fn build_hub_config() -> HubConfig {
    HubConfig {
        db_path: std::env::var("BANTO_DB").unwrap_or_else(|_| DEFAULT_DB_PATH.to_string()),
        allow_setup: std::env::var("BANTO_ALLOW_SETUP")
            .map(|value| value == "1")
            .unwrap_or(false),
        port_override: std::env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok()),
        bind_override: std::env::var("BANTO_BIND").ok(),
        data_dir_override: std::env::var("BANTO_HUB_DATA")
            .ok()
            .map(std::path::PathBuf::from),
    }
}

/// コンソールモード（既存挙動そのまま）: 独自に tokio ランタイムを構築し、
/// [`HubRuntime::start`]→ Ctrl-C 待機 → [`banto_hub_core::runtime::RunningHub::shutdown`]
/// を駆動する（旧 `hub_run::run(shutdown)`の「構築 → shutdown.await →
/// teardown」を、T14-1 の制御反転に合わせて呼び出し側で組み立てている -
/// 中身のシーケンス自体は不変）。
fn run_console() {
    let runtime = tokio::runtime::Runtime::new().expect("failed to build tokio runtime");
    runtime.block_on(async {
        let config = build_hub_config();
        // 旧 `hub_run::run`はここで `expect("init_db should succeed")`等の
        // 4箇所が panic していた（設計 §2「現行コード地図」）。T14-1 で
        // `Result`化した分、コンソールでは同等の異常終了（panic ではないが
        // 即座にエラーを表示してプロセスを終了する）に読み替える - 文言は
        // 簡潔なものに変えたが、「起動失敗はプロセスを続行させない」という
        // 挙動は不変（実装指示 T14-1 §6）。
        let hub = match HubRuntime::start(config).await {
            Ok(hub) => hub,
            Err(err) => {
                eprintln!("banto-hub: {err}");
                std::process::exit(1);
            }
        };
        tokio::signal::ctrl_c()
            .await
            .expect("failed to listen for ctrl-c");
        hub.shutdown().await;
    });
}
