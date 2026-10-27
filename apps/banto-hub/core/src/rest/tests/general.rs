use super::super::*;
use super::common::{
    admin_delete, admin_post, admin_put, router_with_token, test_env, test_manager_with_clock,
    v1_get, CLIENT_HEADER,
};
use crate::db::migrate_memory;
use axum::body::Body;
use axum::http::Request as HttpRequest;
use banto_tstore::SystemClock;
use tokio::sync::broadcast as tokio_broadcast;
use tower::ServiceExt;

#[test]
fn tag_payload_comment_preserves_omission_and_explicit_clear() {
    let base = json!({
        "name": "motor", "collectionGroupId": 1,
        "address": "40001", "dataType": "i16"
    });
    let omitted: TagInput = serde_json::from_value::<TagPayload>(base.clone())
        .unwrap()
        .into();
    assert_eq!(omitted.comment, None);
    let with_comment: TagInput = serde_json::from_value::<TagPayload>(json!({
        "name": "motor", "collectionGroupId": 1,
        "address": "40001", "dataType": "i16", "comment": "設備Aの説明"
    }))
    .unwrap()
    .into();
    assert_eq!(with_comment.comment.as_deref(), Some("設備Aの説明"));
    let cleared: TagInput = serde_json::from_value::<TagPayload>(json!({
        "name": "motor", "collectionGroupId": 1,
        "address": "40001", "dataType": "i16", "comment": ""
    }))
    .unwrap()
    .into();
    assert_eq!(cleared.comment.as_deref(), Some(""));
}

fn slmp_conn_with_cpu(id: i64, cpu: &str) -> banto_tags::PlcConnection {
    let serde_json::Value::Object(settings) = serde_json::json!({
        "host": "127.0.0.1", "port": 5007, "word_order": "low_high", "cpu_type": cpu,
    }) else {
        unreachable!("a JSON object literal")
    };
    banto_tags::PlcConnection::from_settings(
        id,
        format!("conn-{id}"),
        "slmp".to_string(),
        true,
        false,
        settings,
        1,
    )
}

/// #605 P2 → R2b-3a-2: 接続テストが比べる設定は、いま読む handle 自身が作成時に
/// 持っていた設定の指紋（`settings_fingerprint`。#605 では CPU 種別だった）で
/// なければならない。古い R の handle を取ったあと、読む前に設定の反映
/// （`BrokerSimRegistry::resolve`）が新しい Q を公開する、という交差を同期点で
/// 再現する。最新の設定（resolve が公開した Q）と比べると要求の Q と一致して
/// 通ってしまうが、handle の指紋（R）で比べるので一致しない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn session_settings_check_uses_the_handle_generation_not_the_latest_published_settings() {
    let sessions = crate::broker_glue::HubSessions::new(
        banto_broker::BackoffConfig::default(),
        crate::test_support::test_drivers(),
    );
    let registry = crate::broker_glue::BrokerSimRegistry::new();
    let drivers = sessions.drivers().registry.clone();
    let old = slmp_conn_with_cpu(1, "R");
    let (r_settings, changed) = registry.resolve(&old, &drivers).await;
    let r_settings = r_settings.settings().clone();
    assert!(changed, "初回は changed");
    sessions
        .ensure_connection_with_settings(&old, r_settings.clone())
        .unwrap();
    let r = sessions.dial_fingerprint("slmp", Some(&old), &r_settings);
    assert!(r.is_some());

    // 1. 古い R の handle を取る。
    let handle = sessions.handle_for(1).expect("live session");
    assert_eq!(handle.settings_fingerprint(), r);

    // 2. 読む前に、設定の反映が新しい Q を公開する（古いセッションはまだ生きている）。
    let (q_settings, changed) = registry
        .resolve(&slmp_conn_with_cpu(1, "Q"), &drivers)
        .await;
    let q_settings = q_settings.settings().clone();
    assert!(changed, "Q の公開は resolve で changed になる");
    let q = sessions.dial_fingerprint("slmp", Some(&slmp_conn_with_cpu(1, "Q")), &q_settings);
    assert_ne!(q, r);

    // 3. 要求の Q との比較は、古い handle の指紋（R）に対して行われ、一致しない。
    assert_ne!(handle.settings_fingerprint(), q);
    // 同じ設定の再公開は changed にならない。
    let (_, changed) = registry
        .resolve(&slmp_conn_with_cpu(1, "Q"), &drivers)
        .await;
    assert!(!changed);

    // 作り直したあとの handle は Q の指紋を持つ。
    assert!(sessions.stop_and_join(1).await);
    sessions
        .ensure_connection_with_settings(&slmp_conn_with_cpu(1, "Q"), q_settings)
        .unwrap();
    let fresh = sessions.handle_for(1).expect("rebuilt session");
    assert_eq!(fresh.settings_fingerprint(), q);
    sessions.shutdown().await;
}

fn conn_with_settings(protocol: &str, settings: serde_json::Value) -> banto_tags::PlcConnection {
    let serde_json::Value::Object(settings) = settings else {
        unreachable!("a JSON object literal")
    };
    banto_tags::PlcConnection::from_settings(
        3,
        "c".to_string(),
        protocol.to_string(),
        true,
        false,
        settings,
        1,
    )
}

/// R2b-3a-2（Codex のレビュー P2）: 設定が同じでもドライバの種類（protocol）が
/// 違えば別のセッション。`resolve` は `changed` にし、Hub はセッションを張り直す。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_protocol_with_the_same_settings_changes_the_resolved_dial_identity() {
    let drivers = crate::test_support::test_drivers().registry;
    let registry = crate::broker_glue::BrokerSimRegistry::new();
    let settings = || serde_json::json!({ "host": "10.0.0.1", "port": 502 });
    let (_, changed) = registry
        .resolve(&conn_with_settings("modbus-tcp", settings()), &drivers)
        .await;
    assert!(changed);
    let (_, changed) = registry
        .resolve(&conn_with_settings("modbus-tcp", settings()), &drivers)
        .await;
    assert!(!changed, "同じ protocol・設定は同じ同一性");
    let (_, changed) = registry
        .resolve(&conn_with_settings("slmp", settings()), &drivers)
        .await;
    assert!(changed, "protocol だけ変えたらセッションの張り直し");
}

/// R2b-3a-2（Codex のレビュー）: Modbus の unit_id・word_order を変えると指紋が
/// 変わり、セッションが張り直される。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_changed_modbus_unit_id_or_word_order_changes_the_resolved_dial_identity() {
    let drivers = crate::test_support::test_drivers().registry;
    let registry = crate::broker_glue::BrokerSimRegistry::new();
    let modbus = |unit: i64, order: &str| {
        conn_with_settings(
            "modbus-tcp",
            serde_json::json!({
                "host": "10.0.0.1", "port": 502, "unit_id": unit, "word_order": order,
            }),
        )
    };
    let (_, changed) = registry.resolve(&modbus(1, "low_high"), &drivers).await;
    assert!(changed);
    let (_, changed) = registry.resolve(&modbus(1, "low_high"), &drivers).await;
    assert!(!changed);
    let (_, changed) = registry.resolve(&modbus(2, "low_high"), &drivers).await;
    assert!(changed, "unit_id の変更");
    let (_, changed) = registry.resolve(&modbus(2, "high_low"), &drivers).await;
    assert!(changed, "word_order の変更");
}

/// R2b-6b（docs/banto-hub-driver-ui-design.md §5）: 接続テストの `settings` はフォームの値だけを
/// スキーマに通す（省いた `cpu_type`・`word_order` はスキーマの既定。保存済みからは引き継がない。
/// #790 の「保存済みの CPU を引き継ぐ」は MCP の平らな形のアダプタに移した）。保存済みの接続は
/// `connectionId` が**同じプロトコル**のときだけ返る（指紋と秘密値の補いの出どころ）。
#[tokio::test]
async fn the_test_settings_come_from_the_form_and_the_saved_row_only_from_the_same_protocol() {
    use super::super::plc_connections::{requested_settings, PlcConnectionTestPayload};

    let env = test_env().await;
    let service = banto_tags::PlcConnectionService::new(env.pool.clone());
    let input = |name: &str, protocol: &str, word_order: &str, cpu: Option<&str>| {
        banto_tags::PlcConnectionInput {
            name: name.to_string(),
            protocol: protocol.to_string(),
            host: "127.0.0.1".to_string(),
            port: 5007,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: word_order.to_string(),
            cpu_type: cpu.map(str::to_string),
            database: None,
            username: None,
            secrets: Default::default(),
            settings: None,
        }
    };
    let slmp_q = service
        .create(input("slmp-q", "slmp", "high_low", Some("Q")))
        .await
        .unwrap();
    let modbus = service
        .create(input("modbus", "modbus-tcp", "high_low", None))
        .await
        .unwrap();
    let payload =
        |connection_id: Option<i64>, settings: serde_json::Value| PlcConnectionTestPayload {
            protocol: "slmp".to_string(),
            simulation: false,
            settings: settings.as_object().cloned().unwrap(),
            connection_id,
        };
    let registry = env.manager.registry().clone();
    let slmp = registry.get("slmp").unwrap();
    let driver = slmp.base();

    let requested = requested_settings(
        &env.manager,
        driver,
        &payload(Some(slmp_q.id), json!({"host": "127.0.0.1"})),
    )
    .await
    .unwrap_or_else(|err| panic!("{}", err.message));
    assert_eq!(
        requested.settings.get("cpu_type"),
        Some("R"),
        "スキーマの既定"
    );
    assert_eq!(requested.settings.get("word_order"), Some("low_high"));
    assert_eq!(requested.settings.get("port"), Some("5007"));
    assert_eq!(requested.saved.map(|c| c.id), Some(slmp_q.id));
    assert!(!requested.secrets_changed);

    let requested = requested_settings(
        &env.manager,
        driver,
        &payload(None, json!({"host": "127.0.0.1", "cpu_type": "Q"})),
    )
    .await
    .unwrap_or_else(|err| panic!("{}", err.message));
    assert_eq!(
        requested.settings.get("cpu_type"),
        Some("Q"),
        "フォームの値"
    );
    assert!(requested.saved.is_none());

    let requested = requested_settings(
        &env.manager,
        driver,
        &payload(Some(modbus.id), json!({"host": "127.0.0.1"})),
    )
    .await
    .unwrap_or_else(|err| panic!("{}", err.message));
    assert!(
        requested.saved.is_none(),
        "プロトコルが違う保存済み接続は使わない"
    );

    // スキーマに合わない値はダイヤルの前に断る（未知のキー・型違い）。
    let refused = requested_settings(
        &env.manager,
        driver,
        &payload(
            None,
            json!({"host": "127.0.0.1", "port": "5007", "unit_id": 1}),
        ),
    )
    .await
    .err()
    .expect("refused");
    assert!(refused.message.contains("port"), "{}", refused.message);
    assert!(refused.message.contains("unit_id"), "{}", refused.message);
}

/// R2b-6b（設計 §5・§10）: 接続テストの秘密のキー。編集中（`connectionId`）にフォームが秘密の
/// キーを省けば保存済みの値を開いて補い、文字列ならその値で試す（`secrets_changed`）。`null`・
/// 空文字は値無しで、スキーマで必須ならダイヤルの前に断る。保存前（`connectionId` 無し）で
/// 省いたときも必須の欠け。非秘密の欄はフォームの値（変えた host）。今の Modbus / SLMP には
/// 秘密の欄が無いので、`postgres` の id を名乗る偽のドライバ（`password` を必須の秘密に持つ）で
/// 確かめる（保存済みの行は Hub の `postgres` 接続として封印して作る）。
#[tokio::test]
async fn the_test_supplies_the_saved_secret_only_when_the_form_omits_it() {
    use super::super::plc_connections::{requested_settings, PlcConnectionTestPayload};
    use banto_driver_api::{
        AddressError, Capabilities, DriverAddress, DriverBase, DriverDescriptor, DriverId,
        FieldError as DFE, FieldKind, SettingField, Settings, SettingsSchema, TagKind,
    };

    struct SecretDriver(DriverDescriptor);
    impl DriverBase for SecretDriver {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.0
        }
        fn validate_config(&self, _: &Settings) -> Vec<DFE> {
            Vec::new()
        }
        fn settings_schema(&self) -> SettingsSchema {
            SettingsSchema::new(vec![
                SettingField::new("host", "ホスト", FieldKind::host()).required(),
                SettingField::new("port", "ポート", FieldKind::integer(1, 65_535)).required(),
                SettingField::new("database", "DB", FieldKind::text()),
                SettingField::new("username", "ユーザー", FieldKind::text()),
                SettingField::new("password", "パスワード", FieldKind::Secret).required(),
            ])
        }
        fn secret_keys(&self) -> Vec<String> {
            vec!["password".to_string()]
        }
        fn parse_address(&self, _: &str, _: &TagKind) -> Result<DriverAddress, AddressError> {
            Err(AddressError::new("none"))
        }
        fn offset_address(&self, c: &str, _: &TagKind, _: i64) -> Result<String, AddressError> {
            Ok(c.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
    }
    let driver = SecretDriver(DriverDescriptor::new(
        DriverId::new("postgres").unwrap(),
        "0.0.0",
        "postgres",
        Capabilities::READ_ONLY,
    ));

    let env = test_env().await;
    let saved = env
        .manager
        .plc_connections()
        .create(banto_tags::PlcConnectionInput {
            name: "pg".to_string(),
            protocol: "postgres".to_string(),
            host: "db".to_string(),
            port: 5432,
            unit_id: 1,
            enabled: true,
            simulation: false,
            word_order: String::new(),
            cpu_type: None,
            database: Some("erp".to_string()),
            username: Some("reader".to_string()),
            secrets: crate::secrets::password_changes(
                &crate::secrets::ConnectionSecrets::platform_default(),
                Some("saved-pw"),
            )
            .unwrap(),
            settings: None,
        })
        .await
        .unwrap();
    let payload =
        |connection_id: Option<i64>, settings: serde_json::Value| PlcConnectionTestPayload {
            protocol: "postgres".to_string(),
            simulation: false,
            settings: settings.as_object().cloned().unwrap(),
            connection_id,
        };
    let base = json!({ "host": "db2", "port": 5432, "database": "erp", "username": "reader" });
    let with = |password: serde_json::Value| {
        let mut settings = base.clone();
        settings["password"] = password;
        settings
    };

    // Editing, omitted: the saved value is opened; the changed host is the form's.
    let requested = requested_settings(
        &env.manager,
        &driver,
        &payload(Some(saved.id), base.clone()),
    )
    .await
    .unwrap_or_else(|err| panic!("{}", err.message));
    assert_eq!(requested.settings.get("password"), Some("saved-pw"));
    assert_eq!(requested.settings.get("host"), Some("db2"));
    assert!(!requested.secrets_changed);
    assert_eq!(requested.saved.map(|c| c.id), Some(saved.id));

    // Editing, a new value: that value, and the secret counts as changed.
    let requested = requested_settings(
        &env.manager,
        &driver,
        &payload(Some(saved.id), with(json!("typed-pw"))),
    )
    .await
    .unwrap_or_else(|err| panic!("{}", err.message));
    assert_eq!(requested.settings.get("password"), Some("typed-pw"));
    assert!(requested.secrets_changed);

    // `null` / `""` (no value) and a new connection without one: the required secret is
    // missing - refused before any dial, the message names the key and no value.
    for (connection_id, settings) in [
        (Some(saved.id), with(json!(null))),
        (Some(saved.id), with(json!(""))),
        (None, base.clone()),
    ] {
        let refused = requested_settings(&env.manager, &driver, &payload(connection_id, settings))
            .await
            .err()
            .expect("refused");
        assert!(refused.message.contains("password"), "{}", refused.message);
        assert!(!refused.message.contains("saved-pw"), "{}", refused.message);
    }
}

/// R2b-3a-2（コーディネータのレビュー）: `BrokerSimRegistry::resolve` の指紋は秘密値の
/// **参照**を含む（値は含まない）。秘密値を差し替える（新しい参照で封印し直す）と
/// `changed` になり、呼び出し側（`sync_broker_sessions_from`）がセッションを張り直す。
/// 秘密のキーを持つのは `postgres`（`password`）なので、その接続で確かめる。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_new_secret_reference_changes_the_resolved_dial_identity() {
    let drivers = crate::test_support::test_drivers().registry;
    let registry = crate::broker_glue::BrokerSimRegistry::new();
    let pg = |r#ref: &str| {
        let serde_json::Value::Object(settings) = serde_json::json!({
            "host": "db.local", "port": 5432, "database": "plant", "username": "reader",
            "password": { "$secret": r#ref },
        }) else {
            unreachable!("a JSON object literal")
        };
        banto_tags::PlcConnection::from_settings(
            9,
            "pg".to_string(),
            "postgres".to_string(),
            true,
            false,
            settings,
            1,
        )
    };
    let a = "0123456789abcdef0123456789abcdef";
    let b = "fedcba9876543210fedcba9876543210";
    let (settings_a, changed) = registry.resolve(&pg(a), &drivers).await;
    let settings_a = settings_a.settings().clone();
    assert!(changed);
    assert_eq!(
        settings_a.get("password"),
        None,
        "値も参照も設定には入らない"
    );
    let (_, changed) = registry.resolve(&pg(a), &drivers).await;
    assert!(!changed, "同じ参照は同じ同一性");
    let (settings_b, changed) = registry.resolve(&pg(b), &drivers).await;
    let settings_b = settings_b.settings().clone();
    assert!(changed, "新しい参照はセッションの張り直し");
    assert_eq!(settings_a, settings_b, "違うのは参照だけ");
}

/// `index.html` だけを持つ UI アセット（SPA のフォールバックの確認用）。
struct SpaAssets;

impl banto_server::UiAssets for SpaAssets {
    fn get(path: &str) -> Option<(String, std::borrow::Cow<'static, [u8]>)> {
        (path == "index.html").then(|| {
            (
                "text/html; charset=utf-8".to_string(),
                std::borrow::Cow::Borrowed(b"<html>spa-index</html>".as_slice()),
            )
        })
    }
}

/// 本番と同じ合成（`api_router(..).merge(static_router(..))`）で、存在しない
/// `/api/*` が SPA の `index.html`（200）ではなく JSON の 404
/// （`kind: not_found`）になること（banto v6.3.1 の `static_router`、
/// banto-industrial #547）。既知の API ルートと SPA のルートは従来どおり。
/// 反証: 修正は上流（banto）にあり、banto v6.3.0 以前に戻さないと落とせない。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_api_path_is_json_404_through_the_composed_router() {
    let (api, token, _dir) = router_with_token().await;
    let app = api.merge(banto_server::static_router::<SpaAssets>());

    async fn send(app: &Router, req: HttpRequest<Body>) -> (StatusCode, String, Vec<u8>) {
        let response = app.clone().oneshot(req).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, content_type, bytes.to_vec())
    }

    for (method, uri) in [
        ("GET", "/api/does-not-exist"),
        ("POST", "/api/does-not-exist"),
        ("GET", "/api"),
    ] {
        let req = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header("Authorization", format!("Bearer {token}"))
            .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
            .body(Body::empty())
            .unwrap();
        let (status, content_type, bytes) = send(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} {uri}");
        assert!(content_type.starts_with("application/json"), "{uri}");
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["kind"], json!("not_found"), "{uri}");
    }

    // 既知のルートは従来どおり。
    let req = HttpRequest::get("/api/auth/check")
        .header("Authorization", format!("Bearer {token}"))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .body(Body::empty())
        .unwrap();
    let (status, _, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"true");

    // SPA のルートは index.html。
    let req = HttpRequest::get("/monitor").body(Body::empty()).unwrap();
    let (status, content_type, bytes) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert!(content_type.starts_with("text/html"));
    assert_eq!(bytes, b"<html>spa-index</html>");
}

#[tokio::test]
async fn v1_tags_requires_auth_but_not_the_csrf_header() {
    let (router, token, _dir) = router_with_token().await;

    // No X-Banto-Client header at all - would 403 on the admin surface,
    // must succeed on /api/v1/*.
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // No bearer token at all - must 401.
    let response = router
        .oneshot(
            HttpRequest::get("/api/v1/tags")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_routes_require_the_csrf_header() {
    let (router, token, _dir) = router_with_token().await;
    let response = router
        .oneshot(
            HttpRequest::get("/api/tags")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn v1_values_unknown_tag_is_a_400_listing_every_unresolved_name() {
    let (router, token, _dir) = router_with_token().await;
    let response = router
        .oneshot(
            HttpRequest::get("/api/v1/values?tags=nope.nope.nope,also.missing.one")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["error"], "unknown_tag");
    assert_eq!(json["tags"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn v1_value_single_unknown_tag_is_404() {
    let (router, token, _dir) = router_with_token().await;
    let response = router
        .oneshot(
            HttpRequest::get("/api/v1/values/nope.nope.nope")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tags_create_via_admin_router_rebuilds_the_catalog() {
    let (router, token, _dir) = router_with_token().await;

    let create_conn = HttpRequest::post("/api/plc-connections")
        .header("Authorization", format!("Bearer {token}"))
        .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"name":"line1","protocol":"modbus-tcp","settings":{"host":"127.0.0.1","port":15022}}"#,
        ))
        .unwrap();
    let response = router.clone().oneshot(create_conn).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // The catalog is empty until a tag exists, but the write itself must
    // have gone through the CSRF+auth+RBAC gate cleanly and triggered
    // (an ultimately empty, since nothing is collectible yet) rebuild
    // without panicking or erroring the request.
    let response = router
        .oneshot(
            HttpRequest::get("/api/v1/status")
                .header("Authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["revision"], 1);
    assert!(json["last_config_error"].is_null());
    assert_eq!(json["connections"].as_array().unwrap().len(), 1);
    // T19 S2-a (UX-48): a connection with zero collection groups (this
    // one - it was created with no group at all) reports "unused", not
    // "stopped"/"reconnecting" - it never gets a broker session synced
    // in the first place (`CollectorManager::sync_broker_sessions_from`'s
    // doc comment), so lumping it in with "stopped" would read as
    // broken rather than simply not set up yet.
    assert_eq!(json["connections"][0]["status"], "unused");
}

/// T19 S2-a (UX-48): the flip side of the assertion added to
/// `tags_create_via_admin_router_rebuilds_the_catalog` above - once a
/// connection has an enabled group and tag, its status is no longer
/// "unused" even before any explicit rebuild/run has synced a broker
/// session for it (it falls back to the pre-existing "stopped" rounding
/// documented on `compute_status`, not the new "unused" bucket).
#[tokio::test]
async fn a_connection_with_a_tag_is_never_reported_unused() {
    let (router, token, _dir) = router_with_token().await;

    let (status, conn) = admin_post(
        &router,
        "/api/plc-connections",
        &token,
        json!({ "protocol": "modbus-tcp", "name": "line1", "settings": { "host": "127.0.0.1", "port": 15023 } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{conn:?}");
    let (status, group) = admin_post(
        &router,
        "/api/collection-groups",
        &token,
        json!({ "name": "g1", "plcConnectionId": conn["id"], "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{group:?}");
    let (status, tag) = admin_post(
        &router,
        "/api/tags",
        &token,
        json!({
            "name": "t1",
            "collectionGroupId": group["id"],
            "address": "40001",
            "dataType": "i16",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{tag:?}");

    let (status, status_body) = v1_get(&router, &token, "/api/v1/status").await;
    assert_eq!(status, StatusCode::OK, "{status_body:?}");
    assert_eq!(status_body["connections"].as_array().unwrap().len(), 1);
    assert_ne!(status_body["connections"][0]["status"], "unused");
}

/// 2026-10-19 オーナー決定（migration 0022）: グループ名は接続内で一意。
/// REST で別の接続の下に同じ名前のグループを作れ、同じ接続の下では
/// 作れない。別の接続へ移す更新が移り先の同名と重なれば `name` の項目
/// エラー（422）になり、行は動かない。
#[tokio::test]
async fn collection_group_names_are_unique_per_connection_over_rest() {
    let env = test_env().await;
    let (router, token) = (&env.router, env.admin_token.as_str());
    let mut conn_ids = Vec::new();
    for (name, port) in [("line-per-conn-a", 15101), ("line-per-conn-b", 15102)] {
        let (status, conn) = admin_post(
            router,
            "/api/plc-connections",
            token,
            json!({ "protocol": "modbus-tcp", "name": name, "settings": { "host": "127.0.0.1", "port": port } }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{conn:?}");
        conn_ids.push(conn["id"].as_i64().unwrap());
    }
    let (a, b) = (conn_ids[0], conn_ids[1]);

    let (status, ga) = admin_post(
        router,
        "/api/collection-groups",
        token,
        json!({ "name": "fast", "plcConnectionId": a, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ga:?}");
    let (status, gb) = admin_post(
        router,
        "/api/collection-groups",
        token,
        json!({ "name": "fast", "plcConnectionId": b, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "same name under another connection: {gb:?}"
    );

    let (status, body) = admin_post(
        router,
        "/api/collection-groups",
        token,
        json!({ "name": "fast", "plcConnectionId": a, "periodMs": 1000 }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["field_errors"][0]["field"], "name", "{body:?}");
    assert_eq!(
        body["field_errors"][0]["message"], "この接続内では既に使用されています",
        "{body:?}"
    );

    let gb_id = gb["id"].as_i64().unwrap();
    let (status, body) = admin_put(
        router,
        &format!("/api/collection-groups/{gb_id}"),
        token,
        json!({
            "name": "fast",
            "plcConnectionId": a,
            "periodMs": 1000,
            "enabled": true,
            "defaultWritable": true,
            "querySql": null,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
    assert_eq!(body["field_errors"][0]["field"], "name", "{body:?}");
    let still = CollectionGroupService::new(env.pool.clone())
        .get(gb_id)
        .await
        .unwrap();
    assert_eq!(
        still.plc_connection_id, b,
        "a refused move leaves the row in place"
    );
}

/// #635（2026-10-17 オーナー決定 案 A）: 接続・グループ・タグの作成と改名で、
/// ドットを含む名前をつないだ外部名が既存のタグと重なれば 422 で断り、
/// どの既存のタグと重なるかを文言で示す。断った書き込みは残らない。
#[tokio::test]
async fn registry_writes_reject_a_new_external_name_collision() {
    let env = test_env().await;
    let (router, token) = (&env.router, env.admin_token.as_str());

    async fn post_ok(
        router: &Router,
        token: &str,
        path: &str,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let (status, created) = admin_post(router, path, token, body).await;
        assert_eq!(status, StatusCode::OK, "{path}: {created:?}");
        created
    }
    fn assert_collision(status: StatusCode, body: &serde_json::Value, existing: &str) {
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body:?}");
        assert_eq!(body["field_errors"][0]["field"], "name", "{body:?}");
        let message = body["field_errors"][0]["message"].as_str().unwrap_or("");
        assert!(
            message.contains("既存のタグ") && message.contains(existing),
            "{message}"
        );
    }

    // 既存: 接続「a.b」/ グループ「c」/ タグ「d」= 外部名 a.b.c.d
    let ab = post_ok(
        router,
        token,
        "/api/plc-connections",
        json!({ "protocol": "modbus-tcp", "name": "a.b", "settings": { "host": "127.0.0.1", "port": 15081 } }),
    )
    .await;
    let c = post_ok(
        router,
        token,
        "/api/collection-groups",
        json!({ "name": "c", "plcConnectionId": ab["id"], "periodMs": 1000 }),
    )
    .await;
    let tag = |name: &str, group: &serde_json::Value| {
        json!({
            "name": name,
            "collectionGroupId": group["id"],
            "address": "40001",
            "dataType": "i16",
        })
    };
    post_ok(router, token, "/api/tags", tag("d", &c)).await;

    // 作成: 接続「a」/ グループ「b.c」/ タグ「d」も a.b.c.d になる。
    let a = post_ok(
        router,
        token,
        "/api/plc-connections",
        json!({ "protocol": "modbus-tcp", "name": "a", "settings": { "host": "127.0.0.1", "port": 15082 } }),
    )
    .await;
    let bc = post_ok(
        router,
        token,
        "/api/collection-groups",
        json!({ "name": "b.c", "plcConnectionId": a["id"], "periodMs": 1000 }),
    )
    .await;
    let (status, body) = admin_post(router, "/api/tags", token, tag("d", &bc)).await;
    assert_collision(status, &body, "接続「a.b」/ グループ「c」/ タグ「d」");
    assert!(body["field_errors"][0]["message"]
        .as_str()
        .is_some_and(|message| message.contains("a.b.c.d")));

    // タグの改名: 「e」で作ってから「d」へ変える。
    let e = post_ok(router, token, "/api/tags", tag("e", &bc)).await;
    let (status, body) = admin_put(
        router,
        &format!("/api/tags/{}", e["id"]),
        token,
        tag("d", &bc),
    )
    .await;
    assert_collision(status, &body, "接続「a.b」/ グループ「c」/ タグ「d」");

    // グループの改名: a.b / zz / y を作り、グループを「c2.x」へ変えると、
    // 既存の a.b / c2 / x.y（= a.b.c2.x.y）と重なる。
    let c2 = post_ok(
        router,
        token,
        "/api/collection-groups",
        json!({ "name": "c2", "plcConnectionId": ab["id"], "periodMs": 1000 }),
    )
    .await;
    post_ok(router, token, "/api/tags", tag("x.y", &c2)).await;
    let zz = post_ok(
        router,
        token,
        "/api/collection-groups",
        json!({ "name": "zz", "plcConnectionId": ab["id"], "periodMs": 1000 }),
    )
    .await;
    post_ok(router, token, "/api/tags", tag("y", &zz)).await;
    let (status, body) = admin_put(
        router,
        &format!("/api/collection-groups/{}", zz["id"]),
        token,
        json!({ "name": "c2.x", "plcConnectionId": ab["id"], "periodMs": 1000 }),
    )
    .await;
    assert_collision(status, &body, "接続「a.b」/ グループ「c2」/ タグ「x.y」");

    // 接続の改名: 接続「a」は使用中の名前なので先に消す。m / b.c2 / d を作り、
    // m を「a」へ変えると a.b.c2.d になり、既存の a.b / c2 / d と重なる。
    post_ok(router, token, "/api/tags", tag("d", &c2)).await;
    let (status, body) =
        admin_delete(router, &format!("/api/plc-connections/{}", a["id"]), token).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body:?}");
    let m = post_ok(
        router,
        token,
        "/api/plc-connections",
        json!({ "protocol": "modbus-tcp", "name": "m", "settings": { "host": "127.0.0.1", "port": 15083 } }),
    )
    .await;
    let bc2 = post_ok(
        router,
        token,
        "/api/collection-groups",
        json!({ "name": "b.c2", "plcConnectionId": m["id"], "periodMs": 1000 }),
    )
    .await;
    post_ok(router, token, "/api/tags", tag("d", &bc2)).await;
    let (status, body) = admin_put(
        router,
        &format!("/api/plc-connections/{}", m["id"]),
        token,
        json!({ "protocol": "modbus-tcp", "name": "a", "settings": { "host": "127.0.0.1", "port": 15083 } }),
    )
    .await;
    assert_collision(status, &body, "接続「a.b」/ グループ「c2」/ タグ「d」");

    // 断った書き込みは残らず、catalog に重なりは無い。
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM plc_connections")
        .fetch_all(&env.pool)
        .await
        .unwrap();
    assert!(names.contains(&"m".to_owned()) && !names.contains(&"a".to_owned()));
    assert!(env.manager.tag_map().collisions().is_empty());
}

/// A plain `modbus-tcp` connection input at `127.0.0.1:port` for the services without a
/// registry (R2b-6b: the REST payload no longer has the flat fields).
fn modbus_flat_input(name: &str, port: i64) -> banto_tags::PlcConnectionInput {
    banto_tags::PlcConnectionInput {
        name: name.to_string(),
        protocol: "modbus-tcp".to_string(),
        host: "127.0.0.1".to_string(),
        port,
        unit_id: 1,
        enabled: true,
        simulation: false,
        word_order: String::new(),
        cpu_type: None,
        database: None,
        username: None,
        secrets: Default::default(),
        settings: None,
    }
}

/// T19 S2-a 案B (UX-48, docs/banto-hub-t19-design.md §3.8, 2026-09-03)
/// で追加、#341 (2026-09-14) で live re-apply 経路の回帰テストとして
/// 引き継いだ: a manager/controller pair without the router/RBAC/session
/// layer around it, so the tests below can call
/// `commit_catalog_and_notify` (this file's own commit entry point - the
/// ONE function every registry-mutation handler and
/// `execute_pending_apply` funnel through, this fn's own doc comment)
/// directly while the controller is `Running`, without going through a
/// router whose `registry_change_should_queue` policy (試運転か
/// ロックダウン済みか) would decide for them. What these tests pin down
/// is the layer underneath that policy:
/// `CollectionController::commit_catalog_and_apply_live` must sync
/// broker sessions for a change committed while `Running` (案B の要件)
/// and must NOT dial anything while `Stopped` (T15-4 の要件) - both
/// regardless of which REST-level policy sent the change here.
async fn catalog_resync_test_env() -> (
    Arc<CollectorManager>,
    Arc<CollectionController>,
    broadcast::Sender<ServerEvent>,
    sqlx::SqlitePool,
    tempfile::TempDir,
) {
    let pool = migrate_memory().await.expect("migrate_memory");
    let (manager, dir) = test_manager_with_clock(pool.clone(), Arc::new(SystemClock));
    let controller = Arc::new(CollectionController::new(manager.clone()));
    let (events, _rx) = tokio_broadcast::channel(16);
    (manager, controller, events, pool, dir)
}

/// T19 S2-a 案B, "most important" per the implementation instructions:
/// a catalog change committed while collection is `Stopped` must NOT
/// dial a broker session - `CollectionController::
/// commit_catalog_and_apply_live` only touches the collector/broker
/// sessions while `Running` (its own doc comment). This is the direct evidence that 案B does not
/// reopen the T15-4 hazard (`crate::write_path`'s module doc comment,
/// "gate 8 は broker セッションを新規に張らない") of dialing a PLC the
/// operator meant to leave stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_catalog_and_notify_does_not_dial_a_session_while_collection_is_stopped() {
    let (manager, controller, events, pool, _dir) = catalog_resync_test_env().await;
    assert_eq!(controller.status().state, CollectionState::Stopped);

    let conn = PlcConnectionService::new(pool.clone())
        .create(modbus_flat_input("line-resync-stopped", 15041))
        .await
        .unwrap();
    let group = CollectionGroupService::new(pool.clone())
        .create(
            serde_json::from_value::<CollectionGroupPayload>(json!({
                "name": "g1",
                "plcConnectionId": conn.id,
                "periodMs": 1000,
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(
            serde_json::from_value::<TagPayload>(json!({
                "name": "t1",
                "collectionGroupId": group.id,
                "address": "40001",
                "dataType": "i16",
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();

    commit_catalog_and_notify(&controller, &events, "tags")
        .await
        .expect("live apply");

    assert!(
        !manager.sessions().connection_ids().contains(&conn.id),
        "a catalog change committed while stopped must not dial a broker session"
    );
    assert!(
        manager.write_broker_handle_peek(conn.id).is_none(),
        "no session exists, so the write path must still fail closed"
    );
}

/// T19 S2-a 案B: the "add" case - a tag (and its group) registered under
/// a previously-tagless connection while collection is already
/// `Running` gets a broker session synced immediately on that catalog
/// commit, not only at the next `start`/`stop` cycle.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_catalog_and_notify_syncs_a_session_for_a_newly_tagged_connection_while_running() {
    let (manager, controller, events, pool, _dir) = catalog_resync_test_env().await;

    let conn = PlcConnectionService::new(pool.clone())
        .create(modbus_flat_input("line-resync-add", 15042))
        .await
        .unwrap();

    let started = controller
        .start(RunMode::Configured)
        .await
        .expect("収集を開始できる");
    assert_eq!(started.state, CollectionState::Running);
    // Zero collection groups yet (T19 S2-a) - genuinely tagless, so
    // `apply_run` (inside `start`) did not sync a broker session for it.
    assert!(!manager.sessions().connection_ids().contains(&conn.id));

    let group = CollectionGroupService::new(pool.clone())
        .create(
            serde_json::from_value::<CollectionGroupPayload>(json!({
                "name": "g1",
                "plcConnectionId": conn.id,
                "periodMs": 1000,
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(
            serde_json::from_value::<TagPayload>(json!({
                "name": "t1",
                "collectionGroupId": group.id,
                "address": "40001",
                "dataType": "i16",
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();

    commit_catalog_and_notify(&controller, &events, "tags")
        .await
        .expect("live apply");

    assert!(
        manager.sessions().connection_ids().contains(&conn.id),
        "adding a tag to a tagless connection while running must sync a broker session \
         immediately (T19 S2-a 案B), not only at the next start/stop cycle"
    );
    assert!(
        manager.write_broker_handle_peek(conn.id).is_some(),
        "the write path must find the session peek-able right after the catalog commit"
    );

    controller.stop().await;
}

/// T19 S2-a 案B: the "remove" flip side - once a connection's last
/// enabled collection group is disabled while collection is `Running`,
/// its broker session is torn down on that same catalog commit, mirroring
/// `CollectorManager::rebuild`'s own add+remove sync (T7-2).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commit_catalog_and_notify_removes_a_session_once_the_last_group_is_disabled_while_running()
{
    let (manager, controller, events, pool, _dir) = catalog_resync_test_env().await;

    let conn = PlcConnectionService::new(pool.clone())
        .create(modbus_flat_input("line-resync-remove", 15043))
        .await
        .unwrap();
    let groups = CollectionGroupService::new(pool.clone());
    let group = groups
        .create(
            serde_json::from_value::<CollectionGroupPayload>(json!({
                "name": "g1",
                "plcConnectionId": conn.id,
                "periodMs": 1000,
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();
    TagService::new(pool.clone())
        .create(
            serde_json::from_value::<TagPayload>(json!({
                "name": "t1",
                "collectionGroupId": group.id,
                "address": "40001",
                "dataType": "i16",
            }))
            .unwrap()
            .into(),
        )
        .await
        .unwrap();

    let started = controller
        .start(RunMode::Configured)
        .await
        .expect("収集を開始できる");
    assert_eq!(started.state, CollectionState::Running);
    assert!(
        manager.sessions().connection_ids().contains(&conn.id),
        "a connection with a tag must get a broker session synced on start, as before T19 S2-a"
    );

    groups
        .update(
            group.id,
            CollectionGroupInput {
                name: group.name.clone(),
                plc_connection_id: group.plc_connection_id,
                period_ms: group.period_ms,
                enabled: false,
                default_writable: group.default_writable,
                // 外部 DB 連携 S2: 既存行の SQL 文をそのまま持ち回る
                // （この経路はグループを無効化するだけで、SQL 文を
                // 書き換える意図は無い）。
                query_sql: group.query_sql.clone(),
            },
        )
        .await
        .unwrap();

    commit_catalog_and_notify(&controller, &events, "collection_groups")
        .await
        .expect("live apply");

    assert!(
        !manager.sessions().connection_ids().contains(&conn.id),
        "disabling a connection's last enabled group while running must remove its broker \
         session on that same catalog commit (T19 S2-a 案B)"
    );
    assert!(
        manager.write_broker_handle_peek(conn.id).is_none(),
        "no session remains, so the write path must fail closed"
    );

    controller.stop().await;
}
