//! Host-owned Modbus write gate. Guest settings and exports do not grant
//! operational permission; the connection must independently opt in here.

use super::*;

pub(super) const MODE_KEY: &str = "modbus_operational_write_mode";
pub(super) const ALLOWLIST_KEY: &str = "modbus_operational_write_allowlist";
const MAX_RANGES: usize = 16;
const MAX_TEXT: usize = 512;
const MAX_ADDRESS: u32 = 65_535;
const MAX_WORDS: u32 = 123;

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

    fn allows(&self, area: Area, start: u32, end: u32) -> bool {
        self.enabled
            && end <= MAX_ADDRESS
            && self
                .ranges
                .iter()
                .any(|&(candidate, from, to)| candidate == area && from <= start && end <= to)
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
    allowlist.help = Some("Host allowlist for complete FC5 coil or FC16 holding spans.".into());
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

pub(super) fn preflight(
    items: &[WriteItem],
    driver_id: &DriverId,
    policy: &Policy,
) -> Result<Vec<write::WriteItem>, WriteProgress> {
    let refused = || {
        WriteProgress::Done(vec![
            Err(ItemError::Unsupported(
                "Modbus write is disabled or outside the operational allowlist".into()
            ));
            items.len()
        ])
    };
    let first = items.first().ok_or_else(refused)?;
    let (area, start) = parse_reference(&first.spec.address.canonical).map_err(|_| refused())?;
    if area == Area::Coil && items.len() != 1 {
        return Err(refused());
    }
    let mut next = start;
    let mut checked = Vec::with_capacity(items.len());
    for item in items {
        let address = &item.spec.address;
        let kind = match &item.spec.kind {
            TagKind::Numeric(kind) => *kind,
            _ => return Err(refused()),
        };
        let width = if area == Area::Coil {
            if kind != DataType::Bit {
                return Err(refused());
            }
            1
        } else {
            if kind == DataType::Bit {
                return Err(refused());
            }
            u32::from(kind.register_span())
        };
        if next > MAX_ADDRESS
            || address.driver != *driver_id
            || address.area != area.name()
            || !address.writable_area
            || address.bit.is_some()
            || address.index != Some(u64::from(next))
            || address.canonical != area.canonical(next)
            || u32::from(address.word_count) != width
        {
            return Err(refused());
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
            _ => return Err(refused()),
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
        next = next.checked_add(width).ok_or_else(refused)?;
    }
    let span = next - start;
    if span == 0
        || span > if area == Area::Coil { 1 } else { MAX_WORDS }
        || !policy.allows(area, start, next - 1)
    {
        return Err(refused());
    }
    Ok(checked)
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
}
