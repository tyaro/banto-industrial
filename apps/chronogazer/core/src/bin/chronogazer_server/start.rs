//! `chronogazer-server` の起動の順序（#392 B3-2a）。コンソール（`run`）と、B3-2b の
//! Windows サービス（`run-service`）が同じ順序を通る。
//!
//! B1-5 と #598 の全体監査で決めた順序の約束を、ここ 1 か所に置く（呼び出し側ごとに
//! 写さない）:
//!
//! 1. ルートの解決と検証（[`resolve_layout`]。相対パスなどは終了コード 3）。
//!    これと 2. の間に、呼び出し側が止める合図の受け口と非同期ランタイムを用意する
//!    （コンソールは Ctrl のハンドラー。登録できなければ何も作らない）。
//! 2. ルートを作って A1 の排他を取る（`server_host::lock`。host の種類とルートの ACL の
//!    利用者の欄は呼び出し側が渡す）。取れなければルートの下に何も作らず、理由は
//!    コンソールだけに出して終了コード 2。
//! 3. ログを開いて標準出力・標準エラー出力をそこへ向け直す（[`StartHooks::open_log`]）。
//!    **リストア予約の適用と DB の初期化より前**（#598 のオーナーレビュー P2）。
//!    向け直せなければ起動を止める。
//! 4. 段の間で止める合図を見る（#598 監査 F3）→ リストア予約の適用と DB の初期化 →
//!    Hub のキーの保存先 → 止める合図を見る → エンジンの開始。段の途中（DB を開いて
//!    いる最中など）は止めない（checklist §5）。
//! 5. 失敗・中止は、理由と対処を出して**ログを書き出し終えてから**起動の失敗を知らせる
//!    （[`StartHooks::finish_start_failure`]。#598 監査 F1）。
//!
//! 開始の後（動いていることの公開・古いログの削除・待ち受けの案内・止める合図を待つ）は
//! 呼び出し側の担当（コンソールとサービスで公開の相手が違う）。返す [`Started`] の
//! `log` は保持日数の設定（古いログの削除）に使う。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chronogazer_core::instance_lock::ChronoGazerHost;
use chronogazer_core::paths::{resolve_data_root_from_env, DataLayout, DirAccessSource};
use chronogazer_core::server_host::{
    self, ServerLog, StartedServer, EXIT_START_FAILED, SERVER_LABEL,
};

use super::{Flush, LineSink, OpenedLog};

/// 起動の順序の呼び出し側（コンソール・サービス）が差し込むもの。
pub(crate) trait StartHooks {
    /// 段の間で見る止める合図。来ていればその起点の名前（中止の行に出す）。
    fn stop_requested(&self) -> Option<&'static str>;

    /// ログを開いて標準出力・標準エラー出力を向け直す（ログの出し先の選択）。
    /// 返す形は `open_log`（モジュールの親）と同じ。`Err` は起動を止める理由。
    fn open_log(&self, layout: &DataLayout) -> Result<OpenedLog, String>;

    /// ログを開いた後に、書き出しと記録先を受け取る（後始末とその記録に使う）。
    fn log_opened(&self, flush: Flush, record: Option<LineSink>);

    /// 起動の失敗・中止の終わり: 終了の行を出し、ログを書き出し終えてから起動の
    /// 失敗を知らせる（#598 監査 F1）。終了コードを返す。
    fn finish_start_failure(&self, code: i32) -> i32;

    /// DB を開く直前（デバッグビルドの統合テストの待ちの口。既定は何もしない）。
    fn before_open(&self) {}
}

/// 起動の順序に渡すもの。
pub(crate) struct StartSpec<'a> {
    pub layout: &'a DataLayout,
    /// A1 の診断ファイル・`GET /api/host`・`run\endpoint.json` に出す種類。
    pub host: ChronoGazerHost,
    /// データのルートと Hub の API キーの保存先を作るときの ACL の利用者の欄。
    pub access: &'a DirAccessSource,
}

/// 起動できたエンジンと、開いた日次ログ（開けなければ `None`）。
pub(crate) struct Started {
    pub server: Arc<StartedServer>,
    pub log: Option<Arc<Mutex<ServerLog>>>,
}

/// データのルートを解決して検証する（順序の 1.）。使えなければ理由を標準エラー出力に
/// 出して終了コード（3）を返す（ルートの下には何も作らない）。
pub(crate) fn resolve_layout() -> Result<DataLayout, i32> {
    let root = resolve_data_root_from_env();
    if let Err(message) = server_host::validate_root(&root) {
        eprintln!("{SERVER_LABEL}: {message}");
        return Err(EXIT_START_FAILED);
    }
    Ok(DataLayout::new(root))
}

/// 起動の順序の 2.〜5.（モジュール doc）。失敗・中止なら、理由を出してログを書き出し、
/// `hooks` に起動の失敗を知らせた後の終了コードを `Err` で返す。
pub(crate) fn start_server(
    runtime: &tokio::runtime::Runtime,
    spec: StartSpec<'_>,
    hooks: &impl StartHooks,
) -> Result<Started, i32> {
    let StartSpec {
        layout,
        host,
        access,
    } = spec;

    // 2. A1 の排他だけ（ルートの下に作るのはここから。排他を取れなければ何も
    // 作らない - ログもまだ開いていないので、理由はコンソールだけに出す）。
    let locked = match server_host::lock(layout.clone(), host, access) {
        Ok(locked) => locked,
        Err(failure) => return Err(report_start_failure(hooks, &failure)),
    };

    // 3. ログ（排他を取った後・リストア予約の適用と DB の初期化の前。#598 の
    // オーナーレビュー P2: DB を開けない理由と対処、リストア予約の警告を日次ログに
    // 残す。Windows だけ - 標準ハンドルの向け直しは Windows の API）。
    let (log, flush, record) = match hooks.open_log(layout) {
        Ok(opened) => opened,
        Err(reason) => {
            // 日次ログはこの bin の唯一の永続の診断（サービスではコンソールも無い）。
            // 向け直せないまま動かすと起動・停止・失敗の記録が残らないので、起動を
            // 止める（Ctrl のハンドラーの登録の失敗と同じ扱い。#598 の Copilot の指摘）。
            // 理由はログファイルへ直接（`open_log` が書いた）とコンソールに出す。
            eprintln!("{SERVER_LABEL}: {reason}");
            return Err(hooks.finish_start_failure(EXIT_START_FAILED));
        }
    };
    hooks.log_opened(flush, record);
    println!(
        "{SERVER_LABEL}: 起動します（版 {}、pid {}）",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    println!(
        "{SERVER_LABEL}: データのルート: {}",
        layout.root().display()
    );
    match &log {
        Some(log) => println!(
            "{SERVER_LABEL}: ログ: {}",
            log.lock()
                .map(|log| log.current_path())
                .unwrap_or_else(|_| PathBuf::from("?"))
                .display()
        ),
        None => println!("{SERVER_LABEL}: ログファイルには書きません（コンソールだけ）"),
    }

    // 起動の段の間で止める合図を見る（#598 監査 F3）: 合図の後に DB の初期化・
    // 待ち受け・収集の開始・`endpoint.json` まで進めない。段の途中（DB を開いている
    // 最中など）は止めない（DB の操作を途中で落とさない。checklist §5）。
    if let Some(origin) = hooks.stop_requested() {
        return Err(abort_start(hooks, origin));
    }

    // 4. リストア予約の適用 → DB の初期化（失敗はログにも残る）。
    hooks.before_open();
    let opened = match runtime.block_on(locked.open()) {
        Ok(opened) => opened,
        Err(failure) => return Err(report_start_failure(hooks, &failure)),
    };

    #[cfg(debug_assertions)]
    if let Err(reason) = runtime.block_on(super::apply_test_port(&opened)) {
        eprintln!("{SERVER_LABEL}: {reason}");
        runtime.block_on(opened.close());
        return Err(hooks.finish_start_failure(EXIT_START_FAILED));
    }

    // Hub のキーの保存先（排他を取った後）とエンジン。
    let (key_store, key_store_error) = server_host::open_key_store_from(layout, access);
    if let Some(reason) = key_store_error {
        eprintln!(
            "{SERVER_LABEL}: Hub の API キーの保存先（{}）を使えません。Hub 経由の収集は動きません（PLC の収集は続けます）: {reason}",
            layout.hub_keys_dir().display()
        );
    }
    if let Some(origin) = hooks.stop_requested() {
        runtime.block_on(opened.close());
        return Err(abort_start(hooks, origin));
    }
    match runtime.block_on(opened.start(key_store)) {
        Ok(server) => Ok(Started {
            server: Arc::new(server),
            log,
        }),
        Err(failure) => Err(report_start_failure(hooks, &failure)),
    }
}

/// 起動の失敗を出して終了コードを返す（ログを開いていればログにも）。
pub(crate) fn report_start_failure(
    hooks: &impl StartHooks,
    failure: &server_host::StartFailure,
) -> i32 {
    eprintln!("{SERVER_LABEL}: 起動できませんでした: {}", failure.message);
    for line in &failure.guidance {
        eprintln!("{SERVER_LABEL}:   {line}");
    }
    hooks.finish_start_failure(failure.exit_code())
}

/// 起動の途中で止める合図を受けた（#598 監査 F3）: 起動を中止して終了コード 3。
fn abort_start(hooks: &impl StartHooks, origin: &str) -> i32 {
    eprintln!("{SERVER_LABEL}: 起動の途中で停止の合図（{origin}）を受けたため、起動を中止します");
    hooks.finish_start_failure(EXIT_START_FAILED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 差し込みの呼ばれ方を記録する偽物（ログは開かない・向け直さない）。
    #[derive(Default)]
    struct FakeHooks {
        events: Mutex<Vec<String>>,
        /// `stop_requested` が何回目の呼び出しから合図を返すか（`None` は返さない）。
        stop_from_call: Option<usize>,
        stop_calls: Mutex<usize>,
        fail_log: bool,
    }

    impl FakeHooks {
        fn push(&self, event: impl Into<String>) {
            self.events.lock().unwrap().push(event.into());
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }
    }

    impl StartHooks for FakeHooks {
        fn stop_requested(&self) -> Option<&'static str> {
            let mut calls = self.stop_calls.lock().unwrap();
            *calls += 1;
            let stop = self.stop_from_call.is_some_and(|from| *calls >= from);
            self.push(format!("stop?{}", if stop { "yes" } else { "no" }));
            stop.then_some("テスト")
        }

        fn open_log(&self, _layout: &DataLayout) -> Result<OpenedLog, String> {
            self.push("open_log");
            if self.fail_log {
                return Err("テスト用に向け直しの失敗".to_string());
            }
            Ok((None, Box::new(|_| {}), None))
        }

        fn log_opened(&self, _flush: Flush, _record: Option<LineSink>) {
            self.push("log_opened");
        }

        fn finish_start_failure(&self, code: i32) -> i32 {
            self.push(format!("failed {code}"));
            code
        }

        fn before_open(&self) {
            self.push("before_open");
        }
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap()
    }

    /// 一時ルートに DB を作り、待ち受けのポートを 0（OS が選ぶ）にしておく。
    fn prepare_root(runtime: &tokio::runtime::Runtime, layout: &DataLayout) {
        use chronogazer_core::db::{init_db, Db};
        use chronogazer_core::settings::SettingsService;
        std::fs::create_dir_all(layout.root()).unwrap();
        runtime.block_on(async {
            let pool = init_db(layout.db_path()).await.expect("init_db");
            SettingsService::new(Db::Sqlite(pool.clone()))
                .set("server.port", "0")
                .await
                .unwrap();
            pool.close().await;
        });
    }

    fn spec<'a>(layout: &'a DataLayout, access: &'a DirAccessSource) -> StartSpec<'a> {
        StartSpec {
            layout,
            host: ChronoGazerHost::Service,
            access,
        }
    }

    /// 全段を通ると、渡した host の種類で `endpoint.json` を書き、段の間で 2 回だけ
    /// 止める合図を見る（DB の初期化の前・エンジンの開始の前）。ログは DB を開く前に
    /// 開き、失敗は知らせない。
    #[test]
    fn the_whole_sequence_runs_in_order_with_the_given_host_kind() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        let runtime = runtime();
        prepare_root(&runtime, &layout);
        let hooks = FakeHooks::default();
        let started = start_server(
            &runtime,
            spec(&layout, &DirAccessSource::CurrentProcess),
            &hooks,
        )
        .unwrap_or_else(|code| panic!("起動できなかった（{code}）: {:?}", hooks.events()));
        assert_eq!(
            hooks.events(),
            [
                "open_log",
                "log_opened",
                "stop?no",
                "before_open",
                "stop?no"
            ]
        );
        let endpoint =
            chronogazer_core::endpoint::read_endpoint(started.server.endpoint_file()).unwrap();
        assert_eq!(endpoint.host_kind, "service");
        assert!(started.log.is_none());
        let summary = runtime.block_on(started.server.shutdown(Duration::from_secs(15)));
        assert_eq!(summary.exit_code(), 0);
    }

    /// A1 の排他を取れなければ、ログを開かず（ルートの下に何も作らず）終了コード 2。
    #[test]
    fn a_held_lock_fails_with_2_before_opening_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path());
        let _desktop = chronogazer_core::instance_lock::acquire_for_db(
            &layout.db_path(),
            ChronoGazerHost::Desktop,
        )
        .unwrap();
        let hooks = FakeHooks::default();
        let runtime = runtime();
        let result = start_server(
            &runtime,
            spec(&layout, &DirAccessSource::CurrentProcess),
            &hooks,
        );
        assert!(matches!(result, Err(2)));
        assert_eq!(hooks.events(), ["failed 2"]);
        assert!(!layout.db_path().exists());
        assert!(!layout.logs_dir().exists());
    }

    /// ログを向け直せなければ、DB を開かずに終了コード 3。
    #[test]
    fn a_log_failure_stops_before_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("root"));
        let hooks = FakeHooks {
            fail_log: true,
            ..FakeHooks::default()
        };
        let runtime = runtime();
        let result = start_server(
            &runtime,
            spec(&layout, &DirAccessSource::CurrentProcess),
            &hooks,
        );
        assert!(matches!(result, Err(3)));
        assert_eq!(hooks.events(), ["open_log", "failed 3"]);
        assert!(!layout.db_path().exists());
    }

    /// #598 監査 F3: DB の初期化の前に合図が来ていれば、DB を開かずに中止（3）。
    /// エンジンの開始の前に来ていれば、DB は開いたが `endpoint.json` を書かずに中止。
    #[test]
    fn a_stop_between_stages_aborts_the_start() {
        let dir = tempfile::tempdir().unwrap();
        let layout = DataLayout::new(dir.path().join("before-open"));
        let hooks = FakeHooks {
            stop_from_call: Some(1),
            ..FakeHooks::default()
        };
        let runtime = runtime();
        let result = start_server(
            &runtime,
            spec(&layout, &DirAccessSource::CurrentProcess),
            &hooks,
        );
        assert!(matches!(result, Err(3)));
        assert_eq!(
            hooks.events(),
            ["open_log", "log_opened", "stop?yes", "failed 3"]
        );
        assert!(!layout.db_path().exists());

        let layout = DataLayout::new(dir.path().join("before-start"));
        prepare_root(&runtime, &layout);
        let hooks = FakeHooks {
            stop_from_call: Some(2),
            ..FakeHooks::default()
        };
        let result = start_server(
            &runtime,
            spec(&layout, &DirAccessSource::CurrentProcess),
            &hooks,
        );
        assert!(matches!(result, Err(3)));
        assert_eq!(
            hooks.events(),
            [
                "open_log",
                "log_opened",
                "stop?no",
                "before_open",
                "stop?yes",
                "failed 3"
            ]
        );
        assert!(!layout.endpoint_file().exists());
        // 中止の後は A1 の排他を返している（もう一度取れる）。
        assert!(chronogazer_core::instance_lock::acquire_for_db(
            &layout.db_path(),
            ChronoGazerHost::Desktop
        )
        .is_ok());
    }
}
