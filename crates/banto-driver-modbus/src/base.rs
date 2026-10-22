//! [`ModbusDriver`]: the `banto-driver-api` identity of Modbus TCP
//! (docs/banto-driver-api-design.md §4.1, R1-4b).
//!
//! The stateless half of the driver ([`DriverBase`]): who it is, whether a
//! connection's settings are valid, and the address functions. Its I/O is
//! the `SessionDriver` in `api_session.rs` (registered as
//! `DriverEntry::session` since R2b-3a-2). Since R2b-2
//! (docs/banto-hub-driver-address-design.md) the address functions are on the
//! production path: the save path of `banto-tags` (built with a registry)
//! and the Hub's plan, instant read and write paths call `parse_address`;
//! since R2b-3a-2 the broker carries the resulting `TagSpec` to this
//! driver's session as is. Only the collection plan still goes through the
//! public bridge `Address::from_driver_address` until R2b-3a-3. ChronoGazer
//! keeps the legacy rules until R2b-7.
//!
//! ## Address functions
//!
//! - `parse_address` wraps [`banto_plc::Address::parse`] and reports
//!   `canonical = Address::to_string()` (Q8 = A: always the 6-digit form,
//!   `400001`, a bit suffix without a leading zero, `400001.3`).
//! - `offset_address` is a port of `incrementAddress`
//!   (`apps/banto-hub/src/lib/banto/continuousRegistration.ts`), the
//!   fallback branch that handles Modbus reference numbers. See
//!   [`DriverBase::offset_address`]'s impl for the semantics of `n` and for
//!   the three places where it differs from the TypeScript on purpose.
//! - `address_help` is a port of `tagAddressHelp.ts`'s `modbus-tcp` branch,
//!   text unchanged. The TypeScript also returns an *occupancy* hint that
//!   depends on the tag's data type; `address_help()` takes no data type, so
//!   that part is the associated function
//!   [`ModbusDriver::occupancy_hint`] instead.

use std::ops::RangeInclusive;
use std::sync::{Arc, OnceLock};

use banto_driver_api::{
    keys, AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor,
    DriverId, FieldError, Settings, TagKind,
};
use banto_plc::Address;
use banto_tags::ALLOWED_WORD_ORDERS;

use crate::PROTOCOL;

/// The Modbus TCP driver's [`DriverBase`] (a unit struct; the descriptor is a
/// process-wide constant).
#[derive(Debug, Clone, Copy, Default)]
pub struct ModbusDriver;

/// The driver as the registry stores it (`DriverEntry::session`, R2b-3a-2).
pub fn driver() -> Arc<dyn banto_driver_api::SessionDriver> {
    Arc::new(ModbusDriver)
}

/// The connection test's one point (R2b-3a-2, [`DriverBase::probe`]): the
/// literal the Hub's protocol-specific connection test used to read.
pub(crate) const PROBE: &str = "40001";

fn descriptor() -> &'static DriverDescriptor {
    static DESCRIPTOR: OnceLock<DriverDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| {
        DriverDescriptor::new(
            DriverId::new(PROTOCOL).expect("PROTOCOL is a valid driver id"),
            env!("CARGO_PKG_VERSION"),
            "Modbus TCP",
            Capabilities::READ_WRITE,
        )
    })
}

/// `tagAddressHelp.ts` `MODBUS_EXAMPLES`: `(address, description)`.
pub(crate) const EXAMPLES: [(&str, &str); 4] = [
    ("40001", "保持レジスタ（ワード・書き込み可）"),
    ("30001", "入力レジスタ（ワード・読み取り専用）"),
    ("00001", "コイル（ビット・書き込み可）"),
    ("10001", "ディスクリート入力（ビット・読み取り専用）"),
];

/// `tagAddressHelp.ts` `addressHelpFor('modbus-tcp', ..).placeholder`.
pub(crate) const PLACEHOLDER: &str = "40001（ビット指定: 40001.3）";

/// `tagAddressHelp.ts` `MODBUS_DEVICE_HINT`.
pub(crate) const DEVICE_HINT: &str = "先頭の桁がエリアを表します: 0xxxx=コイル（ビット）、1xxxx=ディスクリート入力（ビット・読み取り専用）、3xxxx=入力レジスタ（ワード・読み取り専用）、4xxxx=保持レジスタ（ワード）。通常は5桁、9999を超える番号は6桁表記（400001〜）を使います。";

/// `tagAddressHelp.ts` `MODBUS_BIT_HINT`.
pub(crate) const BIT_HINT: &str = "保持レジスタ／入力レジスタの特定ビットだけを読み書きするときは「40001.3」のように「.」+ビット位置（0〜15）を付けます（data_type = bit のタグでのみ使えます）。コイル／ディスクリート入力は既にビット単位なので「.」指定はできません。";

impl ModbusDriver {
    /// `tagAddressHelp.ts` `modbusOccupancyHint`: how many registers a tag of
    /// `kind` occupies, in words a person can read. The part of the
    /// TypeScript's `AddressHelp` that depends on the data type, which
    /// [`DriverBase::address_help`] cannot carry.
    pub fn occupancy_hint(kind: &TagKind) -> String {
        match kind {
            TagKind::Numeric(data_type) => match data_type {
                DataType::Bit => "bit 型はコイル（0xxxx）またはディスクリート入力（1xxxx）を1点指定します。",
                DataType::I32 | DataType::U32 | DataType::F32 => {
                    "32bit 型（i32/u32/f32）は入力レジスタ／保持レジスタ2点分を連続して占有します（例: 40001 なら 40001・40002）。"
                }
                DataType::I64 | DataType::U64 | DataType::F64 => {
                    "64bit 型（i64/u64/f64）は入力レジスタ／保持レジスタ4点分を連続して占有します（例: 40001 なら 40001〜40004）。"
                }
                DataType::I16 | DataType::U16 => "16bit 型（i16/u16）は入力レジスタ／保持レジスタ1点分を占有します。",
            },
            TagKind::Str { .. } => {
                "string 型は「文字列長（word数）」で指定した word 数（入力／保持レジスタ）を先頭アドレスから連続して占有します。"
            }
            // `TagKind` is `#[non_exhaustive]`: a kind added later reads as one
            // word until it gets its own line (the TypeScript's default).
            _ => "16bit 型（i16/u16）は入力レジスタ／保持レジスタ1点分を占有します。",
        }
        .to_owned()
    }
}

impl DriverBase for ModbusDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        descriptor()
    }

    /// `host` and `port` are required; `unit_id` (0..=255), `word_order`,
    /// the two timeouts (milliseconds) and `transport` (R1: `"tcp"`) are
    /// checked when present. Unknown keys are not an error.
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        let mut errors = Vec::new();
        require_text(settings, keys::HOST, &mut errors);
        check_int(settings, keys::PORT, 1..=65_535, true, &mut errors);
        check_int(settings, keys::UNIT_ID, 0..=255, false, &mut errors);
        check_word_order(settings, &mut errors);
        check_int(
            settings,
            keys::CONNECT_TIMEOUT_MS,
            1..=600_000,
            false,
            &mut errors,
        );
        check_int(
            settings,
            keys::RESPONSE_TIMEOUT_MS,
            1..=600_000,
            false,
            &mut errors,
        );
        check_transport(settings, &mut errors);
        errors
    }

    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        let address = Address::parse(text).map_err(|_| {
            AddressError::new(format!(
                "{:?} は Modbus の参照番号（例: 40001, 40001.3）として読めません",
                text.trim()
            ))
        })?;
        let (area, _offset, bit) = address
            .as_modbus_ref()
            .expect("Address::parse always yields a ModbusRef");
        check_kind(text, area, bit, kind)?;
        let driver_address = DriverAddress::new(
            descriptor().id.clone(),
            address.to_string(),
            area.to_string(),
            area.is_writable(),
            kind.word_count(),
        );
        Ok(match bit {
            Some(bit) => driver_address.with_bit(bit),
            None => driver_address,
        })
    }

    /// The reference number `n` tags after `canonical`: `n` times the
    /// kind's width in registers ([`TagKind::word_count`], at least 1;
    /// `incrementAddress`'s `step * index`), so continuous registration of
    /// `f32` tags uses `n = 0, 1, 2` and gets `+0, +2, +4`. Offset copy
    /// (`offsetCopy.ts`, a distance in words regardless of the tag's type)
    /// passes a one-word kind. `n` may be negative.
    ///
    /// Differences from the TypeScript (`incrementAddress`'s Modbus
    /// fallback, which is a textual `prefix + digits` increment):
    ///
    /// - The arithmetic is on the parsed 0-based offset, so the area never
    ///   changes and the result is bounded to `0..=65535` (the TypeScript
    ///   happily produced `50000` from `49999`, which is another area).
    /// - The result is the canonical 6-digit form (`400002`, not `40002`;
    ///   Q8 = A). The same address, written the way the registry will store
    ///   it.
    /// - A bit suffix (`40001.3`) is refused for `n != 0`, as the TypeScript
    ///   does (its regex does not match), instead of walking the bit axis the
    ///   way SLMP does. `n = 0` returns the canonical address unchanged, bit
    ///   suffix included (conformance check C1).
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        let address = Address::parse(canonical).map_err(|_| {
            AddressError::new(format!(
                "{:?} は Modbus の参照番号（例: 40001）として読めません",
                canonical.trim()
            ))
        })?;
        let (area, offset, bit) = address
            .as_modbus_ref()
            .expect("Address::parse always yields a ModbusRef");
        if n == 0 {
            return Ok(address.to_string());
        }
        if bit.is_some() {
            return Err(AddressError::new(format!(
                "ビット指定付きのアドレス {canonical:?} は連番にできません"
            )));
        }
        let step = i128::from(kind.word_count().max(1));
        let next = i128::from(offset) + step * i128::from(n);
        let next = u16::try_from(next).map_err(|_| {
            AddressError::new(format!(
                "アドレス {canonical:?} から {n} 点先のアドレスを算出できません（範囲外です）"
            ))
        })?;
        Ok(Address::ModbusRef {
            area,
            offset: next,
            bit: None,
        }
        .to_string())
    }

    /// `tagAddressHelp.ts`'s `modbus-tcp` help, minus the data-type-specific
    /// occupancy line ([`ModbusDriver::occupancy_hint`]): the placeholder,
    /// the examples, the area table and the bit-suffix rule.
    fn address_help(&self) -> String {
        let mut help = format!("アドレスの例: {PLACEHOLDER}\n");
        for (address, description) in EXAMPLES {
            help.push_str(&format!("  {address}: {description}\n"));
        }
        help.push('\n');
        help.push_str(DEVICE_HINT);
        help.push_str("\n\n");
        help.push_str(BIT_HINT);
        help
    }

    /// The connection test reads [`PROBE`] as a `u16` (R2b-3a-2).
    fn probe(&self) -> Option<(String, TagKind)> {
        Some((
            PROBE.to_string(),
            TagKind::numeric(banto_driver_api::DataType::U16),
        ))
    }
}

/// The kind-dependent refusals (address design §3.2, Q4): what is wrong with
/// this address *for this tag*, found at registration instead of as a Bad
/// reading every cycle. The combination test is `banto_plc::modbus_accepts`,
/// the very predicate `plan_requests` uses, so the two cannot drift; the
/// planner keeps it as a safety net.
fn check_kind(
    text: &str,
    area: banto_plc::AddressArea,
    bit: Option<u8>,
    kind: &TagKind,
) -> Result<(), AddressError> {
    let text = text.trim();
    let data_type = match kind {
        TagKind::Numeric(data_type) => *data_type,
        TagKind::Str { .. } => {
            return Err(AddressError::new(format!(
                "Modbus TCP は string 型のタグを扱えません（アドレス {text:?}）"
            )))
        }
        // `TagKind` is `#[non_exhaustive]`: a kind added later is refused
        // until this driver learns it.
        _ => {
            return Err(AddressError::new(format!(
                "Modbus TCP はこの種類のタグを扱えません（アドレス {text:?}）"
            )))
        }
    };
    if bit.is_some() && data_type != DataType::Bit {
        return Err(AddressError::new(format!(
            "ビット指定アドレス {text:?} はデータ型 bit のタグでのみ使えます（現在のデータ型: {data_type}）"
        )));
    }
    if !banto_plc::modbus_accepts(area, bit, data_type) {
        return Err(AddressError::new(format!(
            "アドレス {text:?}（{area}）にデータ型 {data_type} のタグは置けません"
        )));
    }
    Ok(())
}

fn require_text(settings: &Settings, key: &str, errors: &mut Vec<FieldError>) {
    match settings.get(key) {
        Some(value) if !value.trim().is_empty() => {}
        _ => errors.push(FieldError::new(key, format!("{key} を指定してください"))),
    }
}

fn check_int(
    settings: &Settings,
    key: &str,
    range: RangeInclusive<u32>,
    required: bool,
    errors: &mut Vec<FieldError>,
) {
    match settings.get(key) {
        None if required => errors.push(FieldError::new(key, format!("{key} を指定してください"))),
        None => {}
        Some(value) => match value.parse::<u32>() {
            Ok(n) if range.contains(&n) => {}
            _ => errors.push(FieldError::new(
                key,
                format!(
                    "{key} は {}〜{} の整数で指定してください",
                    range.start(),
                    range.end()
                ),
            )),
        },
    }
}

fn check_word_order(settings: &Settings, errors: &mut Vec<FieldError>) {
    if let Some(value) = settings.get(keys::WORD_ORDER) {
        if !ALLOWED_WORD_ORDERS.contains(&value) {
            errors.push(FieldError::new(
                keys::WORD_ORDER,
                format!(
                    "{} は {} のどちらかで指定してください",
                    keys::WORD_ORDER,
                    ALLOWED_WORD_ORDERS.join(" / ")
                ),
            ));
        }
    }
}

fn check_transport(settings: &Settings, errors: &mut Vec<FieldError>) {
    if let Some(value) = settings.get(keys::TRANSPORT) {
        if value != "tcp" {
            errors.push(FieldError::new(
                keys::TRANSPORT,
                "transport は tcp のみ対応しています",
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use banto_driver_api::{StringEncoding, Transport};

    use super::*;

    fn kind(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    fn one_word() -> TagKind {
        kind(DataType::I16)
    }

    fn offset(canonical: &str, kind: &TagKind, n: i64) -> Result<String, AddressError> {
        ModbusDriver.offset_address(canonical, kind, n)
    }

    // ------------------------------------------------------------------
    // identity
    // ------------------------------------------------------------------

    #[test]
    fn descriptor_is_modbus_tcp() {
        let d = ModbusDriver.descriptor();
        assert_eq!(d.id.as_str(), PROTOCOL);
        assert_eq!(d.id.as_str(), "modbus-tcp");
        assert_eq!(d.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(d.capabilities, Capabilities::READ_WRITE);
        assert_eq!(d.transports, vec![Transport::Tcp]);
        assert_eq!(d.api_version, banto_driver_api::API_VERSION);
        assert_eq!(driver().descriptor(), d);
        assert!(ModbusDriver.secret_keys().is_empty());
    }

    // ------------------------------------------------------------------
    // validate_config
    // ------------------------------------------------------------------

    fn minimal() -> Settings {
        Settings::from_pairs([("host", "192.168.0.10"), ("port", "502")]).unwrap()
    }

    #[test]
    fn minimal_and_full_settings_validate() {
        assert_eq!(ModbusDriver.validate_config(&minimal()), vec![]);
        let mut full = minimal();
        for (k, v) in [
            ("unit_id", "255"),
            ("word_order", "high_low"),
            ("connect_timeout_ms", "3000"),
            ("response_timeout_ms", "1000"),
            ("transport", "tcp"),
            ("cpu_type", "R"),
            ("x-unknown", "ignored"),
        ] {
            full.insert(k, v);
        }
        assert_eq!(ModbusDriver.validate_config(&full), vec![]);
    }

    #[test]
    fn empty_settings_report_host_and_port() {
        let keys: Vec<String> = ModbusDriver
            .validate_config(&Settings::new())
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, ["host", "port"]);
    }

    #[test]
    fn bad_values_are_reported_per_key() {
        let cases = [
            ("host", " ", "host"),
            ("port", "abc", "port"),
            ("port", "0", "port"),
            ("port", "65536", "port"),
            ("unit_id", "256", "unit_id"),
            ("unit_id", "-1", "unit_id"),
            ("word_order", "middle", "word_order"),
            ("connect_timeout_ms", "0", "connect_timeout_ms"),
            ("response_timeout_ms", "x", "response_timeout_ms"),
            ("transport", "serial", "transport"),
        ];
        for (key, value, reported) in cases {
            let mut s = minimal();
            s.insert(key, value);
            let errors = ModbusDriver.validate_config(&s);
            assert_eq!(errors.len(), 1, "{key}={value}: {errors:?}");
            assert_eq!(errors[0].key, reported, "{key}={value}");
        }
    }

    // ------------------------------------------------------------------
    // parse_address (tagAddressHelp.ts examples; address.rs grammar)
    // ------------------------------------------------------------------

    #[test]
    fn parse_address_covers_every_area_with_canonical_six_digits() {
        // (text, canonical, area, writable)
        let cases = [
            ("40001", "400001", "holding_register", true),
            ("30001", "300001", "input_register", false),
            ("00001", "000001", "coil", true),
            ("10001", "100001", "discrete_input", false),
            ("400001", "400001", "holding_register", true),
            (" 40001 ", "400001", "holding_register", true),
        ];
        for (text, canonical, area, writable) in cases {
            // Coils / discrete inputs hold `bit`, registers any other type.
            let k = if area == "coil" || area == "discrete_input" {
                kind(DataType::Bit)
            } else {
                kind(DataType::I16)
            };
            let a = ModbusDriver
                .parse_address(text, &k)
                .unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(a.driver.as_str(), "modbus-tcp");
            assert_eq!(a.canonical, canonical, "{text}");
            assert_eq!(a.area, area, "{text}");
            assert_eq!(a.writable_area, writable, "{text}");
            assert_eq!(a.bit, None);
            assert_eq!(a.canonical, Address::parse(text).unwrap().to_string());
            // Idempotent.
            let again = ModbusDriver.parse_address(&a.canonical, &k).unwrap();
            assert_eq!(again.canonical, a.canonical);
        }
    }

    #[test]
    fn parse_address_bit_and_word_count() {
        let a = ModbusDriver
            .parse_address("40001.3", &kind(DataType::Bit))
            .unwrap();
        assert_eq!(a.canonical, "400001.3");
        assert_eq!(a.bit, Some(3));
        assert_eq!(a.word_count, 1);
        assert_eq!(
            ModbusDriver
                .parse_address("40001.03", &kind(DataType::Bit))
                .unwrap()
                .canonical,
            "400001.3"
        );
        for (data_type, words) in [(DataType::I16, 1), (DataType::F32, 2), (DataType::U64, 4)] {
            assert_eq!(
                ModbusDriver
                    .parse_address("40001", &kind(data_type))
                    .unwrap()
                    .word_count,
                words
            );
        }
    }

    #[test]
    fn parse_address_refuses_other_notations() {
        for text in [
            "",
            "D100",
            "M50",
            "X1A",
            "20001",
            "40000",
            "4001",
            "40001.16",
            "00001.3",
            "40001.",
            "40001.3.4",
        ] {
            assert!(
                ModbusDriver.parse_address(text, &one_word()).is_err(),
                "{text:?} must be refused"
            );
        }
    }

    // ------------------------------------------------------------------
    // offset_address - fixtures from the TypeScript tests
    // ------------------------------------------------------------------

    /// `continuousRegistration.test.ts` "incrementAddress: 既存の10進連番は不変":
    /// `incrementAddress('40001', 1, 1) == '40002'`. The Rust result is the
    /// canonical form of the same address.
    #[test]
    fn offset_one_register_matches_the_ts_decimal_increment() {
        let got = offset("40001", &one_word(), 1).unwrap();
        assert_eq!(got, "400002");
        assert_eq!(
            Address::parse(&got).unwrap(),
            Address::parse("40002").unwrap()
        );
        assert_eq!(offset("400001", &one_word(), 1).unwrap(), "400002");
    }

    /// `continuousRegistration.test.ts` "Modbus 参照番号（40001）を開始アドレスに、
    /// i64 型3点で 40001/40005/40009 が生成される" (step 4, index 0..3).
    #[test]
    fn offset_steps_by_the_kinds_width_i64() {
        let rows: Vec<String> = (0..3)
            .map(|i| offset("40001", &kind(DataType::I64), i).unwrap())
            .collect();
        assert_eq!(rows, ["400001", "400005", "400009"]);
        for (got, ts) in rows.iter().zip(["40001", "40005", "40009"]) {
            assert_eq!(Address::parse(got).unwrap(), Address::parse(ts).unwrap());
        }
    }

    /// `addressIncrement` ("addressIncrement: 64bit 型は+4" and the 32 / 16 bit
    /// regression cases) and `string` (`Math.max(1, length)`).
    #[test]
    fn offset_step_per_kind_matches_address_increment() {
        let cases = [
            (kind(DataType::Bit), 1),
            (kind(DataType::I16), 1),
            (kind(DataType::U16), 1),
            (kind(DataType::I32), 2),
            (kind(DataType::U32), 2),
            (kind(DataType::F32), 2),
            (kind(DataType::I64), 4),
            (kind(DataType::U64), 4),
            (kind(DataType::F64), 4),
            (TagKind::string(16, StringEncoding::Utf8), 16),
            (TagKind::string(0, StringEncoding::Utf8), 1),
        ];
        for (kind, step) in cases {
            let got = offset("400101", &kind, 1).unwrap();
            assert_eq!(got, format!("{}", 400101 + step), "{kind:?}");
        }
    }

    /// `offsetCopy.test.ts` "Modbus 参照番号タグのデバイス名由来コピー":
    /// `40001` +100 words is `40101` (a distance in words: a one-word kind).
    #[test]
    fn offset_copy_of_a_modbus_reference_number() {
        let got = offset("40001", &one_word(), 100).unwrap();
        assert_eq!(got, "400101");
        assert_eq!(
            Address::parse(&got).unwrap(),
            Address::parse("40101").unwrap()
        );
    }

    #[test]
    fn offset_zero_is_the_canonical_form_and_negative_walks_back() {
        assert_eq!(offset("400001", &one_word(), 0).unwrap(), "400001");
        assert_eq!(offset("40001", &one_word(), 0).unwrap(), "400001");
        assert_eq!(offset("400101", &one_word(), -100).unwrap(), "400001");
        // Area is preserved.
        assert_eq!(offset("300010", &one_word(), 5).unwrap(), "300015");
        assert_eq!(offset("000010", &one_word(), 5).unwrap(), "000015");
        assert_eq!(offset("100010", &one_word(), -9).unwrap(), "100001");
    }

    /// Bounds the TypeScript did not have (`incrementAddress`'s Modbus
    /// fallback only refused a negative number).
    #[test]
    fn offset_outside_the_register_space_is_an_error() {
        assert!(offset("400001", &one_word(), -1).is_err());
        // 465536 is offset 65535, the last register.
        assert_eq!(offset("465535", &one_word(), 1).unwrap(), "465536");
        assert!(offset("465536", &one_word(), 1).is_err());
        assert!(offset("400001", &one_word(), i64::MAX).is_err());
        assert!(offset("400001", &one_word(), i64::MIN).is_err());
    }

    /// `incrementAddress` returns `null` for text its fallback regex does not
    /// match (`ZZZ`, and a bit suffix).
    #[test]
    fn offset_refuses_unparseable_and_bit_addresses() {
        for text in ["ZZZ", "D100", "", "400001.3"] {
            assert!(
                offset(text, &one_word(), 1).is_err(),
                "{text:?} must be refused"
            );
        }
    }

    /// Regression (conformance check C1): `n = 0` on a bit-suffixed canonical
    /// address returns it unchanged, while a non-zero step is still refused.
    #[test]
    fn offset_zero_accepts_a_bit_address_as_canonical() {
        let bit = kind(DataType::Bit);
        let canonical = ModbusDriver
            .parse_address("40001.3", &bit)
            .unwrap()
            .canonical;
        assert_eq!(canonical, "400001.3");
        assert_eq!(offset(&canonical, &bit, 0).unwrap(), canonical);
        assert!(offset(&canonical, &bit, 1).is_err());
    }

    // ------------------------------------------------------------------
    // address_help - tagAddressHelp.test.ts "addressHelpFor: modbus-tcp"
    // ------------------------------------------------------------------

    #[test]
    fn help_does_not_recommend_d100() {
        // "D100 を推奨例として表示しない"
        assert!(!ModbusDriver.address_help().contains("D100"));
        assert!(!PLACEHOLDER.contains("D100"));
        assert!(EXAMPLES.iter().all(|(a, _)| !a.contains("D100")));
    }

    #[test]
    fn help_has_the_reference_number_examples() {
        // "Modbus 参照番号の例を返す"
        let help = ModbusDriver.address_help();
        for (address, _) in EXAMPLES {
            assert!(help.contains(address), "{address}");
        }
        for want in ["40001", "30001", "00001"] {
            assert!(EXAMPLES.iter().any(|(a, _)| *a == want));
        }
    }

    #[test]
    fn help_has_the_area_table_and_the_bit_rule() {
        let help = ModbusDriver.address_help();
        // "対応エリア（0xxxx/1xxxx/3xxxx/4xxxx）のヒントを含む"
        assert!(DEVICE_HINT.contains("0xxxx"));
        assert!(DEVICE_HINT.contains("4xxxx"));
        // "bit 指定（.N）はレジスタのみ・コイル系は不可という書式ヒントを含む"
        assert!(BIT_HINT.contains('.'));
        assert!(BIT_HINT.contains("コイル"));
        assert!(help.contains(DEVICE_HINT));
        assert!(help.contains(BIT_HINT));
        assert!(help.contains(PLACEHOLDER));
        assert!(help.contains("保持レジスタ（ワード・書き込み可）"));
    }

    /// The TypeScript strings, character for character.
    #[test]
    fn help_texts_are_the_ts_strings() {
        assert_eq!(PLACEHOLDER, "40001（ビット指定: 40001.3）");
        assert_eq!(
            DEVICE_HINT,
            concat!(
                "先頭の桁がエリアを表します: 0xxxx=コイル（ビット）、1xxxx=ディスクリート入力（ビット・読み取り専用）、",
                "3xxxx=入力レジスタ（ワード・読み取り専用）、4xxxx=保持レジスタ（ワード）。通常は5桁、9999を超える番号は",
                "6桁表記（400001〜）を使います。"
            )
        );
        assert_eq!(
            BIT_HINT,
            concat!(
                "保持レジスタ／入力レジスタの特定ビットだけを読み書きするときは「40001.3」のように「.」+ビット位置（0〜15）を付けます",
                "（data_type = bit のタグでのみ使えます）。コイル／ディスクリート入力は既にビット単位なので「.」指定はできません。"
            )
        );
        assert_eq!(
            EXAMPLES,
            [
                ("40001", "保持レジスタ（ワード・書き込み可）"),
                ("30001", "入力レジスタ（ワード・読み取り専用）"),
                ("00001", "コイル（ビット・書き込み可）"),
                ("10001", "ディスクリート入力（ビット・読み取り専用）"),
            ]
        );
    }

    /// "bit 型: コイル/ディスクリート入力1点指定のヒント", "i16/u16:
    /// レジスタ1点分のヒント", "i32/u32/f32: レジスタ2点連続のヒント",
    /// "#325: i64/u64/f64: レジスタ4点連続のヒント", "string: 文字列長に応じた
    /// 占有のヒント".
    #[test]
    fn occupancy_hint_per_kind() {
        let hint = |k: TagKind| ModbusDriver::occupancy_hint(&k);
        assert!(hint(kind(DataType::Bit)).contains("コイル"));
        assert!(hint(kind(DataType::I16)).contains("1点"));
        assert!(hint(kind(DataType::U16)).contains("1点"));
        for dt in [DataType::I32, DataType::U32, DataType::F32] {
            assert!(hint(kind(dt)).contains("2点"), "{dt}");
        }
        for dt in [DataType::I64, DataType::U64, DataType::F64] {
            assert!(hint(kind(dt)).contains("4点"), "{dt}");
        }
        assert!(hint(TagKind::string(8, StringEncoding::Utf8)).contains("文字列長"));
        assert_eq!(
            hint(kind(DataType::I16)),
            "16bit 型（i16/u16）は入力レジスタ／保持レジスタ1点分を占有します。"
        );
        assert_eq!(
            hint(kind(DataType::Bit)),
            "bit 型はコイル（0xxxx）またはディスクリート入力（1xxxx）を1点指定します。"
        );
    }

    // ------------------------------------------------------------------
    // R2b-2a: canonical table, kind-dependent refusals, the bridge
    // (docs/banto-hub-driver-address-design.md §1.4, §3.2 Q4, §4.2)
    // ------------------------------------------------------------------

    /// `(text, kind, canonical)`: spellings of one address share a canonical
    /// form, parsing the canonical again is a no-op, and the bridge
    /// (`Address::from_driver_address`, kept for the collection plan until
    /// R2b-3a-3) returns the very `Address` `Address::parse(text)` gives.
    #[test]
    fn canonical_table_is_idempotent_unique_and_bridges_back() {
        let bit = || kind(DataType::Bit);
        let word = || kind(DataType::U16);
        let rows: [(&str, TagKind, &str); 9] = [
            ("40001", word(), "400001"),
            ("400001", word(), "400001"),
            (" 40001 ", word(), "400001"),
            ("40001.03", bit(), "400001.3"),
            ("40001.3", bit(), "400001.3"),
            ("065536", bit(), "065536"),
            ("00001", bit(), "000001"),
            ("30001", kind(DataType::F32), "300001"),
            ("465536", kind(DataType::I32), "465536"),
        ];
        for (text, k, canonical) in rows {
            let a = ModbusDriver
                .parse_address(text, &k)
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(a.canonical, canonical, "{text:?}");
            let again = ModbusDriver.parse_address(&a.canonical, &k).unwrap();
            assert_eq!(again, a, "{text:?}: parse(canonical) == parse(text)");
            assert_eq!(
                Address::from_driver_address(&a).unwrap(),
                Address::parse(text).unwrap(),
                "{text:?}: bridge"
            );
        }
    }

    #[test]
    fn a_driver_address_from_another_driver_is_refused_by_the_bridge() {
        let a = ModbusDriver.parse_address("40001", &one_word()).unwrap();
        let foreign = DriverAddress::new(
            DriverId::new("opc-ua").unwrap(),
            a.canonical.clone(),
            a.area.clone(),
            a.writable_area,
            a.word_count,
        );
        assert!(matches!(
            Address::from_driver_address(&foreign),
            Err(banto_plc::PlcError::UnsupportedDriver(d)) if d == "opc-ua"
        ));
    }

    #[test]
    fn a_bit_suffix_needs_the_bit_data_type() {
        for dt in [DataType::I16, DataType::U16, DataType::F32, DataType::U64] {
            let e = ModbusDriver
                .parse_address("40001.3", &kind(dt))
                .expect_err("bit suffix + non-bit");
            assert!(e.message().contains("ビット指定"), "{dt}: {}", e.message());
        }
        assert!(ModbusDriver
            .parse_address("40001.3", &kind(DataType::Bit))
            .is_ok());
        assert!(ModbusDriver
            .parse_address("30001.15", &kind(DataType::Bit))
            .is_ok());
    }

    /// The refusal set is exactly the planner's: for every (area, bit
    /// suffix, data type) `parse_address` agrees with `plan_requests`, which
    /// is the safety net behind it (the shared predicate
    /// `banto_plc::modbus_accepts` is the single table).
    #[test]
    fn refusals_match_the_planner_for_every_area_and_data_type() {
        use banto_plc::{plan_requests, ReadRequest};
        let types = [
            DataType::Bit,
            DataType::I16,
            DataType::U16,
            DataType::I32,
            DataType::U32,
            DataType::F32,
            DataType::I64,
            DataType::U64,
            DataType::F64,
        ];
        for text in ["00001", "10001", "30001", "40001", "30001.3", "40001.3"] {
            for dt in types {
                let parsed = ModbusDriver.parse_address(text, &kind(dt));
                let outcome = plan_requests(&[ReadRequest {
                    address: Address::parse(text).unwrap(),
                    data_type: dt,
                }]);
                let planner_ok = outcome.immediate_bad.is_empty();
                assert_eq!(parsed.is_ok(), planner_ok, "{text} + {dt}: {parsed:?}");
            }
        }
    }

    #[test]
    fn coil_with_f32_and_register_with_bit_are_refused() {
        assert!(ModbusDriver
            .parse_address("00001", &kind(DataType::F32))
            .is_err());
        assert!(ModbusDriver
            .parse_address("10001", &kind(DataType::U16))
            .is_err());
        assert!(ModbusDriver
            .parse_address("40001", &kind(DataType::Bit))
            .is_err());
        assert!(ModbusDriver
            .parse_address("00001", &kind(DataType::Bit))
            .is_ok());
        assert!(ModbusDriver
            .parse_address("40001", &kind(DataType::F64))
            .is_ok());
    }

    #[test]
    fn a_string_kind_is_refused() {
        let s = TagKind::string(16, StringEncoding::Utf8);
        let e = ModbusDriver.parse_address("40001", &s).unwrap_err();
        assert!(e.message().contains("string"), "{}", e.message());
    }
}
