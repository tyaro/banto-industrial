use super::super::*;
use super::common::{test_env, CLIENT_HEADER};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

// --- T19 S1-b0: GET /api/tags/address-writable (PR #232 レビュー指摘) --
//
// `tags_address_writable`/`AddressWritableResponse`の doc comment が
// 定義する3値の意味論をここで固定する:
//   - `writable: Some(true)`, `area: None`  … 領域制限が「存在しない」
//     （`slmp`/`virtual`）
//   - `writable: Some(bool)`, `area: Some(_)` … `modbus-tcp`でパース成功
//   - `writable: None`, `area: None`         … 判定不能
// 当初案はこれを1つの`null`に統合していたため、"SLMP の既定が永久に
// 効かない"（1と5の混同）と"入力途中の Modbus アドレスで誤って既定
// ON"（3/4と5の混同）の両方の実害を招くところだった - 以下のテストは
// その3値それぞれを区別する。

/// `GET /api/tags/address-writable`をクエリ文字列で叩くヘルパー。
async fn address_writable(
    router: &Router,
    token: &str,
    protocol: &str,
    address: &str,
) -> (StatusCode, serde_json::Value) {
    // テストで使う値はすべて英数字・ハイフン・ドットのみ（`modbus-tcp`・
    // `40001.5`・空文字・`abc`等）で、クエリ文字列としてパーセント
    // エンコード無しでも安全にそのまま埋め込める - 依存を増やしてまで
    // エンコーダを導入する必要はない。
    let uri = format!("/api/tags/address-writable?protocol={protocol}&address={address}");
    let response = router
        .clone()
        .oneshot(
            HttpRequest::get(uri)
                .header("Authorization", format!("Bearer {token}"))
                .header(CLIENT_HEADER.0, CLIENT_HEADER.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// (1) `slmp`は任意のアドレスで `writable: true, area: null`
/// （領域による制限が存在しない）。
#[tokio::test]
async fn slmp_protocol_has_no_area_restriction_regardless_of_address() {
    let env = test_env().await;
    let (status, body) = address_writable(&env.router, &env.admin_token, "slmp", "D100").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

/// (2) `virtual`も同様に `writable: true, area: null`。
#[tokio::test]
async fn virtual_protocol_has_no_area_restriction_regardless_of_address() {
    let env = test_env().await;
    let (status, body) = address_writable(&env.router, &env.admin_token, "virtual", "").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

/// (3) `modbus-tcp` + 書き込み可能領域（holding register `40001`・coil
/// `00001`）は `writable: true` かつ `area`が領域名 - **(5)の
/// `writable: null`（判定不能）と区別できること**が本質。
#[tokio::test]
async fn modbus_writable_areas_report_writable_true_with_area_name() {
    let env = test_env().await;

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "40001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("holding_register"),
        "{body:?}"
    );

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "00001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(body["area"], serde_json::json!("coil"), "{body:?}");
}

/// (4) `modbus-tcp` + 読み取り専用領域（discrete input `10001`・input
/// register `30001`）は `writable: false` かつ `area`が領域名 - **(5)の
/// `writable: null`（判定不能）と絶対に混同してはならない**（`false`と
/// `null`の区別がこのエンドポイントの存在意義そのもの）。
#[tokio::test]
async fn modbus_read_only_areas_report_writable_false_not_null_with_area_name() {
    let env = test_env().await;

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "10001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("discrete_input"),
        "{body:?}"
    );

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "30001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("input_register"),
        "{body:?}"
    );
}

/// (5) `modbus-tcp` + パース不能（空文字・`abc`・桁数不正）は
/// `writable: null, area: null`（判定不能）- **(1)の`writable: true`
/// とも(4)の`writable: false`とも異なる第三の値**であることを固定する。
/// 入力途中のアドレス文字列でこれを`false`扱いすると誤って書き込み
/// 拒否UIになり、`true`扱いすると読み取り専用領域へ誤って既定ONに
/// なるところだった（レビュー指摘の核心）。
#[tokio::test]
async fn modbus_unparseable_address_reports_null_writable_and_null_area() {
    let env = test_env().await;

    for address in ["", "abc", "4001", "400001234567"] {
        let (status, body) =
            address_writable(&env.router, &env.admin_token, "modbus-tcp", address).await;
        assert_eq!(status, StatusCode::OK, "address={address:?} body={body:?}");
        assert!(
            body["writable"].is_null(),
            "address={address:?} body={body:?}"
        );
        assert!(body["area"].is_null(), "address={address:?} body={body:?}");
    }
}

/// (6) 未知の protocol は判定不能 - `writable: null, area: null`。
/// SLMP/virtual/modbus-tcp のいずれでもない値は将来プロトコルの
/// 追加候補であり、安全側（既定を適用しない）へ倒れる。
#[tokio::test]
async fn unknown_protocol_reports_null_writable_and_null_area() {
    let env = test_env().await;
    let (status, body) = address_writable(
        &env.router,
        &env.admin_token,
        "some-future-protocol",
        "40001",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["writable"].is_null(), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}

/// 認可: `require_editor`ではなく`require_auth`のみを要求する意図的な
/// 設計（読み取り専用・副作用なし）なので、viewer でも 200 で呼べる
/// ことを固定する。
#[tokio::test]
async fn viewer_role_can_call_address_writable() {
    let env = test_env().await;
    let (status, body) =
        address_writable(&env.router, &env.viewer_token, "modbus-tcp", "40001").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("holding_register"),
        "{body:?}"
    );
}

/// bit接尾辞（T8 §6.1、`banto_plc::address::Address::parse`）: レジスタ
/// 領域（holding/input register）では`.N`が受理され、`area`は bit を
/// 無視してレジスタ自体の領域を返す - `AddressWritableResponse`は bit
/// 位置を運ばない（doc comment参照）ので、`.5`の有無で判定結果が
/// 変わらないことを固定する。
#[tokio::test]
async fn modbus_bit_suffix_on_register_area_is_accepted_and_reports_register_area() {
    let env = test_env().await;

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "30001.5").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(false), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("input_register"),
        "{body:?}"
    );

    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "40001.3").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["writable"], serde_json::json!(true), "{body:?}");
    assert_eq!(
        body["area"],
        serde_json::json!("holding_register"),
        "{body:?}"
    );
}

/// bit接尾辞をコイル/discrete input（既にビット粒度）へ付けるのは
/// `Address::parse`が拒否する（"すでにビット粒度の領域への冗長な`.N`は
/// 無音で受理せず、未知エリアと同様に拒否する"という`parse`自身の doc
/// comment）ので、このエンドポイントでは判定不能（`null`/`null`）に
/// フォールバックすることを固定する。
#[tokio::test]
async fn modbus_bit_suffix_on_coil_area_is_rejected_as_unparseable() {
    let env = test_env().await;
    let (status, body) =
        address_writable(&env.router, &env.admin_token, "modbus-tcp", "00001.0").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert!(body["writable"].is_null(), "{body:?}");
    assert!(body["area"].is_null(), "{body:?}");
}
