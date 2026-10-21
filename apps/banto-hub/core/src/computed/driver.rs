//! `virtual` の静的な部分（R2b-1a、docs/banto-hub-connection-settings-design.md
//! §3.2・§4.1）: [`VirtualDriver`]（`banto_driver_api::DriverBase`）。
//!
//! `virtual` 接続（`calc` / `mem`）は演算タグ・内部タグの置き場で、どこへも繋がない
//! （`banto_tags::plc_connection` の module doc「`"virtual"`」）。I/O に当たる評価は
//! [`super::ComputedEngine`] とタグ空間ストアが持ち続け、ここは識別と設定の検証だけを
//! ドライバの形で持つ。レジストリには `DriverKind::HostProvided` で載せる
//! （[`crate::host_provided`]）。**R2b-1a では誰もレジストリを読まない。**
//!
//! ## 設計の「空のオブジェクトだけを許す」と適合テスト C2 の調停
//!
//! 設計 §4.1 は `virtual` の `settings` を「空のオブジェクトだけを許す」とするが、
//! `DriverBase::validate_config` は**未知のキーを断ってはならない**（適合テスト C2、
//! `static_checks` が未知のキーを足して確かめる）。ふたつは `Settings`（文字列の対）の
//! 層では両立しないので、**適合テストを優先**する: この `validate_config` は常に空
//! （何も断らない）を返す。「`settings` は `{}` だけ」は JSON の層（`banto-tags` の保存の
//! 層、R2b-1c/1d）が持つ規則で、`virtual` の行の `settings` は移行で `{}` に揃う
//! （§3.2「`virtual` は `{}`」）。`push` の `validate_config` が未知のキーを断らず、
//! JSON そのものの厳しさは `PlcConnectionService` の入口が持つ（`ingest/driver.rs`）のと
//! 同じ分担。
//!
//! 秘密のキーは無い。タグは `address` を持たない（`computed` / `internal` は address
//! 禁止、`banto_tags::tag`）ので、アドレスの関数はすべて断る。

use banto_driver_api::{
    AddressError, Capabilities, DriverAddress, DriverBase, DriverDescriptor, DriverId, FieldError,
    Settings, TagKind,
};

/// `plc_connections.protocol` の `"virtual"`（`banto_tags::VIRTUAL_PROTOCOL` と同じ綴り）。
pub const VIRTUAL_PROTOCOL: &str = banto_tags::VIRTUAL_PROTOCOL;

/// アドレスの関数が断るときの文言。
const NO_ADDRESS: &str = "予約接続（calc/mem）のタグにアドレスはありません";

/// `virtual` の `DriverBase`（モジュールの doc 参照）。状態は持たない。
#[derive(Debug, Clone)]
pub struct VirtualDriver {
    descriptor: DriverDescriptor,
}

impl VirtualDriver {
    /// `id = "virtual"`、`transports = []`（どこへも繋がない）。`capabilities` は読み書き
    /// （内部タグはクライアントの書き込みで更新できる。書き込みの可否は従来どおり
    /// タグの `writable` と各ゲートが決める。`Capabilities` の doc 参照）。
    pub fn new() -> Self {
        let id = DriverId::new(VIRTUAL_PROTOCOL).expect("\"virtual\" fits the DriverId grammar");
        VirtualDriver {
            descriptor: DriverDescriptor::new(
                id,
                env!("CARGO_PKG_VERSION"),
                "予約接続（演算・内部タグ）",
                Capabilities::READ_WRITE,
            )
            .with_transports(Vec::new()),
        }
    }
}

impl Default for VirtualDriver {
    fn default() -> Self {
        Self::new()
    }
}

/// host がレジストリへ登録するときの入口（[`crate::host_provided`]）。
pub fn virtual_driver() -> VirtualDriver {
    VirtualDriver::new()
}

impl DriverBase for VirtualDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        &self.descriptor
    }

    /// 何も断らない（モジュールの doc: 適合テスト C2 が未知のキーの許容を求める）。
    fn validate_config(&self, _settings: &Settings) -> Vec<FieldError> {
        Vec::new()
    }

    fn parse_address(&self, _text: &str, _kind: &TagKind) -> Result<DriverAddress, AddressError> {
        Err(AddressError::new(NO_ADDRESS))
    }

    fn offset_address(
        &self,
        _canonical: &str,
        _kind: &TagKind,
        _n: i64,
    ) -> Result<String, AddressError> {
        Err(AddressError::new(NO_ADDRESS))
    }

    fn address_help(&self) -> String {
        NO_ADDRESS.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::DataType;
    use banto_driver_conformance::{static_checks, StaticFixtures};

    #[test]
    fn virtual_driver_passes_the_static_conformance_checks() {
        let kind = TagKind::numeric(DataType::F64);
        let fixtures = StaticFixtures::new(Settings::new())
            .invalid("D100", kind.clone())
            .invalid("", kind.clone())
            .offset("x", kind, 0, None);
        let failures = static_checks(&virtual_driver(), &fixtures);
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn the_descriptor_is_virtual_without_transports_or_secrets() {
        let driver = VirtualDriver::new();
        let d = driver.descriptor();
        assert_eq!(d.id.as_str(), "virtual");
        assert_eq!(d.id.as_str(), banto_tags::VIRTUAL_PROTOCOL);
        assert!(d.transports.is_empty());
        assert_eq!(d.settings_version, 1);
        assert!(driver.secret_keys().is_empty());
    }

    #[test]
    fn nothing_is_refused_not_even_unknown_keys() {
        let driver = VirtualDriver::new();
        let s = Settings::from_pairs([("host", "x"), ("anything", "y")]).unwrap();
        assert_eq!(driver.validate_config(&s), vec![]);
        assert_eq!(driver.validate_config(&Settings::new()), vec![]);
    }
}
