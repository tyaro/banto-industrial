//! chronogazer's domain/service layer (spec §10). Kept tauri-free so it
//! is testable without the src-tauri crate (which cannot be built in every
//! environment, e.g. CI containers without webkit2gtk). Thin
//! `tauri::command` adapters in `src-tauri` call into this crate; the same
//! services back the embedded REST server in M6.

pub mod assets;
// #437: 監査の保留（banto ADR-0019）の配線。`banto-serve` と Tauri の両方が通る。
pub mod audit_spool;
// #383 段階2b / R1-C（C-1）: 収集ランタイムのサービス層。`tauri` にも `axum`
// にも依存しない（この crate の規律 - 上の doc 参照）ので、`src-tauri` と
// `banto-serve` の両方から同じ形で使える。
pub mod collect;
pub mod db;
// #393（#524 の段階 1）: 表示グループのデータモデル・検証・サービス。REST と
// Tauri の両方が同じ検証関数・同じサービスを使う（モジュール doc 参照）。
pub mod display_groups;
// #392 段階 B（B1-1）: 起動と終了の組み立て（`EngineRuntime`）。`banto-serve` が
// 使い、B1-2 でデスクトップ（`src-tauri`）も乗り換える - モジュール doc 参照。
pub mod endpoint;
pub mod engine;
pub mod events;
// #332: Hub 接続（`banto-hub-bootstrap` の配線）。keyring は src-tauri、
// `banto-serve` は `hub::UnavailableKeyStore` を渡す - モジュール doc 参照。
pub mod hub;
// #392 B1-4: `GET /api/host`（未認証。シェルの probe が別製品・別 DB を見分ける）。
pub mod host_info;
// #383 段階3 P4: 「Hub から追加」（Hub のカタログからタグを登録する）。REST と
// Tauri の両方が同じ関数を呼ぶ - モジュール doc 参照。
pub mod hub_tags;
// #392 A1: 同じ DB を 2 プロセスで開かないための単一インスタンス排他。
// デスクトップと `banto-serve` が DB を開く前に呼ぶ。
pub mod instance_lock;
// #392 B2-4a: Hub の API キーの保存先（DPAPI のファイル、`<root>\secrets\hub-keys\`）と
// 開けなかったときの代わり。B1-5 で `server_host` に置いたものを、デスクトップ host
// （B2-4b）と共有するため移した（`server_host` は再公開）。
pub mod key_store;
// #392 B2-1: この PC からの接続を通す grant `localDesktop`（peer と Host の両方が
// loopback、未初期化なら admin、初期化後は設定 `auth.local_grant`）。
pub mod local_grant;
// #623 PR-4: 設定モード ⇔ 運用モードの切り替えの SCM の扱いと待ち方・デスクトップのログ・引き継ぎ。
pub mod mode_switch;
// #392 B1-4: データのルート（`CHRONOGAZER_ROOT`、既定 `%ProgramData%\ChronoGazer`）と
// その下の配置、旧データの場所。B1-5 の `chronogazer-server` が使う（デスクトップは
// B2-4 で寄せる）。
pub mod paths;
pub mod rest;
// #392 B2-1: LAN の公開設定と認証の設定（デスクトップの Tauri のコマンドと REST の
// `/api/settings/*` が同じ適用の関数を使う）。
pub mod server_admin;
// #392 B1-5: 画面の無い本番用の host `chronogazer-server` の、OS に依存しない部分
// （待ち受けの形・ルートの検証・起動の失敗の案内・KeyStore・日付ごとのログ）。
pub mod server_host;
// #392 B3-2b: `chronogazer-server` の Windows サービスの形の、OS に依存しない判断と
// 文言（終了コード・停止の予算・インストール利用者の SID・install/uninstall の文言）。
// SCM とのやりとりは `banto-service-control`、本体と登録の手順は bin。
pub mod server_service;
mod service_data_dir;
// #392 B2-4a: シェルがサービスの health を確かめる probe（`run\endpoint.json` →
// `GET /api/host`、O8 の条件）。std の TCP だけで、Tauri に依存しない。
pub mod service_probe;
// #538: 保持期間（`retention.days`）を過ぎた時系列データファイルの削除
// （起動時 + 日付が変わるたび）。`tauri`/`axum` に依存しない future を返す。
pub mod retention;
// #393 / #525: 楽観ロックの食い違いの表し方（タグ・表示グループ共通）。
pub mod revision;
pub mod settings;
// #392 B2-4a: シェルの起動判定（サービスへつなぐ・プロセスの中で動かす・fallback）の
// 純関数。Tauri にも SCM にも触れない（観測は呼び出し側）- モジュール doc 参照。
pub mod shell_launch;
// #392 B1-5 → 2026-10-24 オーナー決定「案 A」: 標準出力・標準エラー出力の日次ログへの
// 向け直し（Windows だけ）。`chronogazer-server` の bin から移し、デスクトップも使う。
#[cfg(windows)]
pub mod stdio_log;
// #392（2026-10-12 オーナー決定）: 起動に失敗したときの「エラーの窓と対処の
// 選択」の判断（純関数）と対処（作り直す・バックアップから戻す・保存先を
// 選び直す）。窓は各シェル（今はデスクトップの `src-tauri`）が出す。
pub mod startup_recovery;
// REST と Tauri で共通のタグ更新の楽観ロック応答。保存時のアドレス検査は
// 登録簿を持つ banto-tags のサービスがトランザクション内で行う。
pub mod tag_address;
// #532: 記録計の側のタグごとのしきい値（タグ定義の外の設定）。検証・保存は
// REST と Tauri の両方が同じサービスを使い、収集の開始がここから判定値を読む。
pub mod tag_thresholds;
#[cfg(test)]
pub(crate) mod test_support;

/// The URLs a client can use to reach a server that is ACTUALLY listening on
/// `addr` (its `RunningServer::local_addr()`) - for the startup log's
/// "listening at" list (`banto-serve`) and
/// the desktop settings screen's URL/QR list while the embedded server runs. banto v2.0.0 移行（PR1a オーナーレビュー P3）: the
/// bind setting is a free string handed to `TcpListener::bind`, so a host
/// name such as `localhost` binds fine but `lan_urls_for_bind` cannot parse
/// it as an IP and would list nothing. The bound address is always an IP;
/// an unspecified `0.0.0.0` bind stays `0.0.0.0` here, so every interface is
/// still listed. (An IPv6 bound address lists nothing, by banto's
/// `lan_urls_for_bind` policy - IPv6 is out of scope for now.)
pub fn listening_urls(addr: std::net::SocketAddr) -> Vec<String> {
    banto_server::lan_urls_for_bind(&addr.ip().to_string(), addr.port())
}

// I2a（2026-10-04）: 領域に依らないサービス（アカウント・監査ログ・
// バックアップ）は自前のコピーをやめ、banto の `banto-admin-services` の
// ものをそのまま使う（独自実装は banto に寄せる、2026-10-01 オーナー方針。
// admin-template の `lib.rs` と同じ re-export）。`crate::{audit,backup,users}::*`
// のパスは変わらずに解決する。`settings` だけは ChronoGazer 固有の設定キー
// （`data.dir`・`retention.days`）の型付きラッパを足すため自前のモジュールに
// して、その中で banto の `settings` を re-export している。
// 自前のコピーに入っていなかった banto v2.1.0 のセキュリティ修正 - 初回
// セットアップの原子化（#277）、未認証ログアウト・失敗ログインの監査の増幅
// （#278）、バックアップ保存先の DB ごとの分離（#280）- はこれで入る。
pub use banto_admin_services::{audit, backup, users};

// #383 段階2a / R1-B（レジストリ CRUD）: banto-tags の3サービスと行型を
// re-export し、`src-tauri`（invariant: このアプリ自身の新規依存を持たない -
// `db::DbPool` 相当の precedent と同じ理由）が `AppState` のフィールド・
// コマンドシグネチャに名前を出せるようにする。relay-wright の `lib.rs`
// （R1-B 同名 re-export）と同じ作法。camelCase の create/update wire
// payload は `rest` 側（`rest::PlcConnectionPayload` 等）が持つ -
// banto-tags 自身の `*Input` は snake_case で deserialize し、ワイヤを
// 一切またがない。
pub use banto_tags::driver_catalog;
pub use banto_tags::{
    CollectionGroup, CollectionGroupService, PlcConnection, PlcConnectionService, Tag, TagService,
};

#[cfg(test)]
mod listening_urls_tests {
    use super::listening_urls;

    /// PR1a オーナーレビュー P3: see [`listening_urls`].
    #[test]
    fn listening_urls_come_from_the_bound_address() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        assert_eq!(
            listening_urls(addr),
            vec![format!("http://127.0.0.1:{}", addr.port())]
        );

        let listener = std::net::TcpListener::bind("0.0.0.0:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        assert!(
            listening_urls(addr).contains(&format!("http://127.0.0.1:{}", addr.port())),
            "{:?}",
            listening_urls(addr)
        );
    }
}
