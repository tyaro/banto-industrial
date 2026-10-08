//! chronogazer's domain/service layer (spec §10). Kept tauri-free so it
//! is testable without the src-tauri crate (which cannot be built in every
//! environment, e.g. CI containers without webkit2gtk). Thin
//! `tauri::command` adapters in `src-tauri` call into this crate; the same
//! services back the embedded REST server in M6.

pub mod assets;
// #383 段階2b / R1-C（C-1）: 収集ランタイムのサービス層。`tauri` にも `axum`
// にも依存しない（この crate の規律 - 上の doc 参照）ので、`src-tauri` と
// `banto-serve` の両方から同じ形で使える。
pub mod collect;
pub mod db;
// #393（#524 の段階 1）: 表示グループのデータモデル・検証・サービス。REST と
// Tauri の両方が同じ検証関数・同じサービスを使う（モジュール doc 参照）。
pub mod display_groups;
pub mod events;
// #332: Hub 接続（`banto-hub-bootstrap` の配線）。keyring は src-tauri、
// `banto-serve` は `hub::UnavailableKeyStore` を渡す - モジュール doc 参照。
pub mod hub;
pub mod rest;
// #393 / #525: 楽観ロックの食い違いの表し方（タグ・表示グループ共通）。
pub mod revision;
pub mod settings;
// #413: 接続単位シミュレーションで値が動かないタグの判定（判定そのものは
// `banto_collect::simulation::classify_plc_tag`）。REST と Tauri の両方が使う。
pub mod simulation;
// #414 段階1: タグのアドレスが接続のプロトコルで読めるかを保存時に確かめる
// （判定は banto-collect の `check_tag_address`。REST と Tauri の両方から呼ぶ）。
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
