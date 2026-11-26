//! 筋書きの共通の試験ケース（TypeScript の `apps/banto-scada/src/scenario/scenario.test.ts`
//! も同じものを通す）。

use std::fs;
use std::path::{Path, PathBuf};

use banto_scenario::{Scenario, ScenarioValue};
use serde_json::Value;

const TOLERANCE: f64 = 1e-9;

fn tests_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn read_json(path: &Path) -> Value {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn shared_cases_give_the_expected_values_at_every_sample() {
    let dir = tests_dir().join("cases");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .expect("cases")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();
    assert!(files.len() > 1);
    for file in files {
        let data = read_json(&file);
        let scenario_json = match data.get("scenarioFile").and_then(Value::as_str) {
            Some(relative) => read_json(&dir.join(relative)),
            None => data["scenario"].clone(),
        };
        let scenario = Scenario::from_json(&scenario_json)
            .unwrap_or_else(|error| panic!("{}: {error}", file.display()));
        assert_eq!(
            scenario.frozen_time_sec(None),
            data["frozenTimeSec"].as_f64(),
            "{}",
            file.display()
        );
        let samples = data["samples"].as_array().expect("samples");
        assert!(!samples.is_empty());
        for sample in samples {
            let t_sec = sample["tSec"].as_f64().expect("tSec");
            let where_ = format!("{} t={t_sec}", file.display());
            let expected = sample["values"].as_object().expect("values");
            let actual = scenario.evaluate(t_sec);
            assert_eq!(
                actual.keys().collect::<Vec<_>>(),
                {
                    let mut keys: Vec<_> = expected.keys().collect();
                    keys.sort();
                    keys
                },
                "{where_}"
            );
            for (name, expected) in expected {
                match (actual[name], expected) {
                    (ScenarioValue::Bit(bit), Value::Bool(want)) => {
                        assert_eq!(bit, *want, "{where_} {name}")
                    }
                    (ScenarioValue::Number(n), Value::Number(want)) => {
                        let want = want.as_f64().expect("number");
                        assert!(
                            (n - want).abs() <= TOLERANCE,
                            "{where_} {name}: {n} != {want}"
                        );
                    }
                    (value, want) => panic!("{where_} {name}: {value:?} != {want}"),
                }
            }
        }
    }
}

#[test]
fn every_invalid_scenario_is_refused() {
    let cases = read_json(&tests_dir().join("invalid.json"));
    let cases = cases.as_array().expect("array");
    assert!(cases.len() > 10);
    for case in cases {
        let name = case["name"].as_str().expect("name");
        assert!(Scenario::from_json(&case["scenario"]).is_err(), "{name}");
    }
}

#[test]
fn the_later_event_wins_where_events_overlap() {
    let scenario = Scenario::from_json_str(
        r#"{"format":"banto-scenario","version":1,"cycleSec":10,
            "signals":{"A":{"kind":"number","base":1}},
            "events":[{"atSec":2,"durationSec":4,"values":{"A":2}},
                      {"atSec":4,"durationSec":4,"values":{"A":3}}]}"#,
    )
    .expect("scenario");
    let at = |t: f64| scenario.evaluate(t)["A"];
    assert_eq!(at(3.0), ScenarioValue::Number(2.0));
    assert_eq!(at(5.0), ScenarioValue::Number(3.0));
    assert_eq!(at(7.0), ScenarioValue::Number(3.0));
    assert_eq!(at(9.0), ScenarioValue::Number(1.0));
    assert_eq!(scenario.frozen_time_sec(None), None);
    assert_eq!(scenario.frozen_time_sec(Some(7.0)), Some(7.0));
}
