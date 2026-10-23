//! C1, C2 and C17-C22: the static half of a driver
//! (docs/banto-driver-api-design.md §7.1; C17-C22 are R2b-6a,
//! docs/banto-hub-driver-ui-design.md §3.3, §3.4, §3.6). No I/O and no
//! runtime.

use banto_driver_api::{
    DriverBase, FieldKind, KindName, SettingValue, Settings, SettingsSchema, StringEncoding,
    TagKind,
};

use crate::{Failure, StaticFixtures};

/// A key no driver reads: C2 adds it and expects no complaint.
const UNKNOWN_KEY: &str = "x-banto-conformance-unknown";

/// Run C1 (addresses), C2 (settings) and C17-C22 (schema, kinds, word offsets,
/// address positions) against `driver`'s [`DriverBase`]
/// half and return every failure (design §7.2).
///
/// Generic over `?Sized` so it takes a `&dyn DriverBase` as well as a
/// `&dyn FrameDriver` / `&dyn SessionDriver` or a concrete driver (the API's
/// MSRV, 1.85, has no `dyn` upcasting).
///
/// - **C1**: each `valid` text parses, to its expected canonical form, with
///   `driver` = the descriptor's id and `word_count` = `kind.word_count()`;
///   parsing the canonical form gives the very same address (idempotent);
///   `offset_address(canonical, kind, 0)` is the canonical form. Each
///   `invalid` text is refused with a non-empty message. Each `offsets`
///   case gives its expected result.
/// - **C2**: the minimal settings validate cleanly, also with an unknown key
///   added; empty settings report only keys of the minimal settings (and at
///   least one, when the minimal settings are not empty); a non-numeric
///   `port` (when the minimal settings have one) is reported on `port`;
///   every secret key is non-empty, unique and present in the minimal
///   settings.
/// - **C17** (skipped for an empty schema): field keys are unique and
///   non-empty, `Choice` values are unique and there is at least one, an
///   `Integer` range is not inverted, and each default fits its field (an
///   in-range integer, a listed choice, a bool, text).
/// - **C18**: `secret_keys()` equals the keys of the `Secret` fields.
/// - **C19**: the schema's defaults overlaid with the minimal settings pass
///   `validate_config`, and each minimal-settings key is in the schema.
/// - **C20**: the probe point's kind is a supported kind that parses; each
///   `kind_examples` entry parses exactly when its kind is in
///   `supported_kinds()`, and (when the list is not empty) every supported
///   kind has an entry.
/// - **C21**: `offset_words(c, 0) == c` for each `valid` address on a driver
///   that supports it (`offset_words` does not fail for `n = 0`); the moved
///   address parses again as the same kind and keeps its bit; each
///   `word_offsets` case gives its expected result.
/// - **C22**: where `index` is given, `offset_words(c, n)` has `index + n`.
pub fn static_checks<D: DriverBase + ?Sized>(
    driver: &D,
    fixtures: &StaticFixtures,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    c1_addresses(driver, fixtures, &mut failures);
    c2_settings(driver, fixtures, &mut failures);
    let schema = driver.settings_schema();
    if !schema.is_empty() {
        c17_schema(&schema, &mut failures);
        c18_secrets(driver, &schema, &mut failures);
        c19_defaults(driver, &schema, fixtures, &mut failures);
    }
    c20_kinds(driver, fixtures, &mut failures);
    c21_c22_word_offsets(driver, fixtures, &mut failures);
    failures
}

fn c17_schema(schema: &SettingsSchema, failures: &mut Vec<Failure>) {
    for (i, field) in schema.fields.iter().enumerate() {
        let key = &field.key;
        if key.trim().is_empty() {
            failures.push(Failure::new("C17", "スキーマに空のキーの欄がある"));
        } else if schema.fields[..i].iter().any(|f| &f.key == key) {
            failures.push(Failure::new(
                "C17",
                format!("スキーマのキー {key:?} が重複している"),
            ));
        }
        if field.label.trim().is_empty() {
            failures.push(Failure::new("C17", format!("欄 {key:?} の label が空")));
        }
        let bad_default = |why: &str| {
            Failure::new(
                "C17",
                format!("欄 {key:?} の既定値 {:?} が{why}", field.default),
            )
        };
        match (&field.kind, &field.default) {
            (_, None) => {}
            (FieldKind::Integer { min, max }, Some(SettingValue::Integer(n))) => {
                if min.is_some_and(|m| *n < m) || max.is_some_and(|m| *n > m) {
                    failures.push(bad_default("範囲の外"));
                }
            }
            (FieldKind::Choice { options }, Some(SettingValue::Text(v))) => {
                if !options.iter().any(|o| &o.value == v) {
                    failures.push(bad_default("選択肢の中に無い"));
                }
            }
            (FieldKind::Bool, Some(SettingValue::Bool(_))) => {}
            (FieldKind::Text { .. } | FieldKind::Host, Some(SettingValue::Text(_))) => {}
            (_, Some(_)) => failures.push(bad_default("欄の種類と合わない")),
        }
        match &field.kind {
            FieldKind::Integer {
                min: Some(min),
                max: Some(max),
            } if min > max => failures.push(Failure::new(
                "C17",
                format!("欄 {key:?} の範囲が逆（{min} > {max}）"),
            )),
            FieldKind::Choice { options } => {
                if options.is_empty() {
                    failures.push(Failure::new("C17", format!("欄 {key:?} の選択肢が空")));
                }
                for (j, option) in options.iter().enumerate() {
                    if options[..j].iter().any(|o| o.value == option.value) {
                        failures.push(Failure::new(
                            "C17",
                            format!("欄 {key:?} の選択肢の値 {:?} が重複している", option.value),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
}

fn c18_secrets<D: DriverBase + ?Sized>(
    driver: &D,
    schema: &SettingsSchema,
    failures: &mut Vec<Failure>,
) {
    let mut declared: Vec<String> = driver.secret_keys();
    declared.sort();
    let mut in_schema: Vec<String> = schema
        .fields
        .iter()
        .filter(|f| matches!(f.kind, FieldKind::Secret))
        .map(|f| f.key.clone())
        .collect();
    in_schema.sort();
    if declared != in_schema {
        failures.push(Failure::new(
            "C18",
            format!("secret_keys() {declared:?} と Secret の欄のキー {in_schema:?} が一致しない"),
        ));
    }
}

fn c19_defaults<D: DriverBase + ?Sized>(
    driver: &D,
    schema: &SettingsSchema,
    fixtures: &StaticFixtures,
    failures: &mut Vec<Failure>,
) {
    let mut settings = Settings::new();
    for field in &schema.fields {
        if let Some(default) = &field.default {
            settings.insert(field.key.clone(), default.to_text());
        }
    }
    for item in fixtures.settings.iter() {
        if schema.field(&item.key).is_none() {
            failures.push(Failure::new(
                "C19",
                format!(
                    "最小の設定のキー {:?} がスキーマに無い（ドライバが読むキーは欄にする）",
                    item.key
                ),
            ));
        }
        settings.insert(item.key.clone(), item.value.clone());
    }
    let errors = driver.validate_config(&settings);
    if !errors.is_empty() {
        failures.push(Failure::new(
            "C19",
            format!("スキーマの既定値に最小の設定を重ねた設定が通らない: {errors:?}"),
        ));
    }
}

/// The kind a [`KindName`] stands for, for trying an address on it.
fn tag_kind_of(name: KindName) -> TagKind {
    match name {
        KindName::Numeric(data_type) => TagKind::numeric(data_type),
        _ => TagKind::string(4, StringEncoding::Utf8),
    }
}

fn c20_kinds<D: DriverBase + ?Sized>(
    driver: &D,
    fixtures: &StaticFixtures,
    failures: &mut Vec<Failure>,
) {
    let supported = driver.supported_kinds();
    if let Some((text, kind)) = driver.probe() {
        match KindName::of(&kind) {
            Some(name) if supported.contains(&name) => {}
            other => failures.push(Failure::new(
                "C20",
                format!("probe の種類 {other:?} が supported_kinds に無い"),
            )),
        }
        if let Err(e) = driver.parse_address(&text, &kind) {
            failures.push(Failure::new(
                "C20",
                format!("probe の点 {text:?} が読めない: {e}"),
            ));
        }
    }
    for (name, text) in &fixtures.kind_examples {
        let listed = supported.contains(name);
        let parsed = driver.parse_address(text, &tag_kind_of(*name));
        match (listed, parsed) {
            (true, Ok(_)) | (false, Err(_)) => {}
            (true, Err(e)) => failures.push(Failure::new(
                "C20",
                format!("supported_kinds にある {name} で {text:?} が読めない: {e}"),
            )),
            (false, Ok(_)) => failures.push(Failure::new(
                "C20",
                format!("supported_kinds に無い {name} で {text:?} が読めてしまった"),
            )),
        }
    }
    if !fixtures.kind_examples.is_empty() {
        for name in &supported {
            if !fixtures.kind_examples.iter().any(|(n, _)| n == name) {
                failures.push(Failure::new(
                    "C20",
                    format!("supported_kinds の {name} の例が kind_examples に無い"),
                ));
            }
        }
    }
}

fn c21_c22_word_offsets<D: DriverBase + ?Sized>(
    driver: &D,
    fixtures: &StaticFixtures,
    failures: &mut Vec<Failure>,
) {
    for (canonical, n, expected) in &fixtures.word_offsets {
        let got = driver.offset_words(canonical, *n);
        match (expected, &got) {
            (Some(want), Ok(have)) if want == have => {}
            (None, Err(_)) => {}
            _ => failures.push(Failure::new(
                "C21",
                format!("offset_words({canonical:?}, {n}) が {got:?}（期待は {expected:?}）"),
            )),
        }
    }
    for (text, kind, canonical) in &fixtures.valid {
        // A driver without word-wise offsets fails even for n = 0; the
        // explicit `word_offsets` cases cover one that fails wrongly.
        match driver.offset_words(canonical, 0) {
            Ok(same) if &same == canonical => {}
            Ok(other) => {
                failures.push(Failure::new(
                    "C21",
                    format!(
                        "offset_words({canonical:?}, 0) が {other:?}（canonical のままであるべき）"
                    ),
                ));
                continue;
            }
            Err(_) => continue,
        }
        let Ok(base) = driver.parse_address(canonical, kind) else {
            continue;
        };
        for n in [1_i64, 2] {
            // Out of the address space is the driver's right to refuse.
            let Ok(next) = driver.offset_words(canonical, n) else {
                continue;
            };
            let moved = match driver.parse_address(&next, kind) {
                Ok(a) => a,
                Err(e) => {
                    failures.push(Failure::new(
                        "C21",
                        format!(
                            "offset_words({canonical:?}, {n}) の {next:?} が元の種類（{kind:?}）で読めない: {e}（元の入力 {text:?}）"
                        ),
                    ));
                    continue;
                }
            };
            if moved.bit != base.bit {
                failures.push(Failure::new(
                    "C21",
                    format!(
                        "offset_words({canonical:?}, {n}) でビットが保たれない: {:?} → {:?}",
                        base.bit, moved.bit
                    ),
                ));
            }
            if moved.area != base.area {
                failures.push(Failure::new(
                    "C21",
                    format!(
                        "offset_words({canonical:?}, {n}) で area が変わった: {:?} → {:?}",
                        base.area, moved.area
                    ),
                ));
            }
            if let Some(index) = base.index {
                if moved.index != Some(index + n as u64) {
                    failures.push(Failure::new(
                        "C22",
                        format!(
                            "offset_words({canonical:?}, {n}) の index が {:?}（期待は {}）",
                            moved.index,
                            index + n as u64
                        ),
                    ));
                }
            }
        }
    }
}

fn c1_addresses<D: DriverBase + ?Sized>(
    driver: &D,
    fixtures: &StaticFixtures,
    failures: &mut Vec<Failure>,
) {
    let id = &driver.descriptor().id;
    for (text, kind, expected) in &fixtures.valid {
        let parsed = match driver.parse_address(text, kind) {
            Ok(a) => a,
            Err(e) => {
                failures.push(Failure::new(
                    "C1",
                    format!("{text:?}（{kind:?}）が読めない: {e}"),
                ));
                continue;
            }
        };
        if &parsed.canonical != expected {
            failures.push(Failure::new(
                "C1",
                format!(
                    "{text:?} の canonical が {:?}（期待は {expected:?}）",
                    parsed.canonical
                ),
            ));
        }
        if &parsed.driver != id {
            failures.push(Failure::new(
                "C1",
                format!(
                    "{text:?} の driver が {}（descriptor の id は {id}）",
                    parsed.driver
                ),
            ));
        }
        if parsed.word_count != kind.word_count() {
            failures.push(Failure::new(
                "C1",
                format!(
                    "{text:?}（{kind:?}）の word_count が {}（kind の語数は {}）",
                    parsed.word_count,
                    kind.word_count()
                ),
            ));
        }
        match driver.parse_address(&parsed.canonical, kind) {
            Ok(again) if again == parsed => {}
            Ok(again) => failures.push(Failure::new(
                "C1",
                format!(
                    "canonical {:?} を読み直すと別のアドレスになる: {again:?}（元は {parsed:?}）",
                    parsed.canonical
                ),
            )),
            Err(e) => failures.push(Failure::new(
                "C1",
                format!("canonical {:?} が読み直せない: {e}", parsed.canonical),
            )),
        }
        match driver.offset_address(&parsed.canonical, kind, 0) {
            Ok(same) if same == parsed.canonical => {}
            Ok(other) => failures.push(Failure::new(
                "C1",
                format!(
                    "offset_address({:?}, 0) が {other:?}（canonical のままであるべき）",
                    parsed.canonical
                ),
            )),
            Err(e) => failures.push(Failure::new(
                "C1",
                format!("offset_address({:?}, 0) が失敗: {e}", parsed.canonical),
            )),
        }
    }
    for (text, kind) in &fixtures.invalid {
        match driver.parse_address(text, kind) {
            Ok(a) => failures.push(Failure::new(
                "C1",
                format!("不正なはずの {text:?}（{kind:?}）が読めてしまった: {a:?}"),
            )),
            Err(e) if e.message().trim().is_empty() => failures.push(Failure::new(
                "C1",
                format!("{text:?} を断る AddressError の文言が空"),
            )),
            Err(_) => {}
        }
    }
    for (canonical, kind, n, expected) in &fixtures.offsets {
        let got = driver.offset_address(canonical, kind, *n);
        match (expected, &got) {
            (Some(want), Ok(have)) if want == have => {}
            (None, Err(_)) => {}
            _ => failures.push(Failure::new(
                "C1",
                format!("offset_address({canonical:?}, {kind:?}, {n}) が {got:?}（期待は {expected:?}）"),
            )),
        }
    }
}

fn c2_settings<D: DriverBase + ?Sized>(
    driver: &D,
    fixtures: &StaticFixtures,
    failures: &mut Vec<Failure>,
) {
    let minimal = &fixtures.settings;
    let errors = driver.validate_config(minimal);
    if !errors.is_empty() {
        failures.push(Failure::new(
            "C2",
            format!("最小の設定が通らない: {errors:?}"),
        ));
    }

    let mut with_unknown = minimal.clone();
    with_unknown.insert(UNKNOWN_KEY, "1");
    let errors = driver.validate_config(&with_unknown);
    if !errors.is_empty() {
        failures.push(Failure::new(
            "C2",
            format!("未知のキー {UNKNOWN_KEY:?} を断った（断ってはいけない）: {errors:?}"),
        ));
    }

    let errors = driver.validate_config(&Settings::new());
    for e in &errors {
        if minimal.get(&e.key).is_none() {
            failures.push(Failure::new(
                "C2",
                format!(
                    "空の設定への FieldError のキー {:?} が最小の設定に無い（必須のキーなら最小の設定に入れる）",
                    e.key
                ),
            ));
        }
    }
    if minimal.iter().next().is_some() && errors.is_empty() {
        // The minimal settings are not empty, so at least one of their keys
        // should be required; if none is, the author should give empty
        // minimal settings instead.
        failures.push(Failure::new(
            "C2",
            "空の設定が通った（最小の設定に必須でないキーが入っているなら外す）",
        ));
    }

    if minimal.get(banto_driver_api::keys::PORT).is_some() {
        let mut bad_port = minimal.clone();
        bad_port.insert(banto_driver_api::keys::PORT, "abc");
        let errors = driver.validate_config(&bad_port);
        if !errors.iter().any(|e| e.key == banto_driver_api::keys::PORT) {
            failures.push(Failure::new(
                "C2",
                format!("port = \"abc\" が port の FieldError にならない: {errors:?}"),
            ));
        }
    }

    let secrets = driver.secret_keys();
    for (i, key) in secrets.iter().enumerate() {
        if key.trim().is_empty() {
            failures.push(Failure::new("C2", "secret_keys に空のキーがある"));
        } else if secrets[..i].contains(key) {
            failures.push(Failure::new(
                "C2",
                format!("secret_keys の {key:?} が重複している"),
            ));
        } else if minimal.get(key).is_none() {
            failures.push(Failure::new(
                "C2",
                format!("secret_keys の {key:?} が最小の設定に無い（読むキーである証拠が無い）"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::echo::{self, EchoFrameDriver};
    use banto_driver_api::{
        AddressError, Capabilities, ChoiceOption, DataType, DriverAddress, DriverDescriptor,
        DriverId, FieldError, SettingField, TagKind,
    };

    #[test]
    fn the_echo_driver_passes() {
        let f = echo::fixtures(echo::FRAMING_FIXED);
        assert_eq!(static_checks(&EchoFrameDriver::new(), &f.statics), vec![]);
    }

    /// Breaks the static half on purpose: canonical not idempotent, unknown
    /// keys refused, a secret key it never reads.
    struct Sloppy(DriverDescriptor);

    impl DriverBase for Sloppy {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.0
        }
        fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
            settings
                .iter()
                .filter(|s| s.key != "host")
                .map(|s| FieldError::new(s.key.clone(), "知らない"))
                .collect()
        }
        fn secret_keys(&self) -> Vec<String> {
            vec!["password".into()]
        }
        fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
            // "canonical" grows on every parse.
            Ok(DriverAddress::new(
                self.0.id.clone(),
                format!("{text}!"),
                "x",
                true,
                kind.word_count(),
            ))
        }
        fn offset_address(&self, c: &str, _k: &TagKind, _n: i64) -> Result<String, AddressError> {
            Ok(c.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
    }

    /// Breaks C17-C22 on purpose: a duplicated key, a default outside its
    /// range, a secret that is not in `secret_keys`, a kind list that does
    /// not match the grammar, a word offset that loses the bit and a wrong
    /// index.
    struct Odd(DriverDescriptor);

    impl DriverBase for Odd {
        fn descriptor(&self) -> &DriverDescriptor {
            &self.0
        }
        fn validate_config(&self, settings: &Settings) -> Vec<FieldError> {
            // "port" is read, and refused unless it is "1".
            match settings.get("port") {
                Some("1") => Vec::new(),
                _ => vec![FieldError::new("port", "1 だけ")],
            }
        }
        fn parse_address(&self, text: &str, kind: &TagKind) -> Result<DriverAddress, AddressError> {
            // Numbers only: strings are refused, 64-bit types are accepted.
            if matches!(kind, TagKind::Str { .. }) || !text.starts_with('A') {
                return Err(AddressError::new("no"));
            }
            let n: u64 = text[1..]
                .split('.')
                .next()
                .and_then(|d| d.parse().ok())
                .ok_or_else(|| AddressError::new("no"))?;
            let a = DriverAddress::new(self.0.id.clone(), text, "A", true, kind.word_count())
                .with_index(n);
            Ok(match text.split_once('.') {
                Some((_, bit)) => a.with_bit(bit.parse().unwrap()),
                None => a,
            })
        }
        fn offset_address(&self, c: &str, _k: &TagKind, _n: i64) -> Result<String, AddressError> {
            Ok(c.to_owned())
        }
        fn address_help(&self) -> String {
            String::new()
        }
        fn settings_schema(&self) -> SettingsSchema {
            SettingsSchema::new(vec![
                SettingField::new("port", "ポート", FieldKind::integer(1, 10)).default(99),
                SettingField::new("port", "ポート 2", FieldKind::secret()),
                SettingField::new(
                    "mode",
                    "モード",
                    FieldKind::choice(vec![
                        ChoiceOption::new("a", "A"),
                        ChoiceOption::new("a", "A 2"),
                    ]),
                )
                .default("b"),
            ])
        }
        fn supported_kinds(&self) -> Vec<KindName> {
            // Claims strings (refused) and omits f64 (accepted).
            vec![
                KindName::Numeric(DataType::U16),
                KindName::String,
                KindName::Numeric(DataType::Bit),
            ]
        }
        fn offset_words(&self, c: &str, n: i64) -> Result<String, AddressError> {
            // Drops the bit and moves the index by n + 1.
            if n == 0 {
                return Ok(c.to_owned());
            }
            let number: i64 = c[1..].split('.').next().unwrap().parse().unwrap();
            Ok(format!("A{}", number + n + i64::from(n != 0)))
        }
    }

    #[test]
    fn an_odd_driver_fails_c17_to_c22() {
        let d = Odd(DriverDescriptor::new(
            DriverId::new("odd").unwrap(),
            "0",
            "O",
            Capabilities::READ_ONLY,
        ));
        let f = StaticFixtures::new(Settings::from_pairs([("port", "2")]).unwrap())
            .valid("A5.3", TagKind::numeric(DataType::Bit), "A5.3")
            .kind_example(DataType::U16, "A1")
            .kind_example(KindName::String, "A1")
            .kind_example(DataType::F64, "A1")
            .word_offset("A1", 1, Some("A2"));
        let failures = static_checks(&d, &f);
        let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
        let has = |needle: &str| text.iter().any(|t| t.contains(needle));
        assert!(has("C17: スキーマのキー \"port\" が重複"), "{text:#?}");
        assert!(has("C17: 欄 \"port\" の既定値"), "{text:#?}");
        assert!(
            has("C17: 欄 \"mode\" の選択肢の値 \"a\" が重複"),
            "{text:#?}"
        );
        assert!(has("C17: 欄 \"mode\" の既定値"), "{text:#?}");
        assert!(has("C18: secret_keys()"), "{text:#?}");
        assert!(
            has("C19: スキーマの既定値に最小の設定を重ねた設定が通らない"),
            "{text:#?}"
        );
        assert!(has("C20: supported_kinds にある string で"), "{text:#?}");
        assert!(has("C20: supported_kinds に無い f64 で"), "{text:#?}");
        assert!(has("C20: supported_kinds の bit の例が"), "{text:#?}");
        assert!(has("C21: offset_words(\"A1\", 1) が"), "{text:#?}");
        assert!(
            has("C21: offset_words(\"A5.3\", 1) でビットが保たれない")
                || has("C21: offset_words(\"A5.3\", 1) の"),
            "{text:#?}"
        );
    }

    #[test]
    fn an_index_that_does_not_follow_the_offset_fails_c22() {
        struct Skew(Odd);
        impl DriverBase for Skew {
            fn descriptor(&self) -> &DriverDescriptor {
                self.0.descriptor()
            }
            fn validate_config(&self, s: &Settings) -> Vec<FieldError> {
                self.0.validate_config(s)
            }
            fn parse_address(&self, t: &str, k: &TagKind) -> Result<DriverAddress, AddressError> {
                self.0.parse_address(t, k)
            }
            fn offset_address(&self, c: &str, k: &TagKind, n: i64) -> Result<String, AddressError> {
                self.0.offset_address(c, k, n)
            }
            fn address_help(&self) -> String {
                String::new()
            }
            fn offset_words(&self, c: &str, n: i64) -> Result<String, AddressError> {
                // Keeps the bit, but moves two words per step.
                let (number, bit) = c[1..].split_once('.').unwrap_or((&c[1..], ""));
                let number: i64 = number.parse().unwrap();
                let moved = number + 2 * n;
                Ok(if bit.is_empty() {
                    format!("A{moved}")
                } else {
                    format!("A{moved}.{bit}")
                })
            }
        }
        let d = Skew(Odd(DriverDescriptor::new(
            DriverId::new("odd").unwrap(),
            "0",
            "O",
            Capabilities::READ_ONLY,
        )));
        let f = StaticFixtures::new(Settings::new()).valid(
            "A5.3",
            TagKind::numeric(DataType::Bit),
            "A5.3",
        );
        let text: Vec<String> = static_checks(&d, &f)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(
            text.iter()
                .any(|t| t.starts_with("C22: offset_words(\"A5.3\", 1) の index")),
            "{text:#?}"
        );
    }

    #[test]
    fn a_sloppy_driver_fails_c1_and_c2() {
        let d = Sloppy(DriverDescriptor::new(
            DriverId::new("sloppy").unwrap(),
            "0",
            "S",
            Capabilities::READ_ONLY,
        ));
        let f = StaticFixtures::new(Settings::from_pairs([("host", "h")]).unwrap())
            .valid("A1", TagKind::numeric(DataType::U16), "A1!")
            .invalid("??", TagKind::numeric(DataType::U16))
            .offset("A1", TagKind::numeric(DataType::U16), 1, Some("A2"));
        let failures = static_checks(&d, &f);
        let text: Vec<String> = failures.iter().map(ToString::to_string).collect();
        let has = |needle: &str| text.iter().any(|t| t.contains(needle));
        assert!(
            has("C1: canonical \"A1!\" を読み直すと別のアドレスになる"),
            "{text:#?}"
        );
        assert!(has("C1: 不正なはずの \"??\""), "{text:#?}");
        assert!(has("C1: offset_address(\"A1\""), "{text:#?}");
        assert!(has("C2: 未知のキー"), "{text:#?}");
        assert!(has("C2: 空の設定が通った"), "{text:#?}");
        assert!(has("C2: secret_keys の \"password\""), "{text:#?}");
    }
}
