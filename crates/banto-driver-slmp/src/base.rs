//! [`SlmpDriver`]: the `banto-driver-api` identity of MELSEC SLMP
//! (docs/banto-driver-api-design.md §4.1, R1-4b).
//!
//! The stateless half of the driver ([`DriverBase`]): who it is, whether a
//! connection's settings are valid (including the CPU type, #605), and the
//! address functions. Its I/O is the `SessionDriver` in `api_session.rs`
//! (registered as `DriverEntry::session` since R2b-3a-2). Since R2b-2
//! (docs/banto-hub-driver-address-design.md) the address functions are on
//! the production path: the save path of `banto-tags` (built with a
//! registry) and the Hub's plan, instant read and write paths call
//! `parse_address`; since R2b-3a-2 the broker carries the resulting
//! `TagSpec` to this driver's session as is, and since R2b-3a-3 so does the
//! collection plan (banto-hub and ChronoGazer; the public bridge back
//! to `banto_plc::Address` is gone). ChronoGazer keeps the legacy
//! rules for saving until R2b-7.
//!
//! ## Address functions
//!
//! - `parse_address` wraps [`banto_plc::Address::parse_slmp`] and reports
//!   `canonical = Address::to_string()` (`slmp_address::format`).
//! - `offset_address` is a port of `incrementAddress`
//!   (`apps/banto-hub/src/lib/banto/continuousRegistration.ts`), whose SLMP
//!   branch walks either the bit axis (`D100.E` -> `D100.F` -> `D101.0`) or
//!   the device-number axis (hexadecimal carries for `X1E` -> `X1F` ->
//!   `X20`). The TypeScript keeps its own copy of the device table
//!   (`slmpDeviceTable.ts`); here the Rust parser is the single source.
//! - `address_help` is a port of `tagAddressHelp.ts`'s `slmp` branch, text
//!   unchanged. The TypeScript also returns an *occupancy* hint that depends
//!   on the tag's data type; `address_help()` takes no data type, so that part
//!   is the associated function [`SlmpDriver::occupancy_hint`] instead.

use std::ops::RangeInclusive;
use std::sync::{Arc, OnceLock};

use banto_driver_api::{
    keys, AddressError, Capabilities, DataType, DriverAddress, DriverBase, DriverDescriptor,
    DriverId, FieldError, Settings, TagKind,
};
use banto_plc::slmp::address::MAX_DEVICE_NUMBER;
use banto_plc::{Address, SlmpAccess, SlmpDevice};
use banto_tags::{ALLOWED_CPU_TYPES, ALLOWED_WORD_ORDERS};

use crate::PROTOCOL;

/// The SLMP driver's [`DriverBase`] (a unit struct; the descriptor is a
/// process-wide constant).
#[derive(Debug, Clone, Copy, Default)]
pub struct SlmpDriver;

/// The driver as the registry stores it (`DriverEntry::session`, R2b-3a-2).
pub fn driver() -> Arc<dyn banto_driver_api::SessionDriver> {
    Arc::new(SlmpDriver)
}

/// The connection test's one point (R2b-3a-2, [`DriverBase::probe`]): the
/// literal the Hub's protocol-specific connection test used to read.
pub(crate) const PROBE: &str = "D0";

fn descriptor() -> &'static DriverDescriptor {
    static DESCRIPTOR: OnceLock<DriverDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| {
        DriverDescriptor::new(
            DriverId::new(PROTOCOL).expect("PROTOCOL is a valid driver id"),
            env!("CARGO_PKG_VERSION"),
            "MELSEC SLMP",
            Capabilities::READ_WRITE,
        )
    })
}

/// `tagAddressHelp.ts` `SLMP_EXAMPLES`: `(address, description)`.
pub(crate) const EXAMPLES: [(&str, &str); 4] = [
    ("D100", "データレジスタ（ワード）"),
    ("M50", "内部リレー（ビット）"),
    ("X1A", "入力リレー（ビット・16進）"),
    ("D100.5", "D100 の 5 ビット目（ビット指定・16進で0〜F）"),
];

/// `tagAddressHelp.ts` `addressHelpFor('slmp', ..).placeholder`.
pub(crate) const PLACEHOLDER: &str = "D100（ビット指定: D100.5）";

/// `tagAddressHelp.ts` `SLMP_DEVICE_HINT`.
pub(crate) const DEVICE_HINT: &str = "ビットデバイス（X/Y/M/L/F/V/B/S/TS/TC/SS/SC/CS/CC/SB/SM/DX/DY）とワードデバイス（D/W/Z/R/ZR/TN/SN/CN/SD/SW）があります。X/Y/B/W/SB/SW/DX/DY は16進数、それ以外のデバイス番号は10進数で書きます。";

/// `tagAddressHelp.ts` `SLMP_BIT_HINT`.
pub(crate) const BIT_HINT: &str = "ワードデバイスの特定ビットだけを読み書きするときは「D100.F」のように「.」+ビット位置（0〜F、16進。D100.F は bit 15）を付けます（data_type = bit のタグでのみ使えます）。ビットデバイス自体には「.」指定はできません。";

impl SlmpDriver {
    /// `tagAddressHelp.ts` `slmpOccupancyHint`: how many word devices a tag
    /// of `kind` occupies, in words a person can read. The part of the
    /// TypeScript's `AddressHelp` that depends on the data type, which
    /// [`DriverBase::address_help`] cannot carry.
    pub fn occupancy_hint(kind: &TagKind) -> String {
        match kind {
            TagKind::Numeric(data_type) => match data_type {
                DataType::Bit => "bit 型はビットデバイスのアドレスをそのまま1点指定します（例: M50）。",
                DataType::I32 | DataType::U32 | DataType::F32 => {
                    "32bit 型（i32/u32/f32）はワードデバイス2点分を連続して占有します（例: D100 なら D100・D101）。"
                }
                DataType::I64 | DataType::U64 | DataType::F64 => {
                    "64bit 型（i64/u64/f64）はワードデバイス4点分を連続して占有します（例: D100 なら D100〜D103）。"
                }
                DataType::I16 | DataType::U16 => "16bit 型（i16/u16）はワードデバイス1点分を占有します。",
            },
            TagKind::Str { .. } => {
                "string 型は「文字列長（word数）」で指定した word 数を先頭アドレスから連続して占有します。"
            }
            // `TagKind` is `#[non_exhaustive]`: a kind added later reads as one
            // word until it gets its own line (the TypeScript's default).
            _ => "16bit 型（i16/u16）はワードデバイス1点分を占有します。",
        }
        .to_owned()
    }
}

impl DriverBase for SlmpDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        descriptor()
    }

    /// `host` and `port` are required; `cpu_type` (`"R"` / `"Q"`, #605),
    /// `word_order`, the two timeouts (milliseconds) and `transport` (R1:
    /// `"tcp"`) are checked when present. `unit_id` is ignored (SLMP has no
    /// such byte) and unknown keys are not an error.
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        let mut errors = Vec::new();
        require_text(settings, keys::HOST, &mut errors);
        check_int(settings, keys::PORT, 1..=65_535, true, &mut errors);
        check_enum(settings, keys::WORD_ORDER, ALLOWED_WORD_ORDERS, &mut errors);
        check_enum(settings, keys::CPU_TYPE, ALLOWED_CPU_TYPES, &mut errors);
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
        let address = Address::parse_slmp(text).map_err(|_| {
            AddressError::new(format!(
                "{:?} は MELSEC のデバイス表記（例: D100, X1A, D100.5）として読めません",
                text.trim()
            ))
        })?;
        let (device, number, bit) = address
            .as_slmp()
            .expect("Address::parse_slmp always yields an Slmp address");
        check_kind(text, device, bit, kind)?;
        if let Some(message) = past_last_number(text.trim(), device, number, bit, kind) {
            return Err(AddressError::new(message));
        }
        // SLMP has no read-only device at the protocol level (a CPU's write
        // protection answers with an end code instead): every area is
        // writable as a protocol fact.
        let driver_address = DriverAddress::new(
            descriptor().id.clone(),
            address.to_string(),
            device.mnemonic(),
            true,
            kind.word_count(),
        );
        Ok(match bit {
            Some(bit) => driver_address.with_bit(bit),
            None => driver_address,
        })
    }

    /// The device `n` tags after `canonical`.
    ///
    /// - **Bit axis** (`canonical` has a `.N` suffix): the word number and
    ///   the bit are one number line of 16 bits per word, and `n` moves
    ///   along it one bit per step whatever the kind is (as the TypeScript:
    ///   `D100.E` +2 is `D101.0`).
    /// - **Device-number axis** (no suffix): `n` times the kind's width in
    ///   words ([`TagKind::word_count`], at least 1; `incrementAddress`'s
    ///   `step * index`), formatted in the device's own radix, so `X1E` +2
    ///   is `X20` and `W1FF` +1 is `W200`. Offset copy (`offsetCopy.ts`, a
    ///   distance in words regardless of the tag's type) passes a one-word
    ///   kind.
    ///
    /// The device number must stay within `0..=0xFFFFFF`. Unlike the
    /// TypeScript's `buildOffsetCopyRows`, a negative `n` is allowed (the
    /// "positive offsets only" rule is the copy panel's policy, not the
    /// address function's).
    fn offset_address(
        &self,
        canonical: &str,
        kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        let out_of_range = || {
            AddressError::new(format!(
                "アドレス {canonical:?} から {n} 点先のアドレスを算出できません（デバイス番号の範囲外です）"
            ))
        };
        let address = Address::parse_slmp(canonical).map_err(|_| {
            AddressError::new(format!(
                "{:?} は MELSEC のデバイス表記（例: D100, X1A, D100.5）として読めません",
                canonical.trim()
            ))
        })?;
        let (device, number, bit) = address
            .as_slmp()
            .expect("Address::parse_slmp always yields an Slmp address");

        let (next_number, next_bit) = match bit {
            Some(bit) => {
                let total = i128::from(number) * 16 + i128::from(bit) + i128::from(n);
                if total < 0 {
                    return Err(out_of_range());
                }
                (total / 16, Some((total % 16) as u8))
            }
            None => {
                let step = i128::from(kind.word_count().max(1));
                (i128::from(number) + step * i128::from(n), None)
            }
        };
        if next_number < 0 || next_number > i128::from(MAX_DEVICE_NUMBER) {
            return Err(out_of_range());
        }
        Ok(Address::Slmp {
            device,
            number: next_number as u32,
            bit: next_bit,
        }
        .to_string())
    }

    /// `tagAddressHelp.ts`'s `slmp` help, minus the data-type-specific
    /// occupancy line ([`SlmpDriver::occupancy_hint`]): the placeholder, the
    /// examples, the device table and the bit-suffix rule.
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
/// reading every cycle. The access / bit-position tests are
/// `banto_plc::slmp_accepts_numeric` / `slmp_accepts_string`, the predicates
/// `plan_slmp_batch` itself uses, so the two cannot drift; the planner keeps
/// them as a safety net. The 64-bit refusal is `banto-tags`'
/// `MODBUS_ONLY_DATA_TYPES` rule, which the host no longer applies by
/// protocol name.
fn check_kind(
    text: &str,
    device: SlmpDevice,
    bit: Option<u8>,
    kind: &TagKind,
) -> Result<(), AddressError> {
    let text = text.trim();
    let access = device.access();
    let access_label = if access == SlmpAccess::Bit {
        "ビットデバイス"
    } else {
        "ワードデバイス"
    };
    match kind {
        TagKind::Numeric(data_type) => {
            let data_type = *data_type;
            if matches!(data_type, DataType::I64 | DataType::U64 | DataType::F64) {
                return Err(AddressError::new(format!(
                    "SLMP は 64bit 型（i64/u64/f64）を扱えません（アドレス {text:?}、データ型: {data_type}）"
                )));
            }
            if bit.is_some() && data_type != DataType::Bit {
                return Err(AddressError::new(format!(
                    "ビット指定アドレス {text:?} はデータ型 bit のタグでのみ使えます（現在のデータ型: {data_type}）"
                )));
            }
            if !banto_plc::slmp_accepts_numeric(access, bit, data_type) {
                return Err(AddressError::new(format!(
                    "アドレス {text:?}（{device} は{access_label}）にデータ型 {data_type} のタグは置けません"
                )));
            }
        }
        TagKind::Str { .. } => {
            if !banto_plc::slmp_accepts_string(access, bit) {
                return Err(AddressError::new(format!(
                    "string 型のタグはワードデバイスのビット指定なしのアドレスにだけ置けます（アドレス {text:?}）"
                )));
            }
        }
        // `TagKind` is `#[non_exhaustive]`: a kind added later is refused
        // until this driver learns it.
        _ => {
            return Err(AddressError::new(format!(
                "SLMP はこの種類のタグを扱えません（アドレス {text:?}）"
            )))
        }
    }
    Ok(())
}

/// What a tag occupies from its address, in its device's own unit (#793 の
/// レビュー): a word device counts words, a bit device points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Occupancy {
    /// How many device numbers, from the tag's own.
    pub(crate) count: u32,
    /// Counted in points (a bit device) rather than words.
    pub(crate) points: bool,
}

impl Occupancy {
    /// The unit as the messages say it.
    pub(crate) fn unit(&self) -> &'static str {
        if self.points {
            "点"
        } else {
            "語"
        }
    }
}

/// - One bit of a word (`D100.5`): that one word.
/// - A bit device (`M50`): one point for a `bit` tag. Any other kind would
///   spread its words over 16 points each (`check_kind` and the planner
///   refuse those; the range check stays sound for them anyway).
/// - A word device: the kind's words (at least 1).
pub(crate) fn occupancy(device: SlmpDevice, bit: Option<u8>, kind: &TagKind) -> Occupancy {
    let words = u32::from(kind.word_count().max(1));
    match (bit, device.access()) {
        (Some(_), _) => Occupancy {
            count: 1,
            points: false,
        },
        (None, SlmpAccess::Bit) => Occupancy {
            count: if *kind == TagKind::Numeric(DataType::Bit) {
                1
            } else {
                words * 16
            },
            points: true,
        },
        (None, SlmpAccess::Word) => Occupancy {
            count: words,
            points: false,
        },
    }
}

/// #793 のレビュー: タグの占める最後の番号もデバイス番号の範囲
/// （`0..=MAX_DEVICE_NUMBER`）に入るか。入らなければその理由の文言
/// （`D16777215` の U32 は 2 語目が範囲の外）。保存時の `parse_address` と、
/// 保存を経ない spec への守りとして読み書きの入口（`api_session.rs` の
/// `past_last_number`、フレーム型の `plan_read` も通る）の両方がこれを使う。
/// 単位はデバイスの種類で「語」か「点」。
pub(crate) fn past_last_number(
    text: &str,
    device: SlmpDevice,
    number: u32,
    bit: Option<u8>,
    kind: &TagKind,
) -> Option<String> {
    let occupied = occupancy(device, bit, kind);
    if u64::from(number) + u64::from(occupied.count) - 1 <= u64::from(MAX_DEVICE_NUMBER) {
        return None;
    }
    Some(format!(
        "アドレス {text:?} から {} {}分を占めると、デバイス番号の上限 {MAX_DEVICE_NUMBER}（0x{MAX_DEVICE_NUMBER:X}）を超えます",
        occupied.count,
        occupied.unit()
    ))
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

fn check_enum(settings: &Settings, key: &str, allowed: &[&str], errors: &mut Vec<FieldError>) {
    if let Some(value) = settings.get(key) {
        if !allowed.contains(&value) {
            errors.push(FieldError::new(
                key,
                format!(
                    "{key} は {} のどちらかで指定してください",
                    allowed.join(" / ")
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
        SlmpDriver.offset_address(canonical, kind, n)
    }

    // ------------------------------------------------------------------
    // identity
    // ------------------------------------------------------------------

    #[test]
    fn descriptor_is_slmp() {
        let d = SlmpDriver.descriptor();
        assert_eq!(d.id.as_str(), PROTOCOL);
        assert_eq!(d.id.as_str(), "slmp");
        assert_eq!(d.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(d.capabilities, Capabilities::READ_WRITE);
        assert_eq!(d.transports, vec![Transport::Tcp]);
        assert_eq!(d.api_version, banto_driver_api::API_VERSION);
        assert_eq!(driver().descriptor(), d);
        assert!(SlmpDriver.secret_keys().is_empty());
    }

    // ------------------------------------------------------------------
    // validate_config
    // ------------------------------------------------------------------

    fn minimal() -> Settings {
        Settings::from_pairs([("host", "192.168.0.10"), ("port", "5000")]).unwrap()
    }

    #[test]
    fn minimal_and_full_settings_validate() {
        assert_eq!(SlmpDriver.validate_config(&minimal()), vec![]);
        let mut full = minimal();
        for (k, v) in [
            ("cpu_type", "Q"),
            ("word_order", "low_high"),
            ("connect_timeout_ms", "3000"),
            ("response_timeout_ms", "1000"),
            ("transport", "tcp"),
            // SLMP ignores the Modbus station, whatever it holds.
            ("unit_id", "1"),
            ("x-unknown", "ignored"),
        ] {
            full.insert(k, v);
        }
        assert_eq!(SlmpDriver.validate_config(&full), vec![]);
    }

    #[test]
    fn empty_settings_report_host_and_port() {
        let keys: Vec<String> = SlmpDriver
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
            ("cpu_type", "L", "cpu_type"),
            ("cpu_type", "r", "cpu_type"),
            ("word_order", "middle", "word_order"),
            ("connect_timeout_ms", "0", "connect_timeout_ms"),
            ("response_timeout_ms", "x", "response_timeout_ms"),
            ("transport", "serial", "transport"),
        ];
        for (key, value, reported) in cases {
            let mut s = minimal();
            s.insert(key, value);
            let errors = SlmpDriver.validate_config(&s);
            assert_eq!(errors.len(), 1, "{key}={value}: {errors:?}");
            assert_eq!(errors[0].key, reported, "{key}={value}");
        }
    }

    // ------------------------------------------------------------------
    // parse_address (tagAddressHelp.ts examples; slmp/address.rs grammar)
    // ------------------------------------------------------------------

    #[test]
    fn parse_address_examples() {
        // (text, canonical, area, bit)
        let cases = [
            ("D100", "D100", "D", None),
            ("d100", "D100", "D", None),
            ("M50", "M50", "M", None),
            ("X1A", "X1A", "X", None),
            ("x1a", "X1A", "X", None),
            ("D100.5", "D100.5", "D", Some(5)),
            ("D100.f", "D100.F", "D", Some(15)),
            ("ZR0", "ZR0", "ZR", None),
            ("W1FF", "W1FF", "W", None),
            (" SD10 ", "SD10", "SD", None),
        ];
        for (text, canonical, area, bit) in cases {
            // Bit devices and bit-in-word suffixes take `bit`, word devices
            // any other type (R2b-2a kind checks).
            let k = if bit.is_some() || matches!(area, "M" | "X") {
                kind(DataType::Bit)
            } else {
                kind(DataType::I16)
            };
            let a = SlmpDriver
                .parse_address(text, &k)
                .unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(a.driver.as_str(), "slmp");
            assert_eq!(a.canonical, canonical, "{text}");
            assert_eq!(a.area, area, "{text}");
            assert!(a.writable_area);
            assert_eq!(a.bit, bit, "{text}");
            assert_eq!(a.canonical, Address::parse_slmp(text).unwrap().to_string());
            let again = SlmpDriver.parse_address(&a.canonical, &k).unwrap();
            assert_eq!(again.canonical, a.canonical, "idempotent: {text}");
        }
    }

    #[test]
    fn parse_address_word_count_follows_the_kind() {
        for (data_type, text, words) in [
            (DataType::Bit, "M100", 1),
            (DataType::I16, "D100", 1),
            (DataType::F32, "D100", 2),
        ] {
            assert_eq!(
                SlmpDriver
                    .parse_address(text, &kind(data_type))
                    .unwrap()
                    .word_count,
                words
            );
        }
        let s = TagKind::string(16, StringEncoding::Utf8);
        assert_eq!(SlmpDriver.parse_address("D100", &s).unwrap().word_count, 16);
    }

    #[test]
    fn parse_address_refuses_other_notations() {
        // `continuousRegistration.test.ts` "M1A" (decimal device, hex digit)
        // and "ZZZ"; a Modbus reference number; bit suffix on a bit device
        // or in decimal two digits.
        for text in [
            "",
            "M1A",
            "ZZZ",
            "40001",
            "D",
            "D100.",
            "D100.10",
            "M50.0",
            "D100.G",
            "D16777216",
        ] {
            assert!(
                SlmpDriver.parse_address(text, &one_word()).is_err(),
                "{text:?} must be refused"
            );
        }
    }

    // ------------------------------------------------------------------
    // offset_address - fixtures from the TypeScript tests
    // ------------------------------------------------------------------

    /// `continuousRegistration.test.ts` "incrementAddress: 16進デバイスの桁上がり".
    #[test]
    fn offset_hex_devices_carry_in_hex() {
        let rows: Vec<String> = (0..3)
            .map(|i| offset("X1E", &kind(DataType::Bit), i).unwrap())
            .collect();
        assert_eq!(rows, ["X1E", "X1F", "X20"]);
        assert_eq!(offset("W1FF", &one_word(), 0).unwrap(), "W1FF");
        assert_eq!(offset("W1FF", &one_word(), 1).unwrap(), "W200");
    }

    /// `continuousRegistration.test.ts` "incrementAddress: ワード内 bit 連番"
    /// (and the `generateContinuousTags` case "D100.E を開始アドレスに、bit 型3点").
    #[test]
    fn offset_walks_the_bit_axis() {
        let rows: Vec<String> = (0..3)
            .map(|i| offset("D100.E", &kind(DataType::Bit), i).unwrap())
            .collect();
        assert_eq!(rows, ["D100.E", "D100.F", "D101.0"]);
        // The kind's width does not enter the bit axis (the TypeScript
        // ignores `step` there).
        assert_eq!(offset("D100.E", &kind(DataType::F64), 2).unwrap(), "D101.0");
    }

    /// `continuousRegistration.test.ts` "incrementAddress: 既存の10進連番は不変"
    /// (`D100` step 1 -> `D101`, step 2 -> `D102`).
    #[test]
    fn offset_decimal_devices_step_by_the_kind() {
        assert_eq!(offset("D100", &kind(DataType::I16), 1).unwrap(), "D101");
        assert_eq!(offset("D100", &kind(DataType::I32), 1).unwrap(), "D102");
        assert_eq!(offset("d100", &kind(DataType::I16), 1).unwrap(), "D101");
    }

    /// `continuousRegistration.test.ts` "generateContinuousTags: 64bit 型は
    /// アドレスが4ずつ増える": `D100` / `D104` / `D108` for `f64`.
    #[test]
    fn offset_steps_by_the_kinds_width_f64() {
        let rows: Vec<String> = (0..3)
            .map(|i| offset("D100", &kind(DataType::F64), i).unwrap())
            .collect();
        assert_eq!(rows, ["D100", "D104", "D108"]);
    }

    /// `addressIncrement` for every `DataType` and for `string`
    /// (`Math.max(1, length)`).
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
            assert_eq!(
                offset("D4000", &kind, 1).unwrap(),
                format!("D{}", 4000 + step),
                "{kind:?}"
            );
        }
    }

    /// `continuousRegistration.test.ts` "incrementAddress: 範囲外・不正な形式は
    /// エラー（null）".
    #[test]
    fn offset_respects_the_device_number_limit() {
        let at_max = "D16777215"; // MAX_DEVICE_NUMBER = 0x00FFFFFF
        assert_eq!(offset(at_max, &one_word(), 0).unwrap(), at_max);
        assert!(offset(at_max, &one_word(), 1).is_err());
        // Bit axis: 16 bits fit in the last word, the 17th does not.
        let bit = kind(DataType::Bit);
        assert_eq!(offset("D16777215.0", &bit, 15).unwrap(), "D16777215.F");
        assert!(offset("D16777215.0", &bit, 16).is_err());
        // Unparseable text.
        assert!(offset("M1A", &one_word(), 0).is_err());
        assert!(offset("ZZZ", &one_word(), 0).is_err());
        assert!(offset("", &one_word(), 0).is_err());
        assert!(offset("40001", &one_word(), 1).is_err());
    }

    /// `offsetCopy.test.ts` "buildOffsetCopyRows": the address of each copy
    /// (a distance in words: a one-word kind).
    #[test]
    fn offset_copy_addresses() {
        let w = one_word();
        // "デバイス名由来タグをオフセットコピーすると、新アドレス名で1行生成される"
        assert_eq!(offset("D3000", &w, 100).unwrap(), "D3100");
        // "複数選択（デバイス名由来＋意味名）を1回のバッチで処理できる"
        assert_eq!(offset("D3001", &w, 100).unwrap(), "D3101");
        // "#325: ... f64 ... +6 すると D3006" (the source is an f64, the
        // distance is still 6 words).
        assert_eq!(offset("D3000", &w, 6).unwrap(), "D3006");
        // "デバイス番号の上限を超えるオフセットはアドレスエラーになる"
        assert!(offset("D16777200", &w, 100).is_err());
        // The copy panel refuses 0 / negative / fractional offsets before
        // computing anything; the address function itself is total.
        assert_eq!(offset("D3000", &w, 0).unwrap(), "D3000");
        assert_eq!(offset("D10", &w, -1).unwrap(), "D9");
        assert!(offset("D10", &w, -100).is_err());
        assert!(offset("D10", &w, i64::MIN).is_err());
        assert!(offset("D10", &w, i64::MAX).is_err());
    }

    #[test]
    fn offset_zero_is_the_canonical_form() {
        for text in ["D100", "d100", "X1A", "D100.5", "ZR0", "W1FF"] {
            let k = if text.starts_with('X') || text.contains('.') {
                kind(DataType::Bit)
            } else {
                one_word()
            };
            let canonical = SlmpDriver.parse_address(text, &k).unwrap().canonical;
            assert_eq!(offset(&canonical, &k, 0).unwrap(), canonical);
        }
    }

    // ------------------------------------------------------------------
    // address_help - tagAddressHelp.test.ts "addressHelpFor: slmp"
    // ------------------------------------------------------------------

    #[test]
    fn help_has_the_d100_example_and_placeholder() {
        let help = SlmpDriver.address_help();
        // "SLMP デバイス表記の例を返す（D100 を含む）"
        assert!(EXAMPLES.iter().any(|(a, _)| *a == "D100"));
        assert!(!EXAMPLES.is_empty());
        // "プレースホルダは D100 系のビット指定込み"
        assert!(PLACEHOLDER.contains("D100"));
        assert!(help.contains(PLACEHOLDER));
        for (address, description) in EXAMPLES {
            assert!(help.contains(address));
            assert!(help.contains(description));
        }
    }

    #[test]
    fn help_has_the_device_table_and_the_bit_rule() {
        let help = SlmpDriver.address_help();
        // "対応デバイスのヒントに主要なビット/ワードデバイスを含む"
        for device in ["D", "M", "X"] {
            assert!(DEVICE_HINT.contains(device), "{device}");
        }
        // "bit 指定（.N）の書式ヒントを含む（16進 0〜F）"
        assert!(BIT_HINT.contains('.'));
        assert!(BIT_HINT.contains("0〜F"));
        assert!(help.contains(DEVICE_HINT));
        assert!(help.contains(BIT_HINT));
    }

    /// The TypeScript strings, character for character.
    #[test]
    fn help_texts_are_the_ts_strings() {
        assert_eq!(PLACEHOLDER, "D100（ビット指定: D100.5）");
        assert_eq!(
            DEVICE_HINT,
            concat!(
                "ビットデバイス（X/Y/M/L/F/V/B/S/TS/TC/SS/SC/CS/CC/SB/SM/DX/DY）とワードデバイス（D/W/Z/R/ZR/TN/SN/CN/SD/SW）があります。",
                "X/Y/B/W/SB/SW/DX/DY は16進数、それ以外のデバイス番号は10進数で書きます。"
            )
        );
        assert_eq!(
            BIT_HINT,
            concat!(
                "ワードデバイスの特定ビットだけを読み書きするときは「D100.F」のように「.」+ビット位置（0〜F、16進。",
                "D100.F は bit 15）を付けます（data_type = bit のタグでのみ使えます）。ビットデバイス自体には",
                "「.」指定はできません。"
            )
        );
        assert_eq!(
            EXAMPLES,
            [
                ("D100", "データレジスタ（ワード）"),
                ("M50", "内部リレー（ビット）"),
                ("X1A", "入力リレー（ビット・16進）"),
                ("D100.5", "D100 の 5 ビット目（ビット指定・16進で0〜F）"),
            ]
        );
    }

    /// "bit 型: ビットデバイス1点指定のヒント", "i16/u16: ワード1点分のヒント",
    /// "i32/u32/f32: ワード2点連続のヒント", "#325: i64/u64/f64: ワード4点連続の
    /// ヒント", "string: 文字列長に応じた占有のヒント".
    #[test]
    fn occupancy_hint_per_kind() {
        let hint = |k: TagKind| SlmpDriver::occupancy_hint(&k);
        assert!(hint(kind(DataType::Bit)).contains("ビットデバイス"));
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
            "16bit 型（i16/u16）はワードデバイス1点分を占有します。"
        );
        assert_eq!(
            hint(kind(DataType::Bit)),
            "bit 型はビットデバイスのアドレスをそのまま1点指定します（例: M50）。"
        );
    }

    // ------------------------------------------------------------------
    // R2b-2a: canonical table, kind-dependent refusals, the bridge
    // (docs/banto-hub-driver-address-design.md §1.4, §3.2 Q4, §4.2)
    // ------------------------------------------------------------------

    #[test]
    fn canonical_table_is_idempotent_unique_and_bridges_back() {
        let bit = || kind(DataType::Bit);
        let word = || kind(DataType::U16);
        let rows: [(&str, TagKind, &str); 9] = [
            ("d100", word(), "D100"),
            ("D0100", word(), "D100"),
            ("D100", word(), "D100"),
            (" D100 ", word(), "D100"),
            ("X0010", bit(), "X10"),
            ("x1a", bit(), "X1A"),
            ("d100.a", bit(), "D100.A"),
            ("D100.A", bit(), "D100.A"),
            ("ZR5", kind(DataType::F32), "ZR5"),
        ];
        for (text, k, canonical) in rows {
            let a = SlmpDriver
                .parse_address(text, &k)
                .unwrap_or_else(|e| panic!("{text:?}: {e}"));
            assert_eq!(a.canonical, canonical, "{text:?}");
            let again = SlmpDriver.parse_address(&a.canonical, &k).unwrap();
            assert_eq!(again, a, "{text:?}: parse(canonical) == parse(text)");
            assert_eq!(
                Address::parse_slmp(&a.canonical).unwrap(),
                Address::parse_slmp(text).unwrap(),
                "{text:?}: bridge"
            );
        }
    }

    /// #793 のレビュー: タグの占める最後の番号が上限 0xFFFFFF を超えるものは
    /// 保存で断る。上限ちょうどで終わるものは通す。
    #[test]
    fn a_tag_running_past_the_last_device_number_is_refused() {
        let sjis = |words| TagKind::string(words, StringEncoding::ShiftJis);
        for (text, k) in [
            ("D16777215", kind(DataType::U32)),
            ("D16777215", kind(DataType::I32)),
            ("ZR16777215", kind(DataType::F32)),
            ("D16777215", sjis(2)),
            ("ZR16777214", sjis(3)),
        ] {
            let err = SlmpDriver.parse_address(text, &k).unwrap_err();
            assert!(err.to_string().contains("上限"), "{text} {k:?}: {err}");
        }
        for (text, k) in [
            ("D16777215", kind(DataType::U16)),
            ("D16777214", kind(DataType::U32)),
            ("ZR16777214", sjis(2)),
            ("D16777215.F", kind(DataType::Bit)),
            ("M16777215", kind(DataType::Bit)),
        ] {
            assert!(SlmpDriver.parse_address(text, &k).is_ok(), "{text} {k:?}");
        }
        assert!(SlmpDriver
            .parse_address("BFFFFFF", &kind(DataType::Bit))
            .is_ok());
    }

    /// #793 の 3 回目: 上限をはみ出す文言の単位はデバイスの種類で決まる。
    /// ワードデバイスは「語」、ビットデバイスは「点」、ワード内のビットは 1 語。
    #[test]
    fn the_overrun_message_counts_words_or_points_by_the_device() {
        use banto_plc::SlmpDevice;
        let msg = |device, number, bit, k: TagKind| {
            past_last_number("x", device, number, bit, &k).expect("runs past the limit")
        };
        let max = MAX_DEVICE_NUMBER;
        let m = msg(SlmpDevice::D, max, None, kind(DataType::U32));
        assert!(m.contains("2 語分を占めると"), "{m}");
        let m = msg(
            SlmpDevice::ZR,
            max - 1,
            None,
            TagKind::string(3, StringEncoding::ShiftJis),
        );
        assert!(m.contains("3 語分を占めると"), "{m}");
        for device in [SlmpDevice::M, SlmpDevice::X, SlmpDevice::Y, SlmpDevice::B] {
            let m = msg(device, max, None, kind(DataType::U16));
            assert!(
                m.contains("16 点分を占めると") && !m.contains("語"),
                "{device}: {m}"
            );
            let m = msg(device, max - 20, None, kind(DataType::U32));
            assert!(m.contains("32 点分を占めると"), "{device}: {m}");
        }
        // What ends at the limit has no message, in either unit.
        assert!(past_last_number("x", SlmpDevice::M, max, None, &kind(DataType::Bit)).is_none());
        assert!(
            past_last_number("x", SlmpDevice::D, max, Some(15), &kind(DataType::Bit)).is_none()
        );
        assert!(
            past_last_number("x", SlmpDevice::D, max - 1, None, &kind(DataType::U32)).is_none()
        );
        assert!(
            past_last_number("x", SlmpDevice::M, max - 15, None, &kind(DataType::U16)).is_none()
        );
        // The save path says the same (after `AddressError`'s prefix).
        let err = SlmpDriver
            .parse_address("D16777215", &kind(DataType::U32))
            .unwrap_err();
        let want =
            past_last_number("D16777215", SlmpDevice::D, max, None, &kind(DataType::U32)).unwrap();
        assert!(err.to_string().ends_with(&want), "{err}");
    }

    #[test]
    fn sixty_four_bit_types_are_refused() {
        for dt in [DataType::I64, DataType::U64, DataType::F64] {
            let e = SlmpDriver
                .parse_address("D100", &kind(dt))
                .expect_err("64-bit on SLMP");
            assert!(e.message().contains("64bit"), "{dt}: {}", e.message());
        }
        assert!(SlmpDriver
            .parse_address("D100", &kind(DataType::F32))
            .is_ok());
    }

    #[test]
    fn a_bit_suffix_needs_the_bit_data_type() {
        for dt in [DataType::I16, DataType::U16, DataType::F32] {
            let e = SlmpDriver
                .parse_address("D100.5", &kind(dt))
                .expect_err("bit suffix + non-bit");
            assert!(e.message().contains("ビット指定"), "{dt}: {}", e.message());
        }
        assert!(SlmpDriver
            .parse_address("D100.5", &kind(DataType::Bit))
            .is_ok());
    }

    #[test]
    fn strings_live_in_word_devices_without_a_bit_suffix() {
        let s = TagKind::string(8, StringEncoding::ShiftJis);
        assert!(SlmpDriver.parse_address("D100", &s).is_ok());
        assert!(SlmpDriver.parse_address("M10", &s).is_err());
        assert!(SlmpDriver.parse_address("D100.5", &s).is_err());
    }

    /// The refusal set is exactly the planner's: for every device with each
    /// access, with and without a bit suffix, and every data type
    /// (including `string`), `parse_address` agrees with `plan_slmp_batch`
    /// (apart from the 64-bit rule, which is SLMP-only and checked above).
    #[test]
    fn refusals_match_the_planner_for_every_access_and_data_type() {
        use banto_plc::{
            plan_slmp_batch, BatchReadRequest, ReadRequest, StringEncoding as PlcEncoding,
            StringReadRequest,
        };
        let types = [
            DataType::Bit,
            DataType::I16,
            DataType::U16,
            DataType::I32,
            DataType::U32,
            DataType::F32,
        ];
        for text in ["D100", "M10", "D100.5", "W1F", "X1A"] {
            let address = Address::parse_slmp(text).unwrap();
            for dt in types {
                let parsed = SlmpDriver.parse_address(text, &kind(dt));
                let outcome = plan_slmp_batch(&[BatchReadRequest::Numeric(ReadRequest {
                    address,
                    data_type: dt,
                })]);
                assert_eq!(
                    parsed.is_ok(),
                    outcome.immediate_bad.is_empty(),
                    "{text} + {dt}: {parsed:?}"
                );
            }
            let parsed = SlmpDriver.parse_address(text, &TagKind::string(4, StringEncoding::Utf8));
            let outcome = plan_slmp_batch(&[BatchReadRequest::String(StringReadRequest {
                address,
                words: 4,
                encoding: PlcEncoding::Utf8,
            })]);
            assert_eq!(
                parsed.is_ok(),
                outcome.immediate_bad.is_empty(),
                "{text} + string: {parsed:?}"
            );
        }
    }
}
