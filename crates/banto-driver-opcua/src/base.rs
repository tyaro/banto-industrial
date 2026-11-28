//! [`OpcUaDriver`]: the `banto-driver-api` identity of the OPC UA client
//! (docs/chronogazer/opcua-client-design.md §3.2, §3.3).
//!
//! The stateless half of the driver ([`DriverBase`]): who it is, whether a
//! connection's settings are valid, and the address functions. Its I/O is
//! the [`SessionDriver`] in `session.rs`.

use std::num::NonZeroU64;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use banto_driver_api::{
    AddressError, Capabilities, ChoiceOption, DataType, DriverAddress, DriverBase,
    DriverDescriptor, DriverId, FieldError, FieldKind, KindName, SettingField, Settings,
    SettingsSchema, TagKind,
};
use banto_opcua_core::{NodeAddress, NodeAddressError};

use crate::PROTOCOL;

/// The setting keys this driver reads (design §3.3). camelCase like the
/// design's table; the names follow `banto_opcua_client::ClientConfig`
/// (`keep_alive_interval`, `max_failed_keep_alive_count`).
pub mod keys {
    /// The endpoint URL, `opc.tcp://host:port/path`. Required.
    pub const ENDPOINT_URL: &str = "endpointUrl";
    /// How to read: `read` (cyclic Read). `subscribe` (design §3.7) is not
    /// supported yet and is refused by `validate_config`.
    pub const READ_MODE: &str = "readMode";
    /// The keep-alive Read's interval, in milliseconds.
    pub const KEEP_ALIVE_INTERVAL_MS: &str = "keepAliveIntervalMs";
    /// How many keep-alives in a row may fail before the component closes
    /// the connection (a silent loss, ADR-0005).
    pub const MAX_FAILED_KEEP_ALIVE_COUNT: &str = "maxFailedKeepAliveCount";
}

/// `readMode`'s only accepted value.
pub(crate) const READ_MODE_READ: &str = "read";
/// `readMode`'s value for subscriptions (design §3.7), refused for now.
pub(crate) const READ_MODE_SUBSCRIBE: &str = "subscribe";

/// `ClientConfig::new`'s `keep_alive_interval` (5 s). A test pins the two
/// together.
pub(crate) const DEFAULT_KEEP_ALIVE_INTERVAL_MS: u32 = 5_000;
/// `ClientConfig::new`'s `max_failed_keep_alive_count` (3).
pub(crate) const DEFAULT_MAX_FAILED_KEEP_ALIVE_COUNT: u32 = 3;
/// The keep-alive interval's range. The upper end stays below the session
/// timeout async-opcua requests (60 s), which the component's doc requires.
const KEEP_ALIVE_INTERVAL_MS_RANGE: (u32, u32) = (100, 30_000);
const MAX_FAILED_KEEP_ALIVE_COUNT_RANGE: (u32, u32) = (1, 10);

/// The connection test's one point (design §3.2): `Server/ServerStatus/State`,
/// present on every server. It is an Int32 enumeration (`Running` = 0), so it
/// reads as a number (checked against banto-opcua-server in
/// `tests/server.rs`).
pub(crate) const PROBE: &str = "ns=0;i=2259";

/// The OPC UA client driver's [`DriverBase`] and [`SessionDriver`] (a unit
/// struct; the descriptor is a process-wide constant).
///
/// [`SessionDriver`]: banto_driver_api::SessionDriver
#[derive(Debug, Clone, Copy, Default)]
pub struct OpcUaDriver;

/// The driver as a registry will store it (`DriverEntry::session`).
pub fn driver() -> Arc<dyn banto_driver_api::SessionDriver> {
    Arc::new(OpcUaDriver)
}

fn descriptor() -> &'static DriverDescriptor {
    static DESCRIPTOR: OnceLock<DriverDescriptor> = OnceLock::new();
    DESCRIPTOR.get_or_init(|| {
        DriverDescriptor::new(
            DriverId::new(PROTOCOL).expect("PROTOCOL is a valid driver id"),
            env!("CARGO_PKG_VERSION"),
            "OPC UA（クライアント）",
            Capabilities::READ_WRITE,
        )
    })
}

/// A connection's settings, checked (what `connect` uses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientSettings {
    pub(crate) endpoint_url: String,
    pub(crate) keep_alive_interval: Duration,
    pub(crate) max_failed_keep_alive_count: NonZeroU64,
}

impl ClientSettings {
    /// Check `settings`; every problem is reported on its key.
    pub(crate) fn from_settings(settings: &Settings) -> Result<Self, Vec<FieldError>> {
        let mut errors = Vec::new();
        let endpoint_url = match settings.get(keys::ENDPOINT_URL).map(str::trim) {
            None | Some("") => {
                errors.push(FieldError::new(
                    keys::ENDPOINT_URL,
                    "endpointUrl（opc.tcp://ホスト:ポート/パス）を指定してください",
                ));
                String::new()
            }
            Some(url) => {
                if let Err(message) = check_endpoint_url(url) {
                    errors.push(FieldError::new(keys::ENDPOINT_URL, message));
                }
                url.to_owned()
            }
        };
        match settings.get(keys::READ_MODE).map(str::trim) {
            None | Some(READ_MODE_READ) => {}
            Some(READ_MODE_SUBSCRIBE) => errors.push(FieldError::new(
                keys::READ_MODE,
                "購読（subscribe）にはまだ対応していません。read（周期 Read）を指定してください",
            )),
            Some(_) => errors.push(FieldError::new(
                keys::READ_MODE,
                "readMode は read を指定してください",
            )),
        }
        let keep_alive = int(
            settings,
            keys::KEEP_ALIVE_INTERVAL_MS,
            DEFAULT_KEEP_ALIVE_INTERVAL_MS,
            KEEP_ALIVE_INTERVAL_MS_RANGE,
            &mut errors,
        );
        let failures = int(
            settings,
            keys::MAX_FAILED_KEEP_ALIVE_COUNT,
            DEFAULT_MAX_FAILED_KEEP_ALIVE_COUNT,
            MAX_FAILED_KEEP_ALIVE_COUNT_RANGE,
            &mut errors,
        );
        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(ClientSettings {
            endpoint_url,
            keep_alive_interval: Duration::from_millis(u64::from(keep_alive)),
            max_failed_keep_alive_count: NonZeroU64::new(u64::from(failures))
                .expect("the range starts at 1"),
        })
    }
}

/// `opc.tcp://` followed by a host, and no white space.
fn check_endpoint_url(url: &str) -> Result<(), String> {
    const SCHEME: &str = "opc.tcp://";
    let rest = url
        .get(..SCHEME.len())
        .filter(|scheme| scheme.eq_ignore_ascii_case(SCHEME))
        .map(|_| &url[SCHEME.len()..])
        .ok_or_else(|| "endpointUrl は opc.tcp:// で始めてください".to_owned())?;
    if url.chars().any(char::is_whitespace) {
        return Err("endpointUrl に空白を含めないでください".to_owned());
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.starts_with(':') {
        return Err(
            "endpointUrl にホスト名を書いてください（opc.tcp://ホスト:ポート/パス）".to_owned(),
        );
    }
    Ok(())
}

/// An optional integer setting in `min..=max`, `default` when absent.
fn int(
    settings: &Settings,
    key: &str,
    default: u32,
    (min, max): (u32, u32),
    errors: &mut Vec<FieldError>,
) -> u32 {
    match settings.get(key).map(str::trim) {
        None => default,
        Some(text) => match text.parse::<u32>() {
            Ok(n) if (min..=max).contains(&n) => n,
            _ => {
                errors.push(FieldError::new(
                    key,
                    format!("{key} は {min}〜{max} の整数で指定してください"),
                ));
                default
            }
        },
    }
}

/// Every kind this driver reads and writes: bit and every number.
const KINDS: [DataType; 9] = [
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

/// Parse `text` as a NodeId string, with this driver's error messages.
pub(crate) fn parse_node(text: &str) -> Result<NodeAddress, AddressError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(AddressError::new(
            "NodeId を入力してください（例: nsu=urn:banto:opcua:tags;s=PLC1.Line1.Speed）",
        ));
    }
    NodeAddress::from_str(text).map_err(|e| match e {
        NodeAddressError::UnsupportedIdentifierType(_) => {
            AddressError::new(format!("未対応の識別子の型です: {e}"))
        }
        _ => AddressError::new(format!("{text:?} は NodeId として読めません: {e}")),
    })
}

/// Why a string tag is refused (design §3.4: the mapping of an OPC UA String
/// onto `TagKind::Str { words, encoding }` is decided in #678).
pub(crate) const STRING_REFUSED: &str =
    "OPC UA のドライバは string 型にまだ対応していません（#678 で決めます）";

/// Why an address has no neighbour.
const NO_OFFSET: &str =
    "OPC UA の NodeId には「隣」が無いので、連続登録・オフセットのコピーはできません";

const HELP: &str = "\
アドレスは OPC UA の NodeId の文字列で書きます。
  nsu=<名前空間 URI>;s=<文字列の識別子>  例: nsu=urn:banto:opcua:tags;s=PLC1.Line1.Speed
  nsu=<名前空間 URI>;i=<数値の識別子>    例: nsu=urn:example:plant;i=1001
  ns=<名前空間の番号>;s=<文字列の識別子>  例: ns=2;s=Line1.Speed
  ns=<名前空間の番号>;i=<数値の識別子>    例: ns=0;i=2259（ns=0; は省略可）

ns= の番号はサーバーの再起動で変わることがあるので、nsu=（名前空間の URI）で書くことを勧めます。
URI の中の「;」は %3B、「%」は %25 と書きます。
Hub の OPC UA につなぐときは nsu=urn:banto:opcua:tags;s=<外部名>（Hub の名前空間の URI を変えていなければ）。
GUID（g=）と ByteString（b=）の識別子にはまだ対応していません。NodeId には「隣」が無いので、連続登録はできません。";

impl DriverBase for OpcUaDriver {
    fn descriptor(&self) -> &DriverDescriptor {
        descriptor()
    }

    /// `endpointUrl` is required; `readMode` (only `read`) and the two
    /// keep-alive settings are checked when present. Unknown keys are not an
    /// error.
    fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
        ClientSettings::from_settings(settings)
            .err()
            .unwrap_or_default()
    }

    /// A NodeId string (`banto-opcua-core`'s `NodeAddress`); the canonical
    /// form is its `Display` (`ns=0;` is never left out, `nsu=` re-encodes
    /// `;` and `%`). Every numeric kind is accepted at every NodeId: whether
    /// the node's type fits is only known when it is read or written
    /// (a write of another type is `BadTypeMismatch`). Strings are
    /// refused ([`STRING_REFUSED`]).
    fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
        match kind {
            TagKind::Numeric(_) => {}
            _ => return Err(AddressError::new(STRING_REFUSED)),
        }
        let node = parse_node(text)?;
        Ok(DriverAddress::new(
            descriptor().id.clone(),
            node.to_string(),
            "node",
            true,
            kind.word_count(),
        ))
    }

    /// Only `n = 0` (the canonical form itself): a NodeId has no neighbour,
    /// so continuous registration is refused (design §3.2).
    fn offset_address(
        &self,
        canonical: &str,
        _kind: &TagKind,
        n: i64,
    ) -> Result<String, AddressError> {
        let node = parse_node(canonical)?;
        if n != 0 {
            return Err(AddressError::new(NO_OFFSET));
        }
        Ok(node.to_string())
    }

    fn address_help(&self) -> String {
        HELP.to_owned()
    }

    /// [`PROBE`] as an `i32` (ServerStatus/State, an Int32 enumeration).
    fn probe(&self) -> Option<(String, TagKind)> {
        Some((PROBE.to_owned(), TagKind::numeric(DataType::I32)))
    }

    fn settings_schema(&self) -> SettingsSchema {
        let (ka_min, ka_max) = KEEP_ALIVE_INTERVAL_MS_RANGE;
        let (fail_min, fail_max) = MAX_FAILED_KEEP_ALIVE_COUNT_RANGE;
        SettingsSchema::new(vec![
            SettingField::new(keys::ENDPOINT_URL, "エンドポイント URL", FieldKind::text())
                .required()
                .help(
                    "opc.tcp://ホスト:ポート/パス。今は SecurityPolicy None・Anonymous（平文・匿名）でつなぎます。",
                ),
            SettingField::new(
                keys::READ_MODE,
                "読み方",
                FieldKind::choice(vec![ChoiceOption::new(READ_MODE_READ, "周期 Read")]),
            )
            .default(READ_MODE_READ)
            .help("収集の周期ごとに Read します。購読（subscribe）は後で足します。"),
            SettingField::new(
                keys::KEEP_ALIVE_INTERVAL_MS,
                "keep-alive の間隔（ミリ秒）",
                FieldKind::integer(i64::from(ka_min), i64::from(ka_max)),
            )
            .default(i64::from(DEFAULT_KEEP_ALIVE_INTERVAL_MS))
            .help("TCP が閉じないまま応答が止まる切断を見つけるための Read の間隔。")
            .advanced(),
            SettingField::new(
                keys::MAX_FAILED_KEEP_ALIVE_COUNT,
                "keep-alive の失敗の回数",
                FieldKind::integer(i64::from(fail_min), i64::from(fail_max)),
            )
            .default(i64::from(DEFAULT_MAX_FAILED_KEEP_ALIVE_COUNT))
            .help("keep-alive が続けてこの回数失敗したら、接続を切って張り直します。")
            .advanced(),
        ])
    }

    /// Bits and every number, 64-bit ones included; no strings yet
    /// ([`STRING_REFUSED`]).
    fn supported_kinds(&self) -> Vec<KindName> {
        KINDS.into_iter().map(KindName::Numeric).collect()
    }

    /// A NodeId has no word-wise neighbour either.
    fn offset_words(&self, _canonical: &str, _n: i64) -> Result<String, AddressError> {
        Err(AddressError::new(NO_OFFSET))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_driver_api::{SettingValue, StringEncoding};
    use banto_opcua_client::ClientConfig;

    fn settings(pairs: &[(&str, &str)]) -> Settings {
        Settings::from_pairs(pairs.iter().copied()).unwrap()
    }

    fn keys_of(errors: &[FieldError]) -> Vec<&str> {
        errors.iter().map(|e| e.key.as_str()).collect()
    }

    fn n(data_type: DataType) -> TagKind {
        TagKind::numeric(data_type)
    }

    #[test]
    fn defaults_are_the_components() {
        let config = ClientConfig::new("opc.tcp://127.0.0.1:4840/");
        assert_eq!(
            config.keep_alive_interval,
            Duration::from_millis(u64::from(DEFAULT_KEEP_ALIVE_INTERVAL_MS))
        );
        assert_eq!(
            config.max_failed_keep_alive_count.get(),
            u64::from(DEFAULT_MAX_FAILED_KEEP_ALIVE_COUNT)
        );
    }

    #[test]
    fn minimal_settings_validate_and_take_the_defaults() {
        let s = settings(&[(keys::ENDPOINT_URL, "opc.tcp://127.0.0.1:4840/")]);
        assert_eq!(OpcUaDriver.validate_config(&s), vec![]);
        let parsed = ClientSettings::from_settings(&s).unwrap();
        assert_eq!(parsed.endpoint_url, "opc.tcp://127.0.0.1:4840/");
        assert_eq!(parsed.keep_alive_interval, Duration::from_secs(5));
        assert_eq!(parsed.max_failed_keep_alive_count.get(), 3);
        // Unknown keys (host / port the conformance harness adds) are fine.
        let s = settings(&[
            (keys::ENDPOINT_URL, "opc.tcp://plc.local:4840"),
            ("host", "127.0.0.1"),
            ("port", "1"),
            (keys::READ_MODE, "read"),
            (keys::KEEP_ALIVE_INTERVAL_MS, "1000"),
            (keys::MAX_FAILED_KEEP_ALIVE_COUNT, "1"),
        ]);
        assert_eq!(OpcUaDriver.validate_config(&s), vec![]);
        let parsed = ClientSettings::from_settings(&s).unwrap();
        assert_eq!(parsed.keep_alive_interval, Duration::from_secs(1));
        assert_eq!(parsed.max_failed_keep_alive_count.get(), 1);
    }

    #[test]
    fn endpoint_url_is_required_and_must_be_opc_tcp() {
        assert_eq!(
            keys_of(&OpcUaDriver.validate_config(&Settings::new())),
            vec![keys::ENDPOINT_URL]
        );
        for bad in [
            "",
            "  ",
            "http://127.0.0.1:4840/",
            "127.0.0.1:4840",
            "opc.tcp://",
            "opc.tcp:///path",
            "opc.tcp://:4840/",
            "opc.tcp://host name:4840/",
        ] {
            let s = settings(&[(keys::ENDPOINT_URL, bad)]);
            assert_eq!(
                keys_of(&OpcUaDriver.validate_config(&s)),
                vec![keys::ENDPOINT_URL],
                "{bad:?}"
            );
        }
        let s = settings(&[(keys::ENDPOINT_URL, "OPC.TCP://Host:4840/UA/Server")]);
        assert_eq!(OpcUaDriver.validate_config(&s), vec![]);
    }

    #[test]
    fn subscribe_is_refused_as_not_supported_yet() {
        let s = settings(&[
            (keys::ENDPOINT_URL, "opc.tcp://127.0.0.1:4840/"),
            (keys::READ_MODE, "subscribe"),
        ]);
        let errors = OpcUaDriver.validate_config(&s);
        assert_eq!(keys_of(&errors), vec![keys::READ_MODE]);
        assert!(
            errors[0].message.contains("まだ対応していません"),
            "{errors:?}"
        );
        let s = settings(&[
            (keys::ENDPOINT_URL, "opc.tcp://127.0.0.1:4840/"),
            (keys::READ_MODE, "poll"),
        ]);
        assert_eq!(
            keys_of(&OpcUaDriver.validate_config(&s)),
            vec![keys::READ_MODE]
        );
    }

    #[test]
    fn keep_alive_settings_are_range_checked() {
        for (key, bad) in [
            (keys::KEEP_ALIVE_INTERVAL_MS, "0"),
            (keys::KEEP_ALIVE_INTERVAL_MS, "99"),
            (keys::KEEP_ALIVE_INTERVAL_MS, "30001"),
            (keys::KEEP_ALIVE_INTERVAL_MS, "5s"),
            (keys::MAX_FAILED_KEEP_ALIVE_COUNT, "0"),
            (keys::MAX_FAILED_KEEP_ALIVE_COUNT, "11"),
            (keys::MAX_FAILED_KEEP_ALIVE_COUNT, "-1"),
        ] {
            let s = settings(&[(keys::ENDPOINT_URL, "opc.tcp://h:4840/"), (key, bad)]);
            assert_eq!(
                keys_of(&OpcUaDriver.validate_config(&s)),
                vec![key],
                "{key}={bad}"
            );
        }
    }

    #[test]
    fn schema_matches_the_settings_the_driver_reads() {
        let schema = OpcUaDriver.settings_schema();
        let keys: Vec<&str> = schema.fields.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                keys::ENDPOINT_URL,
                keys::READ_MODE,
                keys::KEEP_ALIVE_INTERVAL_MS,
                keys::MAX_FAILED_KEEP_ALIVE_COUNT
            ]
        );
        assert!(schema.field(keys::ENDPOINT_URL).unwrap().required);
        assert_eq!(
            schema.field(keys::READ_MODE).unwrap().default,
            Some(SettingValue::Text("read".into()))
        );
        assert_eq!(
            schema.field(keys::KEEP_ALIVE_INTERVAL_MS).unwrap().default,
            Some(SettingValue::Integer(5_000))
        );
        assert!(OpcUaDriver.secret_keys().is_empty());
    }

    #[test]
    fn parse_address_gives_the_display_form() {
        for (text, canonical) in [
            ("i=2259", "ns=0;i=2259"),
            ("ns=0;i=2259", "ns=0;i=2259"),
            ("  ns=2;s=Line1.Speed  ", "ns=2;s=Line1.Speed"),
            ("ns=2;s=a;b", "ns=2;s=a;b"),
            (
                "nsu=urn:banto:opcua:tags;s=PLC1.Line1.Speed",
                "nsu=urn:banto:opcua:tags;s=PLC1.Line1.Speed",
            ),
            ("nsu=urn:a%3bb;i=7", "nsu=urn:a%3Bb;i=7"),
            ("nsu=urn:a%25b;i=7", "nsu=urn:a%25b;i=7"),
            ("svr=0;ns=1;i=5", "ns=1;i=5"),
        ] {
            let a = OpcUaDriver.parse_address(text, &n(DataType::F64)).unwrap();
            assert_eq!(a.canonical, canonical, "{text:?}");
            assert_eq!(a.driver.as_str(), PROTOCOL);
            assert_eq!(a.word_count, 4);
            assert_eq!(a.bit, None);
            assert_eq!(a.index, None);
            let again = OpcUaDriver
                .parse_address(&a.canonical, &n(DataType::F64))
                .unwrap();
            assert_eq!(again, a, "idempotent for {text:?}");
        }
    }

    #[test]
    fn parse_address_refuses_bad_nodeids_guid_bytestring_and_strings() {
        for text in [
            "",
            "   ",
            "D100",
            "40001",
            "ns=;i=1",
            "ns=70000;i=1",
            "ns=1",
            "nsu=;i=1",
            "i=-1",
            "i=4294967296",
            "s=",
            "x=1",
            "svr=1;i=1",
        ] {
            assert!(
                OpcUaDriver.parse_address(text, &n(DataType::U16)).is_err(),
                "{text:?}"
            );
        }
        for text in [
            "ns=1;g=09087e75-8e5e-499b-954f-f2a9603db28a",
            "ns=1;b=M/RbKBsRVkePCePcx24oRA==",
        ] {
            let e = OpcUaDriver
                .parse_address(text, &n(DataType::U16))
                .unwrap_err();
            assert!(e.message().contains("未対応の識別子の型"), "{e}");
        }
        let e = OpcUaDriver
            .parse_address("ns=2;s=Name", &TagKind::string(8, StringEncoding::Utf8))
            .unwrap_err();
        assert!(e.message().contains("#678"), "{e}");
    }

    #[test]
    fn offsets_are_refused_except_zero() {
        let d = OpcUaDriver;
        let k = n(DataType::I16);
        assert_eq!(d.offset_address("ns=2;i=5", &k, 0).unwrap(), "ns=2;i=5");
        assert!(d.offset_address("ns=2;i=5", &k, 1).is_err());
        assert!(d.offset_address("ns=2;i=5", &k, -1).is_err());
        assert!(d.offset_address("bogus", &k, 0).is_err());
        assert!(d.offset_words("ns=2;i=5", 1).is_err());
        assert!(d.offset_words("ns=2;i=5", 0).is_err());
    }

    #[test]
    fn kinds_are_bit_and_every_number_without_string() {
        let kinds = OpcUaDriver.supported_kinds();
        assert_eq!(kinds.len(), 9);
        assert!(kinds.contains(&KindName::Numeric(DataType::U64)));
        assert!(!kinds.contains(&KindName::String));
    }

    #[test]
    fn probe_is_server_state_as_i32() {
        let (text, kind) = OpcUaDriver.probe().unwrap();
        assert_eq!(kind, n(DataType::I32));
        assert_eq!(
            OpcUaDriver.parse_address(&text, &kind).unwrap().canonical,
            "ns=0;i=2259"
        );
    }

    #[test]
    fn help_recommends_nsu_and_shows_the_hub_form() {
        let help = OpcUaDriver.address_help();
        for needle in [
            "nsu=",
            "ns=",
            "%3B",
            "%25",
            "nsu=urn:banto:opcua:tags;s=<外部名>",
            "再起動",
        ] {
            assert!(help.contains(needle), "{needle}");
        }
    }

    #[test]
    fn descriptor_is_opcua_read_write() {
        let d = OpcUaDriver.descriptor();
        assert_eq!(d.id.as_str(), "opcua");
        assert!(d.capabilities.read && d.capabilities.write);
    }
}
