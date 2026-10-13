//! `GET /api/host`: 「このポートで動いているのは誰か」を未認証で答える
//! （#392 段階 B の B1-4、設計書 §3.1(b)）。
//!
//! シェル（B2-4）の health probe が、同じ PC の別製品・別 DB のエンジン・port の
//! 競合を見分けるためのもの。`run\endpoint.json`（[`crate::endpoint`]）で見つけた
//! port に打ち、`product == "chronogazer"` かつ `scope` が自分の期待する DB のもの
//! であることを確かめる。
//!
//! ```json
//! {"product":"chronogazer","hostKind":"service","version":"0.1.0","scope":"0123456789abcdef"}
//! ```
//!
//! # 出すもの・出さないもの
//!
//! 未認証なので**機微な情報は出さない**: DB のパス・データのフォルダ・利用者名・
//! 設定値は含めない。`scope` は DB の正規化パスの SHA-256 の先頭 8 バイト
//! （[`crate::instance_lock::InstanceGuard::scope`]）で、パスは逆算できない
//! 不透明な ID（A1 の mutex 名の末尾と同じ値）。`version` と `hostKind` は
//! `/api/auth/status` が既に出している程度の情報より小さい。
//!
//! # 他の未認証の経路と同じ作法
//!
//! `GET /api/auth/status` と同じく、`X-Banto-Client: banto` ヘッダ（CSRF 対策。
//! `banto_server::require_banto_client_header`）を要求する - 無ければ 403。閲覧公開
//! （`server.viewer_public`）とは無関係で、常に答える。ルーターは
//! [`crate::engine::EngineServices::router`] が `api_router` に merge し、
//! セキュリティヘッダ（最外の `with_security_headers`）と CORS は他の `/api` と
//! 同じ層に載る。banto-hub は OpenAPI の `info.x-banto-hub-profile-id` に同種の情報を
//! 載せているが、ChronoGazer に OpenAPI の文書は無く、probe が 1 回の GET で
//! 済むよう専用の経路にした。

use axum::extract::State;
use axum::routing::get;
use axum::{middleware, Json, Router};
use banto_server::require_banto_client_header;
use serde::Serialize;

use crate::instance_lock::ChronoGazerHost;

/// `product` の値。別製品の probe 応答と取り違えないための固定値。
pub const PRODUCT: &str = "chronogazer";

/// `GET /api/host` の応答。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    pub product: &'static str,
    /// [`ChronoGazerHost::as_str`]（`desktop` / `serve` / `server` / `service`）。
    pub host_kind: &'static str,
    pub version: &'static str,
    /// DB のパスのハッシュ（不透明な ID。パスは含まない）。
    pub scope: String,
}

impl HostInfo {
    pub fn new(host: ChronoGazerHost, scope: impl Into<String>) -> Self {
        Self {
            product: PRODUCT,
            host_kind: host.as_str(),
            version: env!("CARGO_PKG_VERSION"),
            scope: scope.into(),
        }
    }
}

async fn host_handler(State(info): State<HostInfo>) -> Json<HostInfo> {
    Json(info)
}

/// `GET /api/host`（認証なし、`X-Banto-Client` ヘッダは要る）。
pub fn host_router(info: HostInfo) -> Router {
    Router::new()
        .route("/api/host", get(host_handler))
        .layer(middleware::from_fn(require_banto_client_header))
        .with_state(info)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn call(router: Router, request: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn get_host(with_header: bool) -> Request<Body> {
        let mut builder = Request::get("/api/host");
        if with_header {
            builder = builder.header("X-Banto-Client", "banto");
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn answers_without_any_token() {
        let router = host_router(HostInfo::new(ChronoGazerHost::Service, "0123456789abcdef"));
        let (status, json) = call(router, get_host(true)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(json["product"], "chronogazer");
        assert_eq!(json["hostKind"], "service");
        assert_eq!(json["scope"], "0123456789abcdef");
        assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
        // 契約: この 4 つだけ。増やすときは機微でないことを確かめてここを直す。
        let mut keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["hostKind", "product", "scope", "version"]);
    }

    #[tokio::test]
    async fn requires_the_csrf_client_header_like_the_other_api_routes() {
        let router = host_router(HostInfo::new(ChronoGazerHost::Server, "s"));
        let (status, _) = call(router, get_host(false)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn every_host_kind_is_reported_by_its_wire_name() {
        for (host, name) in [
            (ChronoGazerHost::Desktop, "desktop"),
            (ChronoGazerHost::Serve, "serve"),
            (ChronoGazerHost::Server, "server"),
            (ChronoGazerHost::Service, "service"),
        ] {
            assert_eq!(HostInfo::new(host, "s").host_kind, name);
        }
    }

    /// 機微な情報（DB のパス）が応答に混じらない: scope は渡された ID だけで、
    /// 本物の排他から作ると 16 桁の hex でパスの断片を含まない。
    #[tokio::test]
    async fn scope_from_a_real_lock_is_an_opaque_id_not_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("secret-customer-name.sqlite3");
        let guard = crate::instance_lock::acquire_for_db(&db, ChronoGazerHost::Server).unwrap();
        let router = host_router(HostInfo::new(ChronoGazerHost::Server, guard.scope()));
        let (status, json) = call(router, get_host(true)).await;
        assert_eq!(status, StatusCode::OK);
        let text = json.to_string();
        assert!(!text.contains("secret-customer-name"), "{text}");
        assert!(!text.contains(dir.path().to_str().unwrap()), "{text}");
        let scope = json["scope"].as_str().unwrap();
        assert_eq!(scope.len(), 16);
        assert!(scope.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
