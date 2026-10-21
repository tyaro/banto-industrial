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
//! ここ（ホスト側）へ移った）。
//!
//! T17-1（docs/banto-hub-t17-design.md §3「T17-1」・P1）で、env 読み取り
//! 自体は3ホスト共通の[`banto_hub_core::profile_paths::build_hub_config_from_env`]
//! へ一本化した - 以前この関数はこのファイル自身が定義していたが、
//! `win_service.rs`（サービスホスト）・`apps/banto-hub/src-tauri`
//! （デスクトップシェル）の3ホストが個別に複製していたロジックを1箇所へ
//! 統合した。`BANTO_DB`/`BANTO_ALLOW_SETUP`/`PORT`/`BANTO_BIND`/
//! `BANTO_HUB_DATA`の読み取り自体（各既定値・レイヤー順）は移設前と1バイトも
//! 変えていない - 新たに`BANTO_HUB_PROFILE`/`BANTO_HUB_ROOT`が加わり、
//! `BANTO_DB`/`BANTO_HUB_DATA`未設定時の既定値が相対パスから profile の
//! 絶対パスへ変わった（`profile_paths`のモジュール doc 参照）。
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
//!   直接叩く想定ではない。#661 661-2 から、サービスは profile を運用モードの
//!   形に締めてから Hub を動かし、STOP で緩める。サービスの**開始の引数**
//!   （`sc start BantoHub relax-only`・`Start-Service` の `-ArgumentList` 相当。
//!   コマンドラインの引数ではない）に `relax-only` ちょうどを渡すと、Hub を
//!   動かさずに緩めるだけで止まる（締めたまま残ったときの戻し方。それ以外の
//!   開始の引数は何も変えずに終了コード 3）
//!
//! 実行手順は docs/banto-hub-operations.md 参照。
//!
//! ## package（R1-7a）
//!
//! このファイルと `banto-hub-elev.rs`・`banto_hub/` は、R1-7a
//! （docs/banto-driver-api-design.md §2.4）で `banto-hub-core` の `src/bin/` から
//! この package（`banto-hub-host`、`apps/banto-hub/host`）へ移した。組み込みドライバ
//! （`banto-drivers`）を normal で持つのはこの package で、`banto-hub-core` のライブラリは
//! [`HubDrivers`] を注入で受け取るだけ（§6.4）。ビルドは
//! `cargo build -p banto-hub-host --bin banto-hub [--features embed-ui]`。
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

// R1-7a: `banto-hub-elev` のマニフェストの埋め込みを確かめるテスト（置き場所の理由は
// そのファイルのモジュール doc）。
#[cfg(test)]
#[path = "../manifest_tests.rs"]
mod manifest_tests;

use banto_hub_core::broker_glue::HubDrivers;
use banto_hub_core::profile_lock::HubHostKind;
use banto_hub_core::profile_paths::build_hub_config_from_env;
use banto_hub_core::runtime::HubRuntime;

/// R1-3b: この bin が組み立てる組み込みドライバ一式（コンソール・Windows サービス
/// 共通）。コアは `HubDrivers` を注入で受け取るだけで具体的なドライバを知らない
/// （docs/banto-driver-api-design.md §6.4）。
fn hub_drivers() -> HubDrivers {
    let builtin = banto_drivers::builtin();
    // R2b-1a: `modbus-tcp` / `slmp`（`HostBound`）に、host が I/O を持つ `push` /
    // `postgres` / `virtual`（`HostProvided`）を足す。R2b-1d から Hub の入口・収集の計画・
    // 移行の preflight がこのレジストリを正にする（docs/banto-hub-connection-settings-design.md §4）。
    let mut registry = builtin.registry;
    banto_hub_core::register_host_provided(&mut registry)
        .expect("host-provided driver ids are distinct from the built-ins");
    HubDrivers::new(
        std::sync::Arc::new(registry),
        builtin.broker,
        builtin.client_factory,
    )
}

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
        eprintln!("                サービスの開始の引数に relax-only を渡すと、Hub を動かさずに");
        eprintln!("                profile の ACL を緩めるだけで止まる（例: sc start BantoHub relax-only）");
    }
    #[cfg(not(windows))]
    {
        eprintln!("  install / uninstall / run-service は Windows 専用です（このビルドでは無効）");
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
        let config = build_hub_config_from_env(HubHostKind::Console, hub_drivers());
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
