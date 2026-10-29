//! Host-owned Modbus write gate. Guest settings and exports do not grant
//! operational permission; the connection must independently opt in here.

use super::*;

pub(super) const MODE_KEY: &str = "modbus_operational_write_mode";
pub(super) const ALLOWLIST_KEY: &str = "modbus_operational_write_allowlist";
const MAX_RANGES: usize = 16;
const MAX_TEXT: usize = 512;
const MAX_ADDRESS: u32 = 65_535;
/// Also the cap on a Modbus WASM string tag (#914 C1), which is one request.
pub(super) const MAX_WORDS: u32 = 123;
/// FC15 allows 1968 coils per request, but the guest caps every write at 123
/// items (CONTRACT.md, guest #53), so the host cap is min(1968, 123).
const MAX_COILS: u32 = 123;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Area {
    Coil,
    Holding,
}

impl Area {
    fn prefix(self) -> char {
        match self {
            Self::Coil => '0',
            Self::Holding => '4',
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Coil => "coil",
            Self::Holding => "holding_register",
        }
    }

    fn canonical(self, index: u32) -> String {
        format!("{}{:05}", self.prefix(), index + 1)
    }
}

#[derive(Clone, Debug)]
pub(super) struct Policy {
    enabled: bool,
    ranges: Vec<(Area, u32, u32)>,
}

impl Policy {
    pub(super) fn parse(settings: &Settings) -> Result<Self, (&'static str, &'static str)> {
        let enabled = match settings.get(MODE_KEY).unwrap_or("disabled") {
            "disabled" => false,
            "enabled" => true,
            _ => {
                return Err((
                    MODE_KEY,
                    "Modbus operational write mode must be disabled or enabled",
                ))
            }
        };
        let text = settings.get(ALLOWLIST_KEY).unwrap_or("");
        if text.len() > MAX_TEXT {
            return Err((
                ALLOWLIST_KEY,
                "Modbus operational write allowlist is too long",
            ));
        }
        let mut ranges = Vec::new();
        if !text.is_empty() {
            for part in text.split(',') {
                if ranges.len() == MAX_RANGES {
                    return Err((ALLOWLIST_KEY, "too many Modbus operational write ranges"));
                }
                let (first, last) = part
                    .split_once('-')
                    .ok_or((ALLOWLIST_KEY, "expected same-area Modbus start-end ranges"))?;
                let (area, start) = parse_reference(first)?;
                let (end_area, end) = parse_reference(last)?;
                if area != end_area || end < start {
                    return Err((
                        ALLOWLIST_KEY,
                        "Modbus write range must ascend within one writable area",
                    ));
                }
                if ranges
                    .iter()
                    .any(|&(existing, from, to)| existing == area && start <= to && end >= from)
                {
                    return Err((ALLOWLIST_KEY, "Modbus operational write ranges overlap"));
                }
                ranges.push((area, start, end));
            }
        }
        if enabled && ranges.is_empty() {
            return Err((
                ALLOWLIST_KEY,
                "enabled Modbus writes require an operational allowlist",
            ));
        }
        Ok(Self { enabled, ranges })
    }

    /// Host permission for the span; the error is the refusal reason ("disabled" wins).
    fn check(&self, area: Area, start: u32, end: u32) -> Result<(), &'static str> {
        if !self.enabled {
            return Err("Modbus writes are disabled");
        }
        if end <= MAX_ADDRESS
            && self
                .ranges
                .iter()
                .any(|&(candidate, from, to)| candidate == area && from <= start && end <= to)
        {
            Ok(())
        } else {
            Err("outside the Modbus write allowlist")
        }
    }
}

fn parse_reference(text: &str) -> Result<(Area, u32), (&'static str, &'static str)> {
    let bytes = text.as_bytes();
    if bytes.len() != 6 || !bytes.iter().all(u8::is_ascii_digit) {
        return Err((ALLOWLIST_KEY, "Modbus write references must use six digits"));
    }
    let area = match bytes[0] {
        b'0' => Area::Coil,
        b'4' => Area::Holding,
        _ => return Err((ALLOWLIST_KEY, "FC2/FC4 input areas cannot be written")),
    };
    let number = text[1..]
        .parse::<u32>()
        .map_err(|_| (ALLOWLIST_KEY, "invalid Modbus write reference"))?;
    let index = number
        .checked_sub(1)
        .filter(|n| *n <= MAX_ADDRESS)
        .ok_or((ALLOWLIST_KEY, "Modbus write reference is out of range"))?;
    Ok((area, index))
}

pub(super) fn schema_fields() -> [SettingField; 2] {
    let mut mode = SettingField::new(
        MODE_KEY,
        "Modbus operational write permission",
        FieldKind::Choice {
            options: vec![
                api::ChoiceOption::new("disabled", "disabled"),
                api::ChoiceOption::new("enabled", "enabled"),
            ],
        },
    );
    mode.default = Some(SettingValue::Text("disabled".into()));
    mode.group = FieldGroup::Advanced;
    mode.help = Some("Host permission. The guest write mode must also be enabled.".into());
    let mut allowlist = SettingField::new(
        ALLOWLIST_KEY,
        "Modbus operational write ranges",
        FieldKind::Text {
            max_len: Some(MAX_TEXT as u32),
            placeholder: Some("000001-000016,400001-400010".into()),
        },
    );
    allowlist.default = Some(SettingValue::Text(String::new()));
    allowlist.group = FieldGroup::Advanced;
    allowlist.help = Some(
        "Host allowlist for complete FC5/FC15 coil or FC16/FC6 holding spans (each request inside one range), or the word of one FC22 holding register bit."
            .into(),
    );
    [mode, allowlist]
}

pub(super) fn validate_guest_schema(
    fields: &[SettingField],
    secrets: &[String],
) -> Result<(), LoadError> {
    if secrets.iter().any(|key| {
        matches!(
            key.as_str(),
            "write_mode" | "write_allowlist" | MODE_KEY | ALLOWLIST_KEY
        )
    }) || fields
        .iter()
        .any(|field| matches!(field.key.as_str(), MODE_KEY | ALLOWLIST_KEY))
    {
        return Err(LoadError::Contract(
            "Modbus write policy fields must be host-owned and non-secret".into(),
        ));
    }
    let mode = fields.iter().find(|field| field.key == "write_mode");
    let allowlist = fields.iter().find(|field| field.key == "write_allowlist");
    if !mode.is_some_and(|field| {
        matches!(&field.default, Some(SettingValue::Text(value)) if value == "disabled")
            && matches!(&field.kind, FieldKind::Choice { options }
                if options.len() == 2
                    && options.iter().any(|option| option.value == "disabled")
                    && options.iter().any(|option| option.value == "enabled"))
    }) || !allowlist.is_some_and(|field| {
        matches!(&field.default, Some(SettingValue::Text(value)) if value.is_empty())
            && matches!(&field.kind, FieldKind::Text { max_len: Some(max), .. } if *max <= MAX_TEXT as u32)
    }) {
        return Err(LoadError::Contract("Modbus guest writes must be disabled by default with a bounded empty allowlist".into()));
    }
    Ok(())
}

const REASON_TYPE: &str = "tag type cannot be written to this area";
const REASON_RUN: &str = "request is not one contiguous run of writable addresses";
const REASON_VALUE: &str = "value does not fit the tag type";
const REASON_BIT: &str = "register bit address does not match this connection";
const REASON_STRING_AREA: &str = "only a holding register span can be written as a string";

pub(super) fn preflight(
    items: &[WriteItem],
    driver_id: &DriverId,
    policy: &Policy,
) -> Result<Vec<write::WriteItem>, WriteProgress> {
    let refused = |reason: &str| {
        WriteProgress::Done(vec![
            Err(ItemError::Unsupported(reason.into()));
            items.len()
        ])
    };
    if items
        .iter()
        .any(|item| matches!(item.spec.kind, TagKind::Str { .. }))
    {
        return string_registers(items, driver_id, policy).map_err(refused);
    }
    if let [item] = items {
        if item.spec.address.bit.is_some() {
            return register_bit(item, driver_id, policy).map_err(refused);
        }
    }
    let first = items.first().ok_or_else(|| refused(REASON_RUN))?;
    if items.iter().any(|item| item.spec.address.bit.is_some()) {
        return Err(refused("a register bit must be written alone"));
    }
    let (area, start) =
        parse_reference(&first.spec.address.canonical).map_err(|_| refused(REASON_RUN))?;
    let mut next = start;
    let mut checked = Vec::with_capacity(items.len());
    for item in items {
        let address = &item.spec.address;
        let kind = match &item.spec.kind {
            TagKind::Numeric(kind) => *kind,
            _ => return Err(refused(REASON_TYPE)),
        };
        let width = if area == Area::Coil {
            if kind != DataType::Bit {
                return Err(refused(REASON_TYPE));
            }
            1
        } else {
            if kind == DataType::Bit {
                return Err(refused(REASON_TYPE));
            }
            u32::from(kind.register_span())
        };
        if next > MAX_ADDRESS
            || address.driver != *driver_id
            || address.area != area.name()
            || !address.writable_area
            || address.index != Some(u64::from(next))
            || address.canonical != area.canonical(next)
            || u32::from(address.word_count) != width
        {
            return Err(refused(REASON_RUN));
        }
        let value = match (kind, &item.value) {
            (DataType::Bit, api::Value::Bit(value)) if area == Area::Coil => {
                write_types::Value::Bool(*value)
            }
            (DataType::I16 | DataType::I32 | DataType::I64, api::Value::F64(value))
                if area == Area::Holding
                    && value.is_finite()
                    && value.fract() == 0.0
                    && match kind {
                        DataType::I16 => (-32768.0..=32767.0).contains(value),
                        DataType::I32 => (-2147483648.0..=2147483647.0).contains(value),
                        _ => value.abs() <= 9_007_199_254_740_991.0,
                    } =>
            {
                write_types::Value::Int(*value as i64)
            }
            (DataType::U16 | DataType::U32 | DataType::U64, api::Value::F64(value))
                if area == Area::Holding
                    && value.is_finite()
                    && value.fract() == 0.0
                    && (0.0..=match kind {
                        DataType::U16 => 65535.0,
                        DataType::U32 => 4294967295.0,
                        _ => 9_007_199_254_740_991.0,
                    })
                        .contains(value) =>
            {
                write_types::Value::Uint(*value as u64)
            }
            (DataType::F32, api::Value::F64(value))
                if area == Area::Holding && value.is_finite() && (*value as f32).is_finite() =>
            {
                write_types::Value::Float(*value)
            }
            (DataType::F64, api::Value::F64(value))
                if area == Area::Holding && value.is_finite() =>
            {
                write_types::Value::Float(*value)
            }
            _ => return Err(refused(REASON_VALUE)),
        };
        checked.push(write::WriteItem {
            spec: write_types::TagSpec {
                address: address.canonical.clone(),
                data_type: if kind == DataType::Bit {
                    "bool".into()
                } else {
                    kind.to_string()
                },
                string_words: None,
            },
            value,
        });
        next = next.checked_add(width).ok_or_else(|| refused(REASON_RUN))?;
    }
    let span = next - start;
    // Unreachable (items are non-empty and every width is at least 1); kept as a guard.
    if span == 0 {
        return Err(refused(REASON_RUN));
    }
    // A coil run of 2+ points is one FC15 (guest #53); the whole span must
    // still lie inside one host allowlist interval (`Policy::check`).
    match area {
        Area::Coil if span > MAX_COILS => return Err(refused("too many coils in one request")),
        Area::Holding if span > MAX_WORDS => {
            return Err(refused("too many registers in one request"))
        }
        _ => {}
    }
    policy.check(area, start, next - 1).map_err(refused)?;
    Ok(checked)
}

/// #914 C1: a request holding a string tag must be exactly that one holding
/// register string of at most 123 words. Its whole span `[start, start +
/// words - 1]` must lie inside one host interval, and the value is sent as the
/// encoded, 0x00-padded span ([`encoded_string`]) for the guest's one FC16.
fn string_registers(
    items: &[WriteItem],
    driver_id: &DriverId,
    policy: &Policy,
) -> Result<Vec<write::WriteItem>, &'static str> {
    let [item] = items else {
        return Err("a string tag must be written alone");
    };
    let address = &item.spec.address;
    let TagKind::Str { words, encoding } = item.spec.kind else {
        return Err(REASON_TYPE);
    };
    let (area, start) = parse_reference(&address.canonical).map_err(|_| REASON_STRING_AREA)?;
    if area != Area::Holding {
        return Err(REASON_STRING_AREA);
    }
    if u32::from(words) > MAX_WORDS {
        return Err("too many registers in one request");
    }
    let end = (start + u32::from(words))
        .checked_sub(1)
        .ok_or(REASON_RUN)?;
    if address.driver != *driver_id
        || address.area != area.name()
        || !address.writable_area
        || address.index != Some(u64::from(start))
        || address.bit.is_some()
        || address.word_count != words
    {
        return Err(REASON_RUN);
    }
    policy.check(area, start, end)?;
    let bytes = encoded_string(&item.value, words, encoding).ok_or(REASON_VALUE)?;
    Ok(vec![write::WriteItem {
        spec: write_types::TagSpec {
            address: address.canonical.clone(),
            data_type: "string".into(),
            string_words: Some(u32::from(words)),
        },
        value: write_types::Value::Bytes(bytes),
    }])
}

/// One holding register bit `4xxxxx.N` written alone (the guest sends one
/// FC22 mask write that changes only that bit). The host permission must
/// cover the whole word. Input-register bits, non-Bool values and bits
/// mixed into a multi-item request never reach the guest.
fn register_bit(
    item: &WriteItem,
    driver_id: &DriverId,
    policy: &Policy,
) -> Result<Vec<write::WriteItem>, &'static str> {
    let address = &item.spec.address;
    let bit = address.bit.filter(|bit| *bit <= 15).ok_or(REASON_BIT)?;
    let (word, bit_text) = address.canonical.split_once('.').ok_or(REASON_BIT)?;
    let parsed = parse_reference(word);
    if parsed.map_or(address.area != Area::Holding.name(), |(area, _)| {
        area != Area::Holding
    }) {
        return Err("only holding register bits can be written");
    }
    let (area, index) = parsed.map_err(|_| REASON_BIT)?;
    if item.spec.kind != TagKind::numeric(DataType::Bit) {
        return Err(REASON_TYPE);
    }
    let api::Value::Bit(value) = item.value else {
        return Err(REASON_VALUE);
    };
    if bit_text != bit.to_string()
        || address.driver != *driver_id
        || address.area != area.name()
        || !address.writable_area
        || address.index != Some(u64::from(index))
        || address.word_count != 1
    {
        return Err(REASON_BIT);
    }
    policy.check(area, index, index)?;
    Ok(vec![write::WriteItem {
        spec: write_types::TagSpec {
            address: address.canonical.clone(),
            data_type: "bool".into(),
            string_words: None,
        },
        value: write_types::Value::Bool(value),
    }])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(
        reference: &str,
        area: &str,
        index: u64,
        kind: DataType,
        value: api::Value,
    ) -> WriteItem {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let address = DriverAddress::new(
            id,
            reference,
            area,
            matches!(area, "coil" | "holding_register"),
            kind.register_span(),
        )
        .with_index(index);
        WriteItem::new(TagSpec::new(address, TagKind::numeric(kind)), value)
    }

    fn policy(mode: &str, ranges: &str) -> Policy {
        Policy::parse(&Settings::from_pairs([(MODE_KEY, mode), (ALLOWLIST_KEY, ranges)]).unwrap())
            .unwrap()
    }

    #[test]
    fn host_gate_requires_independent_permission_and_complete_span() {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let items = [
            item(
                "400001",
                "holding_register",
                0,
                DataType::I32,
                api::Value::F64(-42.0),
            ),
            item(
                "400003",
                "holding_register",
                2,
                DataType::F64,
                api::Value::F64(1.5),
            ),
        ];
        assert!(preflight(&items, &id, &Policy::parse(&Settings::new()).unwrap()).is_err());
        assert!(preflight(&items, &id, &policy("enabled", "400001-400005")).is_err());
        assert_eq!(
            preflight(&items, &id, &policy("enabled", "400001-400006"))
                .unwrap()
                .len(),
            2
        );
        let mut invalid = items.to_vec();
        invalid[1].spec.address.area = "input_register".into();
        assert!(preflight(&invalid, &id, &policy("enabled", "400001-400006")).is_err());
        invalid = items.to_vec();
        invalid[1].value = api::Value::F64(f64::NAN);
        assert!(preflight(&invalid, &id, &policy("enabled", "400001-400006")).is_err());
    }

    fn refusal_text(result: Result<Vec<write::WriteItem>, WriteProgress>, count: usize) -> String {
        let Err(WriteProgress::Done(results)) = result else {
            panic!("expected a refusal");
        };
        assert_eq!(results.len(), count);
        let mut texts = results.into_iter().map(|result| match result {
            Err(ItemError::Unsupported(text)) => text,
            other => panic!("unexpected {other:?}"),
        });
        let first = texts.next().unwrap();
        assert!(texts.all(|text| text == first));
        first
    }

    #[test]
    fn refusals_say_why() {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let on = policy("enabled", "000001-000016,400001-400200");
        let h = |r: &str, i: u64, k: DataType, v: api::Value| item(r, "holding_register", i, k, v);
        let u16_ok = || h("400001", 0, DataType::U16, api::Value::F64(1.0));
        let cases: Vec<(Vec<WriteItem>, Policy, &str)> = vec![
            (
                vec![u16_ok()],
                policy("disabled", ""),
                "Modbus writes are disabled",
            ),
            (
                vec![h("400300", 299, DataType::U16, api::Value::F64(1.0))],
                on.clone(),
                "outside the Modbus write allowlist",
            ),
            (
                vec![bit_item(
                    "300001.0",
                    "input_register",
                    0,
                    0,
                    api::Value::Bit(true),
                )],
                on.clone(),
                "only holding register bits can be written",
            ),
            (
                vec![
                    bit_item("400001.0", "holding_register", 0, 0, api::Value::Bit(true)),
                    h("400002", 1, DataType::U16, api::Value::F64(1.0)),
                ],
                on.clone(),
                "a register bit must be written alone",
            ),
            (
                vec![h("400001", 0, DataType::U16, api::Value::F64(70000.0))],
                on.clone(),
                "value does not fit the tag type",
            ),
            (
                vec![h("400001", 0, DataType::F32, api::Value::F64(f64::NAN))],
                on.clone(),
                "value does not fit the tag type",
            ),
            (
                vec![
                    item("000001", "coil", 0, DataType::Bit, api::Value::Bit(true)),
                    item("000003", "coil", 2, DataType::Bit, api::Value::Bit(true)),
                ],
                on.clone(),
                "request is not one contiguous run of writable addresses",
            ),
            (
                vec![
                    item("000016", "coil", 15, DataType::Bit, api::Value::Bit(true)),
                    item("000017", "coil", 16, DataType::Bit, api::Value::Bit(true)),
                ],
                on.clone(),
                "outside the Modbus write allowlist",
            ),
            (
                coil_run(0, 124),
                policy("enabled", "000001-000200"),
                "too many coils in one request",
            ),
            (
                vec![item(
                    "000001",
                    "coil",
                    0,
                    DataType::U16,
                    api::Value::F64(1.0),
                )],
                on.clone(),
                "tag type cannot be written to this area",
            ),
            (
                vec![bit_item(
                    "400001.0",
                    "holding_register",
                    0,
                    0,
                    api::Value::Bit(true),
                )],
                policy("disabled", ""),
                "Modbus writes are disabled",
            ),
            (
                vec![bit_item(
                    "400300.0",
                    "holding_register",
                    299,
                    0,
                    api::Value::Bit(true),
                )],
                on.clone(),
                "outside the Modbus write allowlist",
            ),
            (
                vec![bit_item(
                    "400001.0",
                    "holding_register",
                    0,
                    0,
                    api::Value::F64(1.0),
                )],
                on.clone(),
                "value does not fit the tag type",
            ),
            (
                vec![{
                    let mut x =
                        bit_item("400001.0", "holding_register", 0, 0, api::Value::Bit(true));
                    x.spec.kind = TagKind::numeric(DataType::U16);
                    x
                }],
                on.clone(),
                "tag type cannot be written to this area",
            ),
            (
                vec![bit_item(
                    "400001.16",
                    "holding_register",
                    0,
                    16,
                    api::Value::Bit(true),
                )],
                on.clone(),
                "register bit address does not match this connection",
            ),
            (
                vec![{
                    let mut x =
                        bit_item("400001.0", "holding_register", 0, 0, api::Value::Bit(true));
                    x.spec.address.writable_area = false;
                    x
                }],
                on.clone(),
                "register bit address does not match this connection",
            ),
            (
                vec![{
                    let mut x = h("400001", 0, DataType::U16, api::Value::F64(1.0));
                    // #914 C1: a holding string is writable, but only with text.
                    x.spec.kind = TagKind::string(2, banto_driver_api::StringEncoding::Utf8);
                    x.spec.address.word_count = 2;
                    x
                }],
                on.clone(),
                "value does not fit the tag type",
            ),
            (
                vec![
                    h("400001", 0, DataType::U16, api::Value::F64(1.0)),
                    bit_item("400002.0", "holding_register", 1, 0, api::Value::Bit(true)),
                ],
                on.clone(),
                "a register bit must be written alone",
            ),
            (
                vec![
                    h("400001", 0, DataType::U16, api::Value::F64(1.0)),
                    h("400003", 2, DataType::U16, api::Value::F64(1.0)),
                ],
                on.clone(),
                "request is not one contiguous run of writable addresses",
            ),
            (
                (0..124)
                    .map(|i| {
                        h(
                            &format!("{}", 400001 + i),
                            i,
                            DataType::U16,
                            api::Value::F64(1.0),
                        )
                    })
                    .collect(),
                on.clone(),
                "too many registers in one request",
            ),
        ];
        for (items, gate, expected) in cases {
            let count = items.len();
            assert_eq!(refusal_text(preflight(&items, &id, &gate), count), expected);
        }
    }

    #[test]
    fn input_areas_and_invalid_types_never_reach_guest() {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let gate = policy("enabled", "000001-000016,400001-400010");
        assert!(preflight(
            &[item(
                "000001",
                "coil",
                0,
                DataType::Bit,
                api::Value::Bit(true)
            )],
            &id,
            &gate
        )
        .is_ok());
        for bad in [
            item(
                "100001",
                "discrete_input",
                0,
                DataType::Bit,
                api::Value::Bit(true),
            ),
            item(
                "300001",
                "input_register",
                0,
                DataType::U16,
                api::Value::F64(1.0),
            ),
            item("000001", "coil", 0, DataType::Bit, api::Value::F64(1.0)),
            item(
                "400001",
                "holding_register",
                0,
                DataType::U16,
                api::Value::F64(65536.0),
            ),
        ] {
            assert!(preflight(&[bad], &id, &gate).is_err());
        }
        let mut bad = item(
            "400001",
            "holding_register",
            0,
            DataType::U32,
            api::Value::F64(1.0),
        );
        bad.spec.address.word_count = 1;
        assert!(preflight(&[bad], &id, &gate).is_err());
    }

    /// `count` contiguous coils from zero-based `first`, alternating values.
    fn coil_run(first: u64, count: u64) -> Vec<WriteItem> {
        (first..first + count)
            .map(|i| {
                item(
                    &format!("{:06}", i + 1),
                    "coil",
                    i,
                    DataType::Bit,
                    api::Value::Bit(i.is_multiple_of(2)),
                )
            })
            .collect()
    }

    #[test]
    fn contiguous_coil_runs_pass_only_inside_one_host_interval() {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        // Two adjacent intervals: a run may not straddle them.
        let gate = policy(
            "enabled",
            "000001-000008,000009-000016,000101-000300,400001-400010",
        );
        for (first, count) in [(0, 2), (0, 8), (8, 8), (3, 5), (100, 123)] {
            let items = coil_run(first, count);
            let checked = preflight(&items, &id, &gate).unwrap();
            assert_eq!(checked.len(), items.len());
            for (i, (checked, item)) in checked.iter().zip(&items).enumerate() {
                assert_eq!(checked.spec.address, item.spec.address.canonical);
                assert_eq!(checked.spec.data_type, "bool");
                let expected = (first + i as u64).is_multiple_of(2);
                assert!(
                    matches!(checked.value, write_types::Value::Bool(v) if v == expected),
                    "{first}+{i}"
                );
            }
        }
        let refused = |items: Vec<WriteItem>, gate: &Policy| {
            let count = items.len();
            refusal_text(preflight(&items, &id, gate), count)
        };
        // Straddles 000008/000009, or leaves the last interval.
        assert_eq!(
            refused(coil_run(6, 4), &gate),
            "outside the Modbus write allowlist"
        );
        assert_eq!(
            refused(coil_run(295, 10), &gate),
            "outside the Modbus write allowlist"
        );
        // Host permission off.
        assert_eq!(
            refused(coil_run(0, 2), &policy("disabled", "")),
            "Modbus writes are disabled"
        );
        // 124 coils inside one interval: over the guest's 123-item cap.
        assert_eq!(
            refused(coil_run(100, 124), &gate),
            "too many coils in one request"
        );
        // Gap, duplicate, descending.
        for order in [[0_usize, 2], [0, 0], [1, 0]] {
            let run = coil_run(0, 3);
            let items = order.iter().map(|&i| run[i].clone()).collect();
            assert_eq!(
                refused(items, &gate),
                "request is not one contiguous run of writable addresses",
                "{order:?}"
            );
        }
        // A coil mixed with a holding register, either way round.
        let holding = item(
            "400001",
            "holding_register",
            0,
            DataType::U16,
            api::Value::F64(1.0),
        );
        let mut coils = coil_run(0, 2);
        coils.push(holding.clone());
        assert_eq!(refused(coils, &gate), REASON_TYPE);
        let mut mixed = vec![holding];
        mixed.extend(coil_run(1, 1));
        assert_eq!(refused(mixed, &gate), REASON_TYPE);
        // A non-Bool value, a non-Bit type, another driver, a read-only area,
        // or a canonical address that does not match the index.
        let mut bad = coil_run(0, 3);
        bad[1].value = api::Value::F64(1.0);
        assert_eq!(refused(bad, &gate), REASON_VALUE);
        let mut bad = coil_run(0, 3);
        bad[2].spec.kind = TagKind::numeric(DataType::U16);
        assert_eq!(refused(bad, &gate), REASON_TYPE);
        for mutate in [
            |x: &mut WriteItem| x.spec.address.driver = DriverId::new("other-driver").unwrap(),
            |x: &mut WriteItem| x.spec.address.writable_area = false,
            |x: &mut WriteItem| x.spec.address.canonical = "000009".into(),
        ] {
            let mut bad = coil_run(0, 3);
            mutate(&mut bad[1]);
            assert_eq!(refused(bad, &gate), REASON_RUN);
        }
    }

    fn bit_item(reference: &str, area: &str, index: u64, bit: u8, value: api::Value) -> WriteItem {
        let mut item = item(reference, area, index, DataType::Bit, value);
        item.spec.address = item.spec.address.with_bit(bit);
        item
    }

    #[test]
    fn one_holding_register_bit_passes_only_inside_the_host_word_allowlist() {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let gate = policy("enabled", "400001-400010");
        for value in [false, true] {
            let checked = preflight(
                &[bit_item(
                    "400010.15",
                    "holding_register",
                    9,
                    15,
                    api::Value::Bit(value),
                )],
                &id,
                &gate,
            )
            .unwrap();
            assert_eq!(checked.len(), 1);
            assert_eq!(checked[0].spec.address, "400010.15");
            assert_eq!(checked[0].spec.data_type, "bool");
            assert!(matches!(checked[0].value, write_types::Value::Bool(v) if v == value));
        }
        let ok = bit_item("400001.0", "holding_register", 0, 0, api::Value::Bit(true));
        assert!(preflight(std::slice::from_ref(&ok), &id, &gate).is_ok());
        assert!(preflight(
            std::slice::from_ref(&ok),
            &id,
            &Policy::parse(&Settings::new()).unwrap()
        )
        .is_err());
        for bad in [
            vec![bit_item(
                "400011.0",
                "holding_register",
                10,
                0,
                api::Value::Bit(true),
            )],
            vec![bit_item(
                "300001.0",
                "input_register",
                0,
                0,
                api::Value::Bit(true),
            )],
            vec![bit_item(
                "400001.0",
                "holding_register",
                0,
                0,
                api::Value::F64(1.0),
            )],
            vec![bit_item(
                "400001.16",
                "holding_register",
                0,
                16,
                api::Value::Bit(true),
            )],
            vec![
                ok.clone(),
                bit_item("400001.1", "holding_register", 0, 1, api::Value::Bit(true)),
            ],
            vec![
                ok.clone(),
                item(
                    "400002",
                    "holding_register",
                    1,
                    DataType::U16,
                    api::Value::F64(1.0),
                ),
            ],
        ] {
            assert!(preflight(&bad, &id, &gate).is_err(), "{bad:?}");
        }
        let mut wrong_bit = ok.clone();
        wrong_bit.spec.address = wrong_bit.spec.address.with_bit(1);
        assert!(preflight(&[wrong_bit], &id, &gate).is_err());
        let mut wrong_index = ok.clone();
        wrong_index.spec.address = wrong_index.spec.address.with_index(1);
        assert!(preflight(&[wrong_index], &id, &gate).is_err());
        let mut other_driver = ok.clone();
        other_driver.spec.address.driver = DriverId::new("other-driver").unwrap();
        assert!(preflight(&[other_driver], &id, &gate).is_err());
        let mut read_only_area = ok.clone();
        read_only_area.spec.address.writable_area = false;
        assert!(preflight(&[read_only_area], &id, &gate).is_err());
        let mut padded_bit = bit_item("400001.03", "holding_register", 0, 3, api::Value::Bit(true));
        assert!(preflight(std::slice::from_ref(&padded_bit), &id, &gate).is_err());
        padded_bit.spec.address.canonical = "400001.3".into();
        assert!(preflight(&[padded_bit], &id, &gate).is_ok());
        let mut mismatched_bit = ok;
        mismatched_bit.spec.address.canonical = "400001.1".into();
        assert!(preflight(&[mismatched_bit], &id, &gate).is_err());
    }

    #[test]
    fn invalid_or_unbounded_host_allowlists_fail_closed() {
        for text in [
            "",
            "100001-100002",
            "300001-300002",
            "000000-000001",
            "400001-400011,400010-400012",
            "400001-000001",
            "400010-400001",
            "40001-40010",
        ] {
            assert!(
                Policy::parse(
                    &Settings::from_pairs([(MODE_KEY, "enabled"), (ALLOWLIST_KEY, text)]).unwrap()
                )
                .is_err(),
                "{text}"
            );
        }
    }

    fn string_item(reference: &str, area: &str, index: u64, words: u16, value: &str) -> WriteItem {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let address = DriverAddress::new(id, reference, area, area == "holding_register", words)
            .with_index(index);
        WriteItem::new(
            TagSpec::new(address, TagKind::string(words, api::StringEncoding::Utf8)),
            api::Value::Str(value.into()),
        )
    }

    #[test]
    fn one_holding_register_string_passes_only_as_a_whole_span_inside_one_interval() {
        let id = DriverId::new("modbus-fc3-wasm").unwrap();
        let gate = policy("enabled", "400001-400010,400101-400300");
        let accepted = preflight(
            &[string_item("400001", "holding_register", 0, 4, "AB")],
            &id,
            &gate,
        )
        .unwrap();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].spec.address, "400001");
        assert_eq!(accepted[0].spec.data_type, "string");
        assert_eq!(accepted[0].spec.string_words, Some(4));
        // Character order; packing into registers (high byte first) is the guest's.
        assert!(matches!(&accepted[0].value,
            write_types::Value::Bytes(bytes) if bytes == b"AB\0\0\0\0\0\0"));
        assert!(preflight(
            &[string_item("400001", "holding_register", 0, 10, "A")],
            &id,
            &gate
        )
        .is_ok());
        let max = preflight(
            &[string_item("400101", "holding_register", 100, 123, "A")],
            &id,
            &gate,
        )
        .unwrap();
        assert!(matches!(&max[0].value, write_types::Value::Bytes(bytes) if bytes.len() == 246));

        let check = |item: WriteItem| refusal_text(preflight(&[item], &id, &gate), 1);
        assert_eq!(
            check(string_item("400008", "holding_register", 7, 4, "A")),
            "outside the Modbus write allowlist"
        );
        assert_eq!(
            check(string_item("400101", "holding_register", 100, 124, "A")),
            "too many registers in one request"
        );
        assert_eq!(
            check(string_item("300001", "input_register", 0, 4, "A")),
            REASON_STRING_AREA
        );
        assert_eq!(
            check(string_item("000001", "coil", 0, 1, "A")),
            REASON_STRING_AREA
        );
        assert_eq!(
            check(string_item("400001", "holding_register", 0, 4, "ABCDEFGHI")),
            REASON_VALUE
        );
        let mut numeric_value = string_item("400001", "holding_register", 0, 4, "A");
        numeric_value.value = api::Value::F64(1.0);
        assert_eq!(check(numeric_value), REASON_VALUE);
        let mut wrong_span = string_item("400001", "holding_register", 0, 4, "A");
        wrong_span.spec.address.word_count = 3;
        assert_eq!(check(wrong_span), REASON_RUN);
        let mut wrong_index = string_item("400001", "holding_register", 0, 4, "A");
        wrong_index.spec.address.index = Some(1);
        assert_eq!(check(wrong_index), REASON_RUN);

        let mixed = [
            string_item("400001", "holding_register", 0, 2, "A"),
            item(
                "400003",
                "holding_register",
                2,
                DataType::U16,
                api::Value::F64(1.0),
            ),
        ];
        assert_eq!(
            refusal_text(preflight(&mixed, &id, &gate), 2),
            "a string tag must be written alone"
        );
        let mixed = [
            item(
                "400001",
                "holding_register",
                0,
                DataType::U16,
                api::Value::F64(1.0),
            ),
            string_item("400002", "holding_register", 1, 2, "A"),
        ];
        assert_eq!(
            refusal_text(preflight(&mixed, &id, &gate), 2),
            "a string tag must be written alone"
        );
        assert_eq!(
            refusal_text(
                preflight(
                    &[string_item("400001", "holding_register", 0, 4, "A")],
                    &id,
                    &policy("disabled", "400001-400010")
                ),
                1
            ),
            "Modbus writes are disabled"
        );
    }
}
