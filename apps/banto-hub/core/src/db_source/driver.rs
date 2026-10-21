//! `postgres` の静的な部分（R2b-1a、docs/banto-hub-connection-settings-design.md
//! §3.2・§4.1）: [`PostgresDriver`]（`banto_driver_api::DriverBase`）。
//!
//! `postgres` 接続は DB Source / DB Sink が使う外部 PostgreSQL への接続で、PLC では
//! ない。I/O（接続・ポーリング・書き込み）は [`super`] の `DbSourceEngine` と DB Sink が
//! 持ち続け、ここは**識別・設定の検証・秘密のキー**だけをドライバの形で持つ。
//! レジストリには `DriverKind::HostProvided` で載せる（[`crate::host_provided`]）。
//! **R2b-1a では誰もレジストリを読まない**（本番の検証は今までどおり
//! `banto_tags::PlcConnectionService` の `validate_plc_connection_input`）。R2b-1d で
//! 検証の正がレジストリになるとき、この規則が旧い規則と同じ入力を断る必要があるので、
//! 規則は `validate_plc_connection_input` の postgres の分を**そのまま写す**
//! （`host` 必須・`port` 1..=65535・`database` / `username` 必須で 128 文字まで）。
//!
//! ## `Settings` のキー（設計 §3.2・§3.5）
//!
//! | キー       | 規則                                                            |
//! | ---------- | --------------------------------------------------------------- |
//! | `host`     | 前後の空白を除いて空でない                                      |
//! | `port`     | 10 進の整数で 1..=65535                                         |
//! | `database` | 前後の空白を除いて空でなく、128 文字（`chars`）まで             |
//! | `username` | 同上                                                            |
//! | `password` | 秘密のキー（`secret_keys`）。**形の規則は無い**（任意・空も可） |
//!
//! `password` の値は §3.5 の規則で検証には渡されない（秘密の参照は `Settings` に
//! 入らない）。未知のキーは断らない（`DriverBase` の約束・適合テスト C2）。
//!
//! アドレスの関数: `postgres` 接続の下にタグは置けない（`banto_tags` の配置規則）ので、
//! `parse_address` / `offset_address` はすべて断る。

use banto_driver_api::{
    keys, AddressError, Capabilities, DriverAddress, DriverBase, DriverDescriptor, DriverId,
    FieldError, Settings, TagKind,
};

/// `plc_connections.protocol` の `"postgres"`（`banto_tags::POSTGRES_PROTOCOL` と同じ綴り）。
pub const POSTGRES_PROTOCOL: &str = banto_tags::POSTGRES_PROTOCOL;

/// `Settings` のキー: 接続先のデータベース名。
pub const DATABASE_KEY: &str = "database";

/// `Settings` のキー: 接続するユーザー名。
pub const USERNAME_KEY: &str = "username";

/// `Settings` のキー: パスワード（秘密のキー）。
pub const PASSWORD_KEY: &str = "password";

/// `database` / `username` の上限（文字数）。`banto_tags` の `MAX_DATABASE_LEN` /
/// `MAX_USERNAME_LEN` と同じ値（あちらは非公開。境界のテストが食い違いを捕まえる）。
const MAX_IDENT_LEN: usize = 128;
const MIN_PORT: i64 = 1;
const MAX_PORT: i64 = 65535;

/// `postgres` の `DriverBase`（モジュールの doc 参照）。状態は持たない。
#[derive(Debug, Clone)]
pub struct PostgresDriver {
    descriptor: DriverDescriptor,
}

impl PostgresDriver {
    /// `id = "postgres"`、読み取りだけ（DB Source。Sink の書き込みはタグの書き込みでは
    /// ない）、`transports = [Tcp]`（既定）。
    pub fn new() -> Self {
        let id = DriverId::new(POSTGRES_PROTOCOL).expect("\"postgres\" fits the DriverId grammar");
        PostgresDriver {
            descriptor: DriverDescriptor::new(
                id,
                env!("CARGO_PKG_VERSION"),
                "PostgreSQL（DB Source / Sink）",
                Capabilities::READ_ONLY,
            ),
        }
    }
}

impl Default for PostgresDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// host がレジストリへ登録するときの入口（[`crate::host_provided`]）。
pub fn postgres_driver() -> PostgresDriver {
    PostgresDriver::new()
}

impl DriverBase for PostgresDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        let mut errors = Vec::new();

        if settings.get(keys::HOST).unwrap_or("").trim().is_empty() {
            errors.push(FieldError::new(keys::HOST, "必須項目です"));
        }

        match settings.get(keys::PORT) {
            None => errors.push(FieldError::new(keys::PORT, "必須項目です")),
            Some(text) => match text.trim().parse::<i64>() {
                Ok(port) if (MIN_PORT..=MAX_PORT).contains(&port) => {}
                Ok(_) => errors.push(FieldError::new(
                    keys::PORT,
                    format!("{MIN_PORT}〜{MAX_PORT}の範囲で入力してください"),
                )),
                Err(_) => errors.push(FieldError::new(keys::PORT, "整数で指定してください")),
            },
        }

        for key in [DATABASE_KEY, USERNAME_KEY] {
            let value = settings.get(key).unwrap_or("").trim();
            if value.is_empty() {
                errors.push(FieldError::new(key, "必須項目です"));
            } else if value.chars().count() > MAX_IDENT_LEN {
                errors.push(FieldError::new(
                    key,
                    format!("{MAX_IDENT_LEN}文字以内で入力してください"),
                ));
            }
        }
        errors
    }

    fn secret_keys(&self) -> Vec<String> {
        vec![PASSWORD_KEY.to_string()]
    }

    fn parse_address(&self, _text: &str, _kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Err(AddressError::new(
            "postgres 接続の下にタグは置けません（DB Source は別の設定で扱います）",
        ))
    }

    fn offset_address(
        &self,
        _canonical: &str,
        _kind: &TagKind,
        _n: i64,
    ) -> Result<String, AddressError> {
        Err(AddressError::new("postgres 接続にアドレスはありません"))
    }

    fn address_help(&self) -> String {
        "postgres 接続にタグのアドレスはありません".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::{DataType, Transport};
    use banto_driver_conformance::{static_checks, StaticFixtures};

    fn minimal() -> Settings {
        Settings::from_pairs([
            (keys::HOST, "db.example"),
            (keys::PORT, "5432"),
            (DATABASE_KEY, "plant"),
            (USERNAME_KEY, "banto"),
            (PASSWORD_KEY, "pw"),
        ])
        .unwrap()
    }

    fn with(key: &str, value: &str) -> Settings {
        let mut s = minimal();
        s.insert(key, value);
        s
    }

    fn keys_of(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.key.as_str()).collect()
    }

    /// R2b-1a の完了条件: 適合テストの静的な部分（C1・C2）を通る。アドレスは
    /// 持たないので、不正な例だけを置く。
    #[test]
    fn postgres_driver_passes_the_static_conformance_checks() {
        let kind = TagKind::numeric(DataType::F64);
        let fixtures = StaticFixtures::new(minimal())
            .invalid("D100", kind.clone())
            .invalid("", kind.clone())
            .offset("D100", kind, 0, None);
        let failures = static_checks(&postgres_driver(), &fixtures);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn the_descriptor_is_postgres_read_only_over_tcp() {
        let driver = PostgresDriver::new();
        let d = driver.descriptor();
        assert_eq!(d.id.as_str(), "postgres");
        assert_eq!(d.id.as_str(), banto_tags::POSTGRES_PROTOCOL);
        assert_eq!(d.capabilities, Capabilities::READ_ONLY);
        assert_eq!(d.transports, vec![Transport::Tcp]);
        assert_eq!(d.settings_version, 1);
        assert_eq!(driver.secret_keys(), ["password"]);
    }

    #[test]
    fn the_minimal_settings_are_valid_and_the_password_is_optional() {
        let driver = PostgresDriver::new();
        assert_eq!(driver.validate_config(&minimal()), vec![]);
        let no_password = Settings::from_pairs([
            (keys::HOST, "h"),
            (keys::PORT, "5432"),
            (DATABASE_KEY, "d"),
            (USERNAME_KEY, "u"),
        ])
        .unwrap();
        assert_eq!(driver.validate_config(&no_password), vec![]);
        // 他の層のキー（`unit_id` など、旧い平らな形が運ぶもの）は断らない。
        assert_eq!(driver.validate_config(&with(keys::UNIT_ID, "999")), vec![]);
    }

    #[test]
    fn empty_settings_report_every_required_key() {
        let errors = PostgresDriver::new().validate_config(&Settings::new());
        assert_eq!(keys_of(&errors), ["host", "port", "database", "username"]);
    }

    /// `validate_plc_connection_input` の postgres の境界を 1 行ずつ。
    #[test]
    fn the_boundaries_match_the_flat_rules() {
        let driver = PostgresDriver::new();
        let check = |s: &Settings| keys_of(&driver.validate_config(s)).join(",");
        for (port, ok) in [
            ("0", false),
            ("1", true),
            ("65535", true),
            ("65536", false),
            ("-1", false),
            ("abc", false),
            ("", false),
            (" 5432 ", true),
        ] {
            let got = check(&with(keys::PORT, port));
            assert_eq!(got.is_empty(), ok, "port {port:?}: {got}");
            if !ok {
                assert_eq!(got, "port", "port {port:?}");
            }
        }
        // host: 空白だけは空と同じ
        assert_eq!(check(&with(keys::HOST, "   ")), "host");
        assert_eq!(check(&with(keys::HOST, "")), "host");
        // database / username: 128 文字まで（chars で数える）
        for key in [DATABASE_KEY, USERNAME_KEY] {
            assert_eq!(check(&with(key, &"a".repeat(128))), "");
            assert_eq!(check(&with(key, &"a".repeat(129))), key);
            assert_eq!(check(&with(key, &"あ".repeat(128))), "");
            assert_eq!(check(&with(key, &"あ".repeat(129))), key);
            assert_eq!(check(&with(key, "  ")), key);
            // 前後の空白は数えない
            assert_eq!(check(&with(key, &format!(" {} ", "a".repeat(128)))), "");
        }
    }

    #[test]
    fn addresses_are_all_refused() {
        let driver = PostgresDriver::new();
        let kind = TagKind::numeric(DataType::F64);
        assert!(driver.parse_address("x", &kind).is_err());
        assert!(driver.offset_address("x", &kind, 0).is_err());
        assert!(!driver.address_help().is_empty());
    }
}
