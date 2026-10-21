//! 接続の `settings`（JSON）⇔ ドライバ API の `Settings`、平らな形 ⇔ `settings`、
//! レジストリによる検証（R2b-1c、docs/banto-hub-connection-settings-design.md
//! §3.2・§3.3・§3.5・§4.2・§4.3）。
//!
//! **R2b-1c では保存にまだ使わない**（DB の列は固定列のまま。migration と
//! `PlcConnection` の形の切り替えは R2b-1d）。ここにあるのは、R2b-1d がそのまま使う
//! 純関数と、「旧い平らな形の検証」と「レジストリの検証」が同じ入力を断ることを
//! 表のテストで固定するための部品だけ。
//!
//! ## [`settings_to_api`]（設計 §3.5）
//!
//! `banto_driver_api::Settings` は文字列の対しか持てない（入れ子を持てない）ので、
//! JSON の値を次の規則で文字列にする。**API は変えず、ホストの純関数 1 つで変換する**。
//!
//! | JSON の値          | `Settings` の値                                      |
//! | ------------------ | ---------------------------------------------------- |
//! | 文字列             | そのまま                                             |
//! | 整数・小数         | 10 進の文字列（`serde_json` の表記）                 |
//! | `true` / `false`   | `"true"` / `"false"`                                 |
//! | オブジェクト・配列 | 詰めた JSON の文字列（R2a の `push` の `bridge`）    |
//! | 秘密の参照         | **渡さない**（検証には渡らない。接続にだけ開封して渡す） |
//! | `null`             | キーを渡さない                                       |
//!
//! **秘密の参照の見分け方**: キーを**ちょうど 1 つ** `"$secret"` だけ持ち、その値が
//! **文字列**のオブジェクト（`{"$secret": "<ref>"}`、設計 §3.3）。キーが増えている・値が
//! 文字列でない・`"$secret"` 以外のキーのオブジェクトは秘密の参照ではなく、ふつうの
//! オブジェクトとして詰めた JSON の文字列になる（秘密のキーに平文を置く取りこぼしを
//! 断るのは保存の層、R2b-1d）。`serde_json` の `Map` は既定では順序を持たない（キーの
//! 辞書順）ので、`Settings` の並びも辞書順になる。並びに意味を持たせる呼び出し側は
//! 無い（検証は `get` で引く）。
//!
//! ## [`legacy`]（設計 §4.3、R2b-7 まで）
//!
//! REST・MCP・設定パッケージ・ChronoGazer の**平らな形**（`host`・`port`・`unit_id`・
//! `word_order`・`cpu_type`・`database`・`username`・`password`）と `settings` の間の
//! アダプタ。[`legacy::settings_from_flat`] は §3.2 の表のとおりにプロトコルごとに写し、
//! [`legacy::flat_view`] は逆向き（`settings` から平らな欄を作る読み取りの写し）。
//!
//! ## [`validate_settings_with_registry`]（設計 §4.2 の (3)(4)）
//!
//! `registry.get(protocol)` が無ければ `protocol` の項目エラー、あれば
//! [`settings_to_api`] → `validate_config` → `FieldError.key` を REST の綴り
//! （`unit_id` → `unitId`、`word_order` → `wordOrder`、`cpu_type` → `cpuType`、`push` は
//! 今の `settings.*`）に写して返す。旧い平らな形の検証（(1)）は呼ばない
//! （`plc_connection.rs` の `validate_plc_connection_input`）。

use banto_core::FieldError;
use banto_driver_api::{DriverRegistry, Settings};
use serde_json::{Map, Value};

/// 秘密の参照のキー（設計 §3.3 `{"$secret": "<ref>"}`）。
pub const SECRET_REF_KEY: &str = "$secret";

/// `value` が秘密の参照か（モジュールの doc: キーがちょうど `"$secret"` 1 つで、値が文字列の
/// オブジェクト）。
pub fn is_secret_ref(value: &Value) -> bool {
    match value {
        Value::Object(object) => {
            object.len() == 1 && matches!(object.get(SECRET_REF_KEY), Some(Value::String(_)))
        }
        _ => false,
    }
}

/// 接続の `settings`（JSON のオブジェクト）をドライバ API の [`Settings`] に直す
/// （設計 §3.5、モジュールの doc の表）。純関数。
pub fn settings_to_api(map: &Map<String, Value>) -> Settings {
    let mut settings = Settings::new();
    for (key, value) in map {
        match value {
            Value::Null => {}
            Value::String(text) => settings.insert(key.as_str(), text.as_str()),
            Value::Number(number) => settings.insert(key.as_str(), number.to_string()),
            Value::Bool(flag) => settings.insert(key.as_str(), flag.to_string()),
            Value::Object(_) | Value::Array(_) => {
                if !is_secret_ref(value) {
                    settings.insert(key.as_str(), value.to_string());
                }
            }
        }
    }
    settings
}

/// `FieldError.key`（`Settings` のキー）を REST の項目名（`validate_plc_connection_input` の
/// `field`）に写す。`push` は今の `settings.*`、ほかは `unit_id` / `word_order` /
/// `cpu_type` だけ camelCase にして、残り（`host`・`port`・`database`・`username` ほか）は
/// そのまま。
///
/// `push` の入れ子（`bridge.queries[0].sql: ...` のように文言の先頭へ残された位置、
/// `ingest/driver.rs` の `to_driver_field_error`）は、`settings.bridge.queries[0].sql` の
/// 項目名と元の文言に戻す。
fn rest_field_error(protocol: &str, key: &str, message: String) -> FieldError {
    if protocol == crate::PUSH_PROTOCOL {
        if let Some(rest) = message.strip_prefix(key) {
            if rest.starts_with(['.', '[']) {
                if let Some((path, inner)) = message.split_once(": ") {
                    return FieldError {
                        field: format!("settings.{path}"),
                        message: inner.to_string(),
                    };
                }
            }
        }
        return FieldError {
            field: format!("settings.{key}"),
            message,
        };
    }
    let field = match key {
        "unit_id" => "unitId",
        "word_order" => "wordOrder",
        "cpu_type" => "cpuType",
        other => other,
    };
    FieldError {
        field: field.to_string(),
        message,
    }
}

/// 設計 §4.2 の (3)(4): `protocol` をレジストリで引き、`settings` をそのドライバの
/// `validate_config` に掛けて、項目エラーを REST の綴りで返す。空なら通った。
///
/// `registry.get(protocol)` が無ければ `protocol` の項目エラー 1 つだけ
/// （`validate_plc_connection_input` の文言と同じ形で、レジストリの id を並べる）。
/// 平らな形の旧い検証は含まない。
pub fn validate_settings_with_registry(
    registry: &DriverRegistry,
    protocol: &str,
    settings: &Map<String, Value>,
) -> Vec<FieldError> {
    let Some(entry) = registry.get(protocol) else {
        let ids: Vec<&str> = registry.ids().map(|id| id.as_str()).collect();
        return vec![FieldError {
            field: "protocol".to_string(),
            message: format!("対応プロトコルは {} のいずれかです", ids.join(", ")),
        }];
    };
    entry
        .base()
        .validate_config(&settings_to_api(settings))
        .into_iter()
        .map(|error| rest_field_error(protocol, &error.key, error.message))
        .collect()
}

/// 平らな形 ⇔ `settings` のアダプタ（設計 §4.3、R2b-7 で消える）。
pub mod legacy {
    use super::{validate_settings_with_registry, DriverRegistry, FieldError, Map, Value};
    use crate::plc_connection::{
        default_word_order_for, normalize_plc_connection_input, PlcConnectionInput, CPU_TYPE_R,
        MODBUS_PROTOCOL, POSTGRES_PROTOCOL, PUSH_PROTOCOL,
    };

    /// `slmp` の綴り（`ALLOWED_PROTOCOLS` と同じ。定数は無い）。
    const SLMP_PROTOCOL: &str = "slmp";

    /// 平らな欄を `settings` から作った読み取りの写し（`PlcConnection` の同名の欄）。
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct FlatView {
        pub host: String,
        pub port: i64,
        pub unit_id: i64,
        pub word_order: String,
        pub cpu_type: String,
        pub database: Option<String>,
        pub username: Option<String>,
    }

    /// 正規化（`normalize_plc_connection_input`）と旧い検証を通った入力を、設計 §3.2 の
    /// 表のとおりに `settings` へ写す。**プロトコルが読まない欄は写さない**（捨てる）。
    ///
    /// | `protocol`   | `settings` のキー                                     |
    /// | ------------ | ----------------------------------------------------- |
    /// | `modbus-tcp` | `host`・`port`・`unit_id`・`word_order`               |
    /// | `slmp`       | `host`・`port`・`word_order`・`cpu_type`              |
    /// | `postgres`   | `host`・`port`・`database`・`username`（`password` は下）|
    /// | `virtual`    | `{}`                                                  |
    /// | `hub`        | `{}`                                                  |
    /// | `push`       | `input.settings` をそのまま（`None`・`null` は `{}`） |
    /// | その他       | `{}`（レジストリが断る）                              |
    ///
    /// - `host`・`database`・`username` は保存と同じく前後の空白を除く。
    /// - `slmp` の `cpu_type` は、入力が `None`・空のとき**キーを置かない**（作成は既定の
    ///   `R`、更新は既存を保つ、`PlcConnectionInput::cpu_type`。R2b-1d が書き込みの
    ///   トランザクションの中で合わせる。`flat_view` は無ければ `R`）。
    /// - **`postgres` の `password` は写さない**。パスワードは秘密値で、`settings` には
    ///   `{"$secret": "<ref>"}` しか置かない（設計 §3.3）。R2b-1c にはまだ封印が無いので
    ///   平文を `settings` に入れず、R2b-1d（封印の配線は R2b-1e）が参照を挿す。
    /// - `push` で `settings` がオブジェクトでないとき（検証が先に断る入力）は `{}`。
    ///   この関数は検証の後に呼ぶ前提で、断る責任は旧い検証が持つ。
    pub fn settings_from_flat(input: &PlcConnectionInput) -> Map<String, Value> {
        let mut map = Map::new();
        let protocol = input.protocol.as_str();
        if protocol == MODBUS_PROTOCOL {
            map.insert("host".into(), input.host.trim().into());
            map.insert("port".into(), input.port.into());
            map.insert("unit_id".into(), input.unit_id.into());
            map.insert("word_order".into(), input.word_order.clone().into());
        } else if protocol == SLMP_PROTOCOL {
            map.insert("host".into(), input.host.trim().into());
            map.insert("port".into(), input.port.into());
            map.insert("word_order".into(), input.word_order.clone().into());
            if let Some(cpu_type) = input.cpu_type.as_deref().filter(|s| !s.is_empty()) {
                map.insert("cpu_type".into(), cpu_type.into());
            }
        } else if protocol == POSTGRES_PROTOCOL {
            map.insert("host".into(), input.host.trim().into());
            map.insert("port".into(), input.port.into());
            if let Some(database) = input.database.as_deref() {
                map.insert("database".into(), database.trim().into());
            }
            if let Some(username) = input.username.as_deref() {
                map.insert("username".into(), username.trim().into());
            }
        } else if protocol == PUSH_PROTOCOL {
            if let Some(Value::Object(settings)) = &input.settings {
                map = settings.clone();
            }
        }
        // `virtual`・`hub`・知らないプロトコルは `{}`。
        map
    }

    /// 設計 §4.2 の (2)〜(4) を平らな入力から続けて行う: 正規化 → [`settings_from_flat`] →
    /// [`super::validate_settings_with_registry`]。旧い平らな形の検証 (1) は含まない
    /// （それは `PlcConnectionService` の書き込みの入口が先に行う）。R2b-1c では保存に
    /// 使わず、旧い検証とレジストリの検証が同じ入力を断ることの表のテスト
    /// （`apps/banto-hub/host/tests/connection_validation_consistency.rs`）が呼ぶ。
    pub fn validate_flat_with_registry(
        registry: &DriverRegistry,
        input: PlcConnectionInput,
    ) -> Vec<FieldError> {
        let input = normalize_plc_connection_input(input);
        let settings = settings_from_flat(&input);
        validate_settings_with_registry(registry, &input.protocol, &settings)
    }

    /// `settings` から平らな欄を作る（設計 §4.3）。**プロトコルが読むキーだけ**を読み
    /// （`settings_from_flat` の表と同じ）、読まない欄と無いキーは今の既定にする:
    /// `host` は `""`、`port` は 0、`unit_id` は 1、`word_order` はプロトコルの既定
    /// （`modbus-tcp` は `high_low`、ほかは `low_high`）、`cpu_type` は `R`、
    /// `database`・`username` は `None`。
    ///
    /// 数は JSON の数のほか、10 進の整数を表す文字列も受ける（`settings_to_api` が
    /// 文字列にして渡す先のドライバと同じ読み方）。読めない型は既定にする（失敗しない。
    /// 検証に通らない `settings` はレジストリの検証が断る）。
    pub fn flat_view(protocol: &str, settings: &Map<String, Value>) -> FlatView {
        let reads_endpoint = protocol == MODBUS_PROTOCOL
            || protocol == SLMP_PROTOCOL
            || protocol == POSTGRES_PROTOCOL;
        let text = |key: &str| {
            settings
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let int = |key: &str| match settings.get(key) {
            Some(Value::Number(number)) => number.as_i64(),
            Some(Value::String(text)) => text.trim().parse::<i64>().ok(),
            _ => None,
        };
        let is_postgres = protocol == POSTGRES_PROTOCOL;
        FlatView {
            host: if reads_endpoint {
                text("host").unwrap_or_default()
            } else {
                String::new()
            },
            port: if reads_endpoint {
                int("port").unwrap_or(0)
            } else {
                0
            },
            unit_id: if protocol == MODBUS_PROTOCOL {
                int("unit_id").unwrap_or(1)
            } else {
                1
            },
            word_order: if protocol == MODBUS_PROTOCOL || protocol == SLMP_PROTOCOL {
                text("word_order").unwrap_or_else(|| default_word_order_for(protocol))
            } else {
                default_word_order_for(protocol)
            },
            cpu_type: if protocol == SLMP_PROTOCOL {
                text("cpu_type").unwrap_or_else(|| CPU_TYPE_R.to_string())
            } else {
                CPU_TYPE_R.to_string()
            },
            database: if is_postgres { text("database") } else { None },
            username: if is_postgres { text("username") } else { None },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::legacy::{flat_view, settings_from_flat, FlatView};
    use super::*;
    use crate::plc_connection::PlcConnectionInput;
    use serde_json::json;

    fn object(value: Value) -> Map<String, Value> {
        match value {
            Value::Object(map) => map,
            other => panic!("not an object: {other}"),
        }
    }

    fn pairs(settings: &Settings) -> Vec<(String, String)> {
        let mut pairs: Vec<(String, String)> = settings
            .iter()
            .map(|s| (s.key.clone(), s.value.clone()))
            .collect();
        pairs.sort();
        pairs
    }

    fn p(key: &str, value: &str) -> (String, String) {
        (key.to_string(), value.to_string())
    }

    // ---- JSON ⇔ Settings（設計 §3.5 の表 1 行 1 ケース）----

    #[test]
    fn json_to_settings_follows_the_design_table_row_by_row() {
        // (行の名前, JSON の値, `Settings` に出るキーの値。None はキーが出ない)
        let rows: Vec<(&str, Value, Option<&str>)> = vec![
            ("string", json!("high_low"), Some("high_low")),
            ("empty string", json!(""), Some("")),
            ("integer", json!(502), Some("502")),
            ("negative integer", json!(-1), Some("-1")),
            ("float", json!(1.5), Some("1.5")),
            ("true", json!(true), Some("true")),
            ("false", json!(false), Some("false")),
            (
                "object",
                json!({"b": 1, "a": [1, 2]}),
                Some(r#"{"a":[1,2],"b":1}"#),
            ),
            ("array", json!([1, "x", null]), Some(r#"[1,"x",null]"#)),
            (
                "secret reference",
                json!({"$secret": "0123456789abcdef0123456789abcdef"}),
                None,
            ),
            ("null", Value::Null, None),
        ];
        for (name, value, want) in rows {
            let mut map = Map::new();
            map.insert("k".to_string(), value);
            let settings = settings_to_api(&map);
            assert_eq!(settings.get("k"), want, "{name}");
            assert_eq!(
                settings.iter().count(),
                usize::from(want.is_some()),
                "{name}"
            );
        }
    }

    #[test]
    fn only_an_object_with_exactly_one_string_secret_key_is_a_secret_reference() {
        let secret = json!({"$secret": "ref"});
        assert!(is_secret_ref(&secret));
        for not_secret in [
            json!({"$secret": 5}),
            json!({"$secret": null}),
            json!({"$secret": "ref", "extra": 1}),
            json!({"$other": "ref"}),
            json!({}),
            json!("$secret"),
            json!(["$secret"]),
        ] {
            assert!(!is_secret_ref(&not_secret), "{not_secret}");
        }
        // 秘密の参照ではない `$secret` つきのオブジェクトは、ふつうの JSON の文字列になる。
        let map = object(json!({"k": {"$secret": "ref", "extra": 1}}));
        assert_eq!(
            settings_to_api(&map).get("k"),
            Some(r#"{"$secret":"ref","extra":1}"#)
        );
    }

    #[test]
    fn settings_to_api_keeps_every_key_once_and_skips_nulls_and_secrets_together() {
        let map = object(json!({
            "host": "plc.local",
            "port": 502,
            "password": {"$secret": "ref"},
            "gone": null,
            "bridge": {"kind": "db"},
        }));
        assert_eq!(
            pairs(&settings_to_api(&map)),
            [
                p("bridge", r#"{"kind":"db"}"#),
                p("host", "plc.local"),
                p("port", "502"),
            ]
        );
        assert_eq!(settings_to_api(&Map::new()), Settings::new());
    }

    // ---- §3.2 の写し方（プロトコルごと、捨てる欄を含む）----

    fn input(protocol: &str) -> PlcConnectionInput {
        PlcConnectionInput {
            name: "c".to_string(),
            protocol: protocol.to_string(),
            host: " 10.0.0.5 ".to_string(),
            port: 5000,
            unit_id: 7,
            enabled: true,
            simulation: false,
            word_order: "high_low".to_string(),
            cpu_type: Some("Q".to_string()),
            database: Some(" appdb ".to_string()),
            username: Some(" alice ".to_string()),
            password: Some("secret-plaintext".to_string()),
            settings: Some(json!({"stale_timeout_ms": 5000})),
        }
    }

    #[test]
    fn settings_from_flat_follows_design_3_2_per_protocol() {
        // modbus-tcp: host・port・unit_id・word_order。cpu_type・database・username・
        // password・settings は捨てる。
        assert_eq!(
            settings_from_flat(&input("modbus-tcp")),
            object(
                json!({"host": "10.0.0.5", "port": 5000, "unit_id": 7, "word_order": "high_low"})
            )
        );
        // slmp: host・port・word_order・cpu_type。unit_id は捨てる。
        assert_eq!(
            settings_from_flat(&input("slmp")),
            object(
                json!({"host": "10.0.0.5", "port": 5000, "word_order": "high_low", "cpu_type": "Q"})
            )
        );
        // postgres: host・port・database・username。password は写さない（秘密の参照は
        // R2b-1d）。unit_id・word_order・cpu_type は捨てる。
        assert_eq!(
            settings_from_flat(&input("postgres")),
            object(
                json!({"host": "10.0.0.5", "port": 5000, "database": "appdb", "username": "alice"})
            )
        );
        // virtual・hub: {}。
        assert_eq!(settings_from_flat(&input("virtual")), Map::new());
        assert_eq!(settings_from_flat(&input("hub")), Map::new());
        // push: 今の settings をそのまま。
        assert_eq!(
            settings_from_flat(&input("push")),
            object(json!({"stale_timeout_ms": 5000}))
        );
        let mut push = input("push");
        push.settings = None;
        assert_eq!(settings_from_flat(&push), Map::new());
        push.settings = Some(Value::Null);
        assert_eq!(settings_from_flat(&push), Map::new());
        // 知らないプロトコル: {}（レジストリが protocol で断る）。
        assert_eq!(settings_from_flat(&input("bogus")), Map::new());
    }

    #[test]
    fn settings_from_flat_never_carries_the_plaintext_password() {
        for protocol in ["modbus-tcp", "slmp", "postgres", "virtual", "hub", "push"] {
            let text = Value::Object(settings_from_flat(&input(protocol))).to_string();
            assert!(!text.contains("secret-plaintext"), "{protocol}: {text}");
            assert!(!text.contains("password"), "{protocol}: {text}");
        }
    }

    #[test]
    fn slmp_cpu_type_is_left_out_when_unspecified() {
        for unspecified in [None, Some(String::new())] {
            let mut slmp = input("slmp");
            slmp.cpu_type = unspecified;
            let map = settings_from_flat(&slmp);
            assert!(!map.contains_key("cpu_type"));
            assert_eq!(flat_view("slmp", &map).cpu_type, "R");
        }
    }

    // ---- flat_view の既定（設計 §4.3）----

    #[test]
    fn flat_view_of_an_empty_settings_is_the_defaults() {
        let empty = Map::new();
        let defaults = |word_order: &str| FlatView {
            host: String::new(),
            port: 0,
            unit_id: 1,
            word_order: word_order.to_string(),
            cpu_type: "R".to_string(),
            database: None,
            username: None,
        };
        assert_eq!(flat_view("modbus-tcp", &empty), defaults("high_low"));
        for protocol in ["slmp", "postgres", "virtual", "hub", "push", "bogus"] {
            assert_eq!(
                flat_view(protocol, &empty),
                defaults("low_high"),
                "{protocol}"
            );
        }
    }

    #[test]
    fn flat_view_reads_only_the_keys_the_protocol_reads() {
        let everything = object(json!({
            "host": "h", "port": 9, "unit_id": 3, "word_order": "high_low",
            "cpu_type": "Q", "database": "d", "username": "u",
        }));
        let view = |protocol: &str| flat_view(protocol, &everything);
        let modbus = view("modbus-tcp");
        assert_eq!(
            (modbus.host.as_str(), modbus.port, modbus.unit_id),
            ("h", 9, 3)
        );
        assert_eq!(modbus.word_order, "high_low");
        assert_eq!(
            (modbus.cpu_type.as_str(), modbus.database, modbus.username),
            ("R", None, None)
        );
        let slmp = view("slmp");
        assert_eq!((slmp.host.as_str(), slmp.port, slmp.unit_id), ("h", 9, 1));
        assert_eq!(
            (slmp.word_order.as_str(), slmp.cpu_type.as_str()),
            ("high_low", "Q")
        );
        assert_eq!((slmp.database, slmp.username), (None, None));
        let postgres = view("postgres");
        assert_eq!(
            (postgres.host.as_str(), postgres.port, postgres.unit_id),
            ("h", 9, 1)
        );
        assert_eq!(postgres.word_order, "low_high");
        assert_eq!(postgres.database.as_deref(), Some("d"));
        assert_eq!(postgres.username.as_deref(), Some("u"));
        for protocol in ["virtual", "hub", "push"] {
            assert_eq!(
                view(protocol),
                flat_view(protocol, &Map::new()),
                "{protocol}"
            );
        }
    }

    #[test]
    fn flat_view_reads_decimal_strings_as_numbers_and_ignores_unreadable_types() {
        let strings = object(json!({"host": "h", "port": " 502 ", "unit_id": "7"}));
        let view = flat_view("modbus-tcp", &strings);
        assert_eq!((view.port, view.unit_id), (502, 7));
        let junk = object(json!({"host": 5, "port": "x", "unit_id": true, "word_order": 1}));
        let view = flat_view("modbus-tcp", &junk);
        assert_eq!(
            (
                view.host.as_str(),
                view.port,
                view.unit_id,
                view.word_order.as_str()
            ),
            ("", 0, 1, "high_low")
        );
    }

    /// 往復: 正規化した入力 → `settings_from_flat` → `flat_view` が、プロトコルが読む欄を
    /// 再現し、捨てた欄は既定にする。
    #[test]
    fn flat_view_of_settings_from_flat_round_trips_per_protocol() {
        struct Row {
            protocol: &'static str,
            want: FlatView,
        }
        let view = |host: &str,
                    port,
                    unit_id,
                    word_order: &str,
                    cpu: &str,
                    db: Option<&str>,
                    user: Option<&str>| FlatView {
            host: host.to_string(),
            port,
            unit_id,
            word_order: word_order.to_string(),
            cpu_type: cpu.to_string(),
            database: db.map(str::to_string),
            username: user.map(str::to_string),
        };
        let rows = [
            Row {
                protocol: "modbus-tcp",
                want: view("10.0.0.5", 5000, 7, "high_low", "R", None, None),
            },
            Row {
                protocol: "slmp",
                want: view("10.0.0.5", 5000, 1, "high_low", "Q", None, None),
            },
            Row {
                protocol: "postgres",
                want: view(
                    "10.0.0.5",
                    5000,
                    1,
                    "low_high",
                    "R",
                    Some("appdb"),
                    Some("alice"),
                ),
            },
            Row {
                protocol: "virtual",
                want: view("", 0, 1, "low_high", "R", None, None),
            },
            Row {
                protocol: "hub",
                want: view("", 0, 1, "low_high", "R", None, None),
            },
            Row {
                protocol: "push",
                want: view("", 0, 1, "low_high", "R", None, None),
            },
        ];
        for row in rows {
            let map = settings_from_flat(&input(row.protocol));
            assert_eq!(flat_view(row.protocol, &map), row.want, "{}", row.protocol);
        }
    }

    // ---- rest_field_error ----

    #[test]
    fn field_keys_are_written_in_the_rest_spelling() {
        let cases = [
            ("modbus-tcp", "unit_id", "unitId"),
            ("modbus-tcp", "word_order", "wordOrder"),
            ("slmp", "cpu_type", "cpuType"),
            ("slmp", "host", "host"),
            ("postgres", "port", "port"),
            ("postgres", "database", "database"),
            ("postgres", "username", "username"),
            ("push", "stale_timeout_ms", "settings.stale_timeout_ms"),
            ("push", "bridge", "settings.bridge"),
        ];
        for (protocol, key, field) in cases {
            assert_eq!(
                rest_field_error(protocol, key, "m".to_string()).field,
                field
            );
        }
        // push の入れ子の位置は項目名へ戻し、文言は元に戻す。
        let nested = rest_field_error(
            "push",
            "bridge",
            "bridge.queries[0].sql: 空です".to_string(),
        );
        assert_eq!(nested.field, "settings.bridge.queries[0].sql");
        assert_eq!(nested.message, "空です");
        // 位置でない先頭一致（"bridge を JSON として…"）は戻さない。
        let not_nested = rest_field_error(
            "push",
            "bridge",
            "bridge を JSON として読めません: x".to_string(),
        );
        assert_eq!(not_nested.field, "settings.bridge");
        assert_eq!(not_nested.message, "bridge を JSON として読めません: x");
    }

    #[test]
    fn an_unregistered_protocol_is_a_protocol_field_error() {
        let registry = DriverRegistry::new();
        let errors = validate_settings_with_registry(&registry, "modbus-tcp", &Map::new());
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].field, "protocol");
        assert!(errors[0].message.starts_with("対応プロトコルは "));
    }
}
