//! C1 and C2: the static half of a driver (docs/banto-driver-api-design.md
//! §7.1). No I/O and no runtime.

use banto_driver_api::{DriverBase, Settings};

use crate::{Failure, StaticFixtures};

/// A key no driver reads: C2 adds it and expects no complaint.
const UNKNOWN_KEY: &str = "x-banto-conformance-unknown";

/// Run C1 (addresses) and C2 (settings) against `driver`'s [`DriverBase`]
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
pub fn static_checks<D: DriverBase + ?Sized>(
    driver: &D,
    fixtures: &StaticFixtures,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    c1_addresses(driver, fixtures, &mut failures);
    c2_settings(driver, fixtures, &mut failures);
    failures
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
        AddressError, Capabilities, DataType, DriverAddress, DriverDescriptor, DriverId,
        FieldError, TagKind,
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
