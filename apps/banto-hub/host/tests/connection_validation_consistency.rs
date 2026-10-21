//! 平らな形の旧い検証とレジストリの検証の一致の表（R2b-1c、
//! docs/banto-hub-connection-settings-design.md §4.2・§9「平らな形とレジストリの一致」）。
//!
//! R2b-1d で検証の正がレジストリになるとき、「今の画面とテストが見るエラーの文言は
//! 変わらない」ために、組み込みのプロトコルについて次の 2 つが**同じ入力を断る**ことを、
//! 境界値の表で固定する:
//!
//! - 旧: `PlcConnectionService::create` の入口の `validate_plc_connection_input`
//!   （非公開なので、実際の書き込みの入口を通す。DB は `banto_tags::migrate` を流した
//!   メモリの SQLite）。
//! - 新: `legacy::validate_flat_with_registry`（正規化 → `settings_from_flat` →
//!   `settings_to_api` → 実ドライバの `validate_config` → REST の綴りの項目名）。レジストリは
//!   `tests/common` の `hub_drivers()`（`banto_drivers::builtin()` + `host_provided_drivers()`、
//!   本番の host と同じ組み立て）。
//!
//! 比べるのは**拒否の有無と項目名の集合**。文言は別の表（[`message_differences`]）で、
//! 食い違っているものを**そのまま固定**する（直すかどうかは設計の判断、報告済み）。
//! 表の各行は `Verdict` を持ち、実際の結果と一致しないと落ちる。食い違いが増えても減っても
//! 落ちるので、どちらかの検証を変えたら表を見直すことになる。

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU32, Ordering};

use banto_core::BantoError;
use banto_driver_api::DriverRegistry;
use banto_tags::settings_json::legacy::validate_flat_with_registry;
use banto_tags::{PlcConnectionInput, PlcConnectionService};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;

/// 行の期待。`Agree` 以外は「食い違い」で、理由を持つ。
#[derive(Debug, Clone, Copy)]
enum Verdict {
    /// 旧と新が、拒否の有無も項目名の集合も同じ。
    Agree,
    /// 旧だけが断る。プロトコルが読まない欄（`settings_from_flat` が捨てる）の検証で、
    /// 設計 §4.2 の順序（(1) 旧い平らな検証が先）が断るので、書き込みの結果は同じ。
    DroppedFieldOldOnly(&'static str),
    /// 旧と新の食い違い（書き込みの結果が変わる）。理由は報告に載せる。
    Discrepancy(&'static str),
}

impl Verdict {
    fn reason(&self) -> &'static str {
        match self {
            Verdict::Agree => "agree",
            Verdict::DroppedFieldOldOnly(reason) | Verdict::Discrepancy(reason) => reason,
        }
    }
}

struct Row {
    label: String,
    input: PlcConnectionInput,
    verdict: Verdict,
}

static NAME_SEQ: AtomicU32 = AtomicU32::new(0);

fn base(protocol: &str) -> PlcConnectionInput {
    let n = NAME_SEQ.fetch_add(1, Ordering::SeqCst);
    let is_postgres = protocol == "postgres";
    PlcConnectionInput {
        name: format!("conn-{n}"),
        protocol: protocol.to_string(),
        host: "127.0.0.1".to_string(),
        port: 5000,
        unit_id: 1,
        enabled: true,
        simulation: false,
        word_order: String::new(),
        cpu_type: None,
        database: is_postgres.then(|| "appdb".to_string()),
        username: is_postgres.then(|| "alice".to_string()),
        password: None,
        settings: None,
    }
}

fn with(protocol: &str, edit: impl FnOnce(&mut PlcConnectionInput)) -> PlcConnectionInput {
    let mut input = base(protocol);
    edit(&mut input);
    input
}

fn field_set(errors: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    errors.into_iter().collect()
}

async fn memory_service(registry: std::sync::Arc<DriverRegistry>) -> PlcConnectionService {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("memory sqlite");
    banto_tags::migrate(&pool).await.expect("migrate");
    // `settings.bridge.db_connection_id` が指す postgres 接続（id = 1）。
    let service = PlcConnectionService::with_registry(pool, registry);
    service
        .create(base("postgres"))
        .await
        .expect("bridge target postgres connection");
    service
}

/// 旧い検証の `(field, message)`。通れば空（行は作られる。名前は行ごとに別）。
async fn old_errors(
    service: &PlcConnectionService,
    input: &PlcConnectionInput,
) -> Vec<(String, String)> {
    match service.create(input.clone()).await {
        Ok(_) => Vec::new(),
        Err(BantoError::Validation { field_errors }) => field_errors
            .into_iter()
            .map(|e| (e.field, e.message))
            .collect(),
        Err(other) => panic!("{}: unexpected error {other:?}", input.name),
    }
}

fn new_errors(registry: &DriverRegistry, input: &PlcConnectionInput) -> Vec<(String, String)> {
    validate_flat_with_registry(registry, input.clone())
        .into_iter()
        .map(|e| (e.field, e.message))
        .collect()
}

fn fields(errors: &[(String, String)]) -> BTreeSet<String> {
    field_set(errors.iter().map(|(field, _)| field.clone()))
}

fn long(n: usize) -> String {
    "a".repeat(n)
}

fn bridge(period_ms: u64) -> Value {
    json!({
        "stale_timeout_ms": 5000,
        "bridge": {
            "kind": "db",
            "db_connection_id": 1,
            "queries": [{
                "name": "q", "period_ms": period_ms, "sql": "SELECT 1 AS v",
                "columns": [{"column": "v", "key": "k"}]
            }]
        }
    })
}

#[allow(clippy::too_many_lines)]
fn rows() -> Vec<Row> {
    use Verdict::{Agree, Discrepancy, DroppedFieldOldOnly};
    let mut rows = Vec::new();
    let mut add = |label: String, input: PlcConnectionInput, verdict: Verdict| {
        rows.push(Row {
            label,
            input,
            verdict,
        });
    };

    // ---- modbus-tcp / slmp / postgres 共通の host・port ----
    for protocol in ["modbus-tcp", "slmp", "postgres"] {
        add(format!("{protocol}: valid"), base(protocol), Agree);
        for port in [0, 1, 65535, 65536, -1] {
            add(
                format!("{protocol}: port {port}"),
                with(protocol, |i| i.port = port),
                Agree,
            );
        }
        for host in ["", "  ", "\t\n", " h ", "h"] {
            add(
                format!("{protocol}: host {host:?}"),
                with(protocol, |i| i.host = host.to_string()),
                Agree,
            );
        }
    }

    // ---- unit_id（modbus-tcp だけが読む）----
    for unit_id in [-1, 0, 255, 256] {
        add(
            format!("modbus-tcp: unit_id {unit_id}"),
            with("modbus-tcp", |i| i.unit_id = unit_id),
            Agree,
        );
        add(
            format!("slmp: unit_id {unit_id} (dropped)"),
            with("slmp", |i| i.unit_id = unit_id),
            if (0..=255).contains(&unit_id) {
                Agree
            } else {
                DroppedFieldOldOnly(
                    "slmp は unit_id を読まない: 旧は範囲を検証する（設計 §3.2 で捨てる欄）",
                )
            },
        );
        // postgres は正規化が unit_id を既定に揃えるので、旧も新も断らない。
        add(
            format!("postgres: unit_id {unit_id} (normalized)"),
            with("postgres", |i| i.unit_id = unit_id),
            Agree,
        );
    }

    // ---- word_order（modbus-tcp・slmp が読む）----
    for protocol in ["modbus-tcp", "slmp"] {
        for word_order in [
            "",
            "low_high",
            "high_low",
            "LOW_HIGH",
            "low-high",
            " low_high",
            "lowhigh",
        ] {
            add(
                format!("{protocol}: word_order {word_order:?}"),
                with(protocol, |i| i.word_order = word_order.to_string()),
                Agree,
            );
        }
    }

    // ---- cpu_type（slmp が読む。None・"" は未指定）----
    for cpu_type in [
        None,
        Some(""),
        Some("Q"),
        Some("R"),
        Some("q"),
        Some("L"),
        Some("QQ"),
        Some(" Q"),
    ] {
        add(
            format!("slmp: cpu_type {cpu_type:?}"),
            with("slmp", |i| i.cpu_type = cpu_type.map(str::to_string)),
            Agree,
        );
        let valid = matches!(cpu_type, None | Some("" | "Q" | "R"));
        add(
            format!("modbus-tcp: cpu_type {cpu_type:?} (dropped)"),
            with("modbus-tcp", |i| i.cpu_type = cpu_type.map(str::to_string)),
            if valid {
                Agree
            } else {
                DroppedFieldOldOnly("modbus-tcp は cpu_type を読まない: 旧は全プロトコルで検証する")
            },
        );
    }

    // ---- postgres の database・username（128 文字まで、前後の空白は除く）----
    for key in ["database", "username"] {
        let set = |i: &mut PlcConnectionInput, v: Option<String>| {
            if key == "database" {
                i.database = v;
            } else {
                i.username = v;
            }
        };
        let cases: Vec<(&str, Option<String>)> = vec![
            ("none", None),
            ("empty", Some(String::new())),
            ("spaces", Some("   ".to_string())),
            ("127", Some(long(127))),
            ("128", Some(long(128))),
            ("129", Some(long(129))),
            ("128 padded", Some(format!(" {} ", long(128)))),
            ("129 padded", Some(format!(" {} ", long(129)))),
            ("128 multibyte", Some("あ".repeat(128))),
            ("129 multibyte", Some("あ".repeat(129))),
        ];
        for (name, value) in cases {
            add(
                format!("postgres: {key} {name}"),
                with("postgres", |i| set(i, value.clone())),
                Agree,
            );
        }
    }

    // ---- postgres の password: 検証に形の規則は無い（秘密のキーは検証に渡らない）----
    for password in [None, Some(""), Some("pw"), Some("   ")] {
        add(
            format!("postgres: password {password:?}"),
            with("postgres", |i| i.password = password.map(str::to_string)),
            Agree,
        );
    }

    // ---- postgres 以外に database・username・password を付ける（旧は断る。読まない欄）----
    for protocol in ["modbus-tcp", "slmp", "virtual", "push"] {
        add(
            format!("{protocol}: database set (dropped)"),
            with(protocol, |i| i.database = Some("d".to_string())),
            DroppedFieldOldOnly(
                "postgres 以外の database / username / password は旧い検証が断る（設計 §4.2 (1)）",
            ),
        );
        add(
            format!("{protocol}: password set (dropped)"),
            with(protocol, |i| i.password = Some("pw".to_string())),
            DroppedFieldOldOnly(
                "postgres 以外の database / username / password は旧い検証が断る（設計 §4.2 (1)）",
            ),
        );
    }

    // ---- virtual: host・port は検証されない。unit_id・word_order は旧が検証する ----
    add(
        "virtual: valid".into(),
        with("virtual", |i| {
            i.host = String::new();
            i.port = 0;
        }),
        Agree,
    );
    add(
        "virtual: host/port junk".into(),
        with("virtual", |i| {
            i.host = " x ".into();
            i.port = -5;
        }),
        Agree,
    );
    add(
        "virtual: unit_id 256 (dropped)".into(),
        with("virtual", |i| i.unit_id = 256),
        DroppedFieldOldOnly("virtual は unit_id を読まない（正規化もしない）: 旧は範囲を検証する"),
    );
    add(
        "virtual: word_order junk (dropped)".into(),
        with("virtual", |i| i.word_order = "x".into()),
        DroppedFieldOldOnly("virtual は word_order を読まない（正規化もしない）: 旧は検証する"),
    );
    add(
        "virtual: settings set (dropped)".into(),
        with("virtual", |i| i.settings = Some(json!({"a": 1}))),
        DroppedFieldOldOnly("settings は push 以外には指定できない: 旧い検証が断る。virtual のドライバは何も断らない"),
    );
    // simulation + virtual は旧い検証の規則で、settings に写らない。
    add(
        "virtual: simulation (not a setting)".into(),
        with("virtual", |i| i.simulation = true),
        DroppedFieldOldOnly("simulation は settings の外（列）。旧い検証だけが見る"),
    );

    // ---- push の settings ----
    add("push: settings none".into(), base("push"), Agree);
    add(
        "push: settings {}".into(),
        with("push", |i| i.settings = Some(json!({}))),
        Agree,
    );
    for ms in [0u64, 999, 1000, 3_600_000, 3_600_001] {
        add(
            format!("push: stale_timeout_ms {ms}"),
            with("push", |i| {
                i.settings = Some(json!({"stale_timeout_ms": ms}))
            }),
            Agree,
        );
    }
    add(
        "push: stale_timeout_ms -1".into(),
        with("push", |i| {
            i.settings = Some(json!({"stale_timeout_ms": -1}))
        }),
        Agree,
    );
    add(
        "push: stale_timeout_ms 1.5".into(),
        with("push", |i| {
            i.settings = Some(json!({"stale_timeout_ms": 1.5}))
        }),
        Agree,
    );
    add(
        "push: stale_timeout_ms \"5000\" (string)".into(),
        with("push", |i| i.settings = Some(json!({"stale_timeout_ms": "5000"}))),
        Discrepancy("stale_timeout_ms が文字列 \"5000\": 旧は断る（整数でない）。新は 10 進の文字列として受かる（Settings は型を持たない）"),
    );
    add(
        "push: stale_timeout_ms true".into(),
        with("push", |i| {
            i.settings = Some(json!({"stale_timeout_ms": true}))
        }),
        Agree,
    );
    add(
        "push: unknown key".into(),
        with("push", |i| i.settings = Some(json!({"nope": 1}))),
        Discrepancy("未知のキー: 旧は断る（reject_unknown_keys）。ドライバは未知のキーを断らない（DriverBase の約束・適合テスト C2）"),
    );
    add(
        "push: host key (other layer's key)".into(),
        with("push", |i| i.settings = Some(json!({"host": "x"}))),
        Discrepancy("未知のキー（host）: 旧は断る。ドライバは断らない"),
    );
    add(
        "push: valid bridge".into(),
        with("push", |i| i.settings = Some(bridge(1000))),
        Agree,
    );
    add(
        "push: bridge period 300".into(),
        with("push", |i| i.settings = Some(bridge(300))),
        Agree,
    );
    add(
        "push: bridge not an object".into(),
        with("push", |i| i.settings = Some(json!({"bridge": 5}))),
        Agree,
    );
    add(
        "push: bridge as a JSON string".into(),
        with("push", |i| i.settings = Some(json!({"bridge": bridge(1000)["bridge"].to_string()}))),
        Discrepancy("bridge が JSON を入れた文字列: 旧は断る（オブジェクトでない）。新は文字列をそのまま JSON として読んで受かる"),
    );
    add(
        "push: settings not an object (array)".into(),
        with("push", |i| i.settings = Some(json!([1]))),
        Discrepancy("settings がオブジェクトでない: 旧は断る。settings_from_flat は {} にして検証に渡す（旧い検証が先に断る前提）"),
    );
    add(
        "push: settings null".into(),
        with("push", |i| i.settings = Some(Value::Null)),
        Discrepancy("settings が null: 旧は断る（オブジェクトでない）。settings_from_flat は null を {} にする（§3.2「NULL なら {}」）"),
    );
    // push の host・port・unit_id は正規化が既定に揃える。
    add(
        "push: host/port/unit_id junk (normalized)".into(),
        with("push", |i| {
            i.host = "x".into();
            i.port = 99_999;
            i.unit_id = 999;
        }),
        Agree,
    );

    // ---- hub / 未知 / 綴り ----
    add(
        "hub".into(),
        base("hub"),
        Discrepancy("hub: 旧の許可リストに含まれて受かる。Hub のレジストリには hub が無い（設計 §4.1）ので protocol の項目エラー。Hub の REST は reject_hub_connection_protocol で元から断る"),
    );
    for protocol in ["bogus", "", "SLMP", "Modbus-TCP", "modbus", "slmp "] {
        add(format!("protocol {protocol:?}"), base(protocol), Agree);
    }
    rows
}

#[tokio::test]
async fn the_old_flat_validation_and_the_registry_reject_the_same_inputs() {
    let drivers = common::hub_drivers();
    let registry = drivers.registry.clone();
    let service = memory_service(registry.clone()).await;

    let mut report = Vec::new();
    for row in rows() {
        let old = old_errors(&service, &row.input).await;
        let new = new_errors(&registry, &row.input);
        let (old_fields, new_fields) = (fields(&old), fields(&new));
        let agree = old_fields == new_fields;
        let ok = match row.verdict {
            Verdict::Agree => agree,
            Verdict::DroppedFieldOldOnly(_) => !old.is_empty() && !agree,
            Verdict::Discrepancy(_) => !agree,
        };
        if !ok {
            report.push(format!(
                "{}: verdict {:?} ({}) but old={old_fields:?} new={new_fields:?}\n  old messages: {old:?}\n  new messages: {new:?}",
                row.label,
                row.verdict,
                row.verdict.reason()
            ));
        }
    }
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}

/// 項目名が同じ行で、文言が違うものを固定する。「今の画面とテストが見るエラーの文言は
/// 変わらない」（設計 §4.2）を守るには、R2b-1d の前に次のどちらかが要る: 旧い検証の文言に
/// ドライバ側を揃える、またはこの表の `new` に旧い検証の文言を載せ替える（`validate_*_with_
/// registry` で `error.key` ごとに文言を持つ）。判断は設計の側（報告済み）で、ここは現状を
/// 固定するだけ。postgres（R2b-1a で旧い規則を文言ごと写した）は食い違いが無い。
#[tokio::test]
async fn message_differences_are_exactly_the_known_ones() {
    let drivers = common::hub_drivers();
    let registry = drivers.registry.clone();
    let service = memory_service(registry.clone()).await;
    let mut seen = BTreeSet::new();
    for row in rows() {
        let old = old_errors(&service, &row.input).await;
        let new = new_errors(&registry, &row.input);
        if fields(&old) != fields(&new) {
            continue;
        }
        for (field, old_message) in &old {
            for (new_field, new_message) in &new {
                if new_field == field && new_message != old_message {
                    seen.insert(format!("{field} | {old_message} | {new_message}"));
                }
            }
        }
    }
    let want: BTreeSet<String> = [
        "host | 必須項目です | host を指定してください",
        "port | 1〜65535の範囲で入力してください | port は 1〜65535 の整数で指定してください",
        "unitId | 0〜255の範囲で入力してください | unit_id は 0〜255 の整数で指定してください",
        "wordOrder | ワード順は low_high, high_low のいずれかです | word_order は low_high / high_low のどちらかで指定してください",
        "cpuType | CPU種別は Q, R のいずれかです | cpu_type は Q / R のどちらかで指定してください",
        "settings.stale_timeout_ms | 1000〜3600000の範囲で入力してください | stale_timeout_ms は整数（ミリ秒）で指定してください",
        "protocol | 対応プロトコルは modbus-tcp, slmp, virtual, postgres, hub, push のいずれかです | 対応プロトコルは modbus-tcp, postgres, push, slmp, virtual のいずれかです",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    assert_eq!(
        seen,
        want,
        "\n{}",
        seen.iter().cloned().collect::<Vec<_>>().join("\n")
    );
}
