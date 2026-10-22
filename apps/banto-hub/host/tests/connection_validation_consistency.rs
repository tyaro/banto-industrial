//! 平らな形の旧い検証とレジストリの検証の一致の表（R2b-1c、
//! docs/banto-hub-connection-settings-design.md §4.2・§9「平らな形とレジストリの一致」）。
//!
//! R2b-1d で検証の正がレジストリになるとき、「今の画面とテストが見るエラーの文言は
//! 変わらない」ために、組み込みのプロトコルについて次の 2 つが**同じ入力を断る**ことを、
//! 境界値の表で固定する:
//!
//! - 旧: `PlcConnectionService::create` の入口の `validate_plc_connection_input`
//!   （非公開なので、実際の書き込みの入口を通す。DB は `banto_tags::migrate` を流した
//!   メモリの SQLite。R2b-1d から `with_registry` は (1) の後にレジストリも走らせるので、
//!   旧規則だけの `PlcConnectionService::new` を通す）。
//! - 新: `legacy::validate_flat_with_registry`（正規化 → `settings_from_flat` →
//!   `settings_to_api` → 実ドライバの `validate_config` → REST の綴りの項目名）。レジストリは
//!   `tests/common` の `hub_drivers()`（`banto_drivers::builtin()` + `host_provided_drivers()`、
//!   本番の host と同じ組み立て）。
//!
//! 比べるのは**拒否の有無と項目名の集合**。文言は別の表（[`message_differences`]）で、
//! 食い違っているものを**そのまま固定**する（直すかどうかは設計の判断、報告済み）。
//! 表の各行は `Verdict` と、旧・新それぞれが断る項目名の集合の**期待そのもの**（`Expect`）を持ち、
//! 実際の結果と一致しないと落ちる。食い違いの行も「どの項目を旧が断り、新が断らないか」まで
//! 固定するので、食い違いが別の食い違いに変わっても、増えても減っても落ちる。どちらかの検証を
//! 変えたら表を見直すことになる。

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

/// 行の期待。判定と、旧・新が断る項目名（REST の綴り）の集合の正確な期待。
struct Expect {
    verdict: Verdict,
    old: BTreeSet<&'static str>,
    new: BTreeSet<&'static str>,
}

fn set(fields: &[&'static str]) -> BTreeSet<&'static str> {
    fields.iter().copied().collect()
}

/// 旧も新も何も断らない。
fn ok() -> Expect {
    agree_on(&[])
}

/// 旧も新も同じ項目の集合を断る。
fn agree_on(fields: &[&'static str]) -> Expect {
    Expect {
        verdict: Verdict::Agree,
        old: set(fields),
        new: set(fields),
    }
}

/// `bad` なら旧も新も `field` を断り、そうでなければどちらも断らない。
fn agree_if(bad: bool, field: &'static str) -> Expect {
    if bad {
        agree_on(&[field])
    } else {
        ok()
    }
}

/// 旧だけが `old` を断り、新は何も断らない（読まない欄）。
fn dropped(reason: &'static str, old: &[&'static str]) -> Expect {
    Expect {
        verdict: Verdict::DroppedFieldOldOnly(reason),
        old: set(old),
        new: BTreeSet::new(),
    }
}

/// 旧が `old`、新が `new` を断る食い違い。
fn discrepancy(reason: &'static str, old: &[&'static str], new: &[&'static str]) -> Expect {
    Expect {
        verdict: Verdict::Discrepancy(reason),
        old: set(old),
        new: set(new),
    }
}

struct Row {
    label: String,
    input: PlcConnectionInput,
    expect: Expect,
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
        secrets: Default::default(),
        settings: None,
    }
}

/// R2b-1d: the REST tri-state of `password`, sealed the way banto-hub's entrances seal it.
fn sealed_password(password: Option<&str>) -> banto_tags::SecretChanges {
    banto_hub_core::secrets::password_changes(
        &banto_hub_core::secrets::ConnectionSecrets::platform_default(),
        password,
    )
    .expect("seal")
}

fn with(protocol: &str, edit: impl FnOnce(&mut PlcConnectionInput)) -> PlcConnectionInput {
    let mut input = base(protocol);
    edit(&mut input);
    input
}

fn field_set(errors: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    errors.into_iter().collect()
}

/// 書き込みの入口を通すメモリの SQLite のサービス。`registry` が `None` なら旧規則だけ
/// （`PlcConnectionService::new`、設計 §4.2 の (1)）、`Some` なら R2b-1d の配線
/// （`with_registry`: (1) が通ったときだけ (2)〜(4)）。
async fn memory_service(registry: Option<std::sync::Arc<DriverRegistry>>) -> PlcConnectionService {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .expect("memory sqlite");
    banto_tags::migrate(&pool).await.expect("migrate");
    // `settings.bridge.db_connection_id` が指す postgres 接続（id = 1）。
    let service = match registry {
        Some(registry) => PlcConnectionService::with_registry(pool, registry),
        None => PlcConnectionService::new(pool),
    };
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
    let mut rows = Vec::new();
    let mut add = |label: String, input: PlcConnectionInput, expect: Expect| {
        rows.push(Row {
            label,
            input,
            expect,
        });
    };

    // ---- modbus-tcp / slmp / postgres 共通の host・port ----
    for protocol in ["modbus-tcp", "slmp", "postgres"] {
        add(format!("{protocol}: valid"), base(protocol), ok());
        for port in [0, 1, 65535, 65536, -1] {
            add(
                format!("{protocol}: port {port}"),
                with(protocol, |i| i.port = port),
                agree_if(!(1..=65535).contains(&port), "port"),
            );
        }
        for host in ["", "  ", "\t\n", " h ", "h"] {
            add(
                format!("{protocol}: host {host:?}"),
                with(protocol, |i| i.host = host.to_string()),
                agree_if(host.trim().is_empty(), "host"),
            );
        }
    }

    // ---- unit_id（modbus-tcp だけが読む）----
    for unit_id in [-1, 0, 255, 256] {
        add(
            format!("modbus-tcp: unit_id {unit_id}"),
            with("modbus-tcp", |i| i.unit_id = unit_id),
            agree_if(!(0..=255).contains(&unit_id), "unitId"),
        );
        add(
            format!("slmp: unit_id {unit_id} (dropped)"),
            with("slmp", |i| i.unit_id = unit_id),
            if (0..=255).contains(&unit_id) {
                ok()
            } else {
                dropped(
                    "slmp は unit_id を読まない: 旧は範囲を検証する（設計 §3.2 で捨てる欄）",
                    &["unitId"],
                )
            },
        );
        // postgres は正規化が unit_id を既定に揃えるので、旧も新も断らない。
        add(
            format!("postgres: unit_id {unit_id} (normalized)"),
            with("postgres", |i| i.unit_id = unit_id),
            ok(),
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
                agree_if(
                    !matches!(word_order, "" | "low_high" | "high_low"),
                    "wordOrder",
                ),
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
        let valid = matches!(cpu_type, None | Some("" | "Q" | "R"));
        add(
            format!("slmp: cpu_type {cpu_type:?}"),
            with("slmp", |i| i.cpu_type = cpu_type.map(str::to_string)),
            agree_if(!valid, "cpuType"),
        );
        add(
            format!("modbus-tcp: cpu_type {cpu_type:?} (dropped)"),
            with("modbus-tcp", |i| i.cpu_type = cpu_type.map(str::to_string)),
            if valid {
                ok()
            } else {
                dropped(
                    "modbus-tcp は cpu_type を読まない: 旧は全プロトコルで検証する",
                    &["cpuType"],
                )
            },
        );
    }

    // ---- postgres の database・username（128 文字まで、前後の空白は除く）----
    for key in ["database", "username"] {
        let key: &'static str = key;
        let set_field = |i: &mut PlcConnectionInput, v: Option<String>| {
            if key == "database" {
                i.database = v;
            } else {
                i.username = v;
            }
        };
        // (名前, 値, 断られるはずか)
        let cases: Vec<(&str, Option<String>, bool)> = vec![
            ("none", None, true),
            ("empty", Some(String::new()), true),
            ("spaces", Some("   ".to_string()), true),
            ("127", Some(long(127)), false),
            ("128", Some(long(128)), false),
            ("129", Some(long(129)), true),
            ("128 padded", Some(format!(" {} ", long(128))), false),
            ("129 padded", Some(format!(" {} ", long(129))), true),
            ("128 multibyte", Some("あ".repeat(128)), false),
            ("129 multibyte", Some("あ".repeat(129)), true),
        ];
        for (name, value, bad) in cases {
            add(
                format!("postgres: {key} {name}"),
                with("postgres", |i| set_field(i, value.clone())),
                agree_if(bad, key),
            );
        }
    }

    // ---- postgres の password: 検証に形の規則は無い（秘密のキーは検証に渡らない）----
    for password in [None, Some(""), Some("pw"), Some("   ")] {
        add(
            format!("postgres: password {password:?}"),
            with("postgres", |i| i.secrets = sealed_password(password)),
            ok(),
        );
    }

    // ---- postgres 以外に database・username・password を付ける（旧は断る。読まない欄）----
    for protocol in ["modbus-tcp", "slmp", "virtual", "push"] {
        add(
            format!("{protocol}: database set (dropped)"),
            with(protocol, |i| i.database = Some("d".to_string())),
            dropped(
                "postgres 以外の database / username / password は旧い検証が断る（設計 §4.2 (1)）",
                &["database"],
            ),
        );
        add(
            format!("{protocol}: password set (dropped)"),
            with(protocol, |i| i.secrets = sealed_password(Some("pw"))),
            dropped(
                "postgres 以外の database / username / password は旧い検証が断る（設計 §4.2 (1)）",
                &["password"],
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
        ok(),
    );
    add(
        "virtual: host/port junk".into(),
        with("virtual", |i| {
            i.host = " x ".into();
            i.port = -5;
        }),
        ok(),
    );
    add(
        "virtual: unit_id 256 (dropped)".into(),
        with("virtual", |i| i.unit_id = 256),
        dropped(
            "virtual は unit_id を読まない（正規化もしない）: 旧は範囲を検証する",
            &["unitId"],
        ),
    );
    add(
        "virtual: word_order junk (dropped)".into(),
        with("virtual", |i| i.word_order = "x".into()),
        dropped(
            "virtual は word_order を読まない（正規化もしない）: 旧は検証する",
            &["wordOrder"],
        ),
    );
    add(
        "virtual: settings set (dropped)".into(),
        with("virtual", |i| i.settings = Some(json!({"a": 1}))),
        dropped(
            "settings は push 以外には指定できない: 旧い検証が断る。virtual のドライバは何も断らない",
            &["settings"],
        ),
    );
    // simulation + virtual は旧い検証の規則で、settings に写らない。
    add(
        "virtual: simulation (not a setting)".into(),
        with("virtual", |i| i.simulation = true),
        dropped(
            "simulation は settings の外（列）。旧い検証だけが見る",
            &["simulation"],
        ),
    );

    // ---- push の settings ----
    add("push: settings none".into(), base("push"), ok());
    add(
        "push: settings {}".into(),
        with("push", |i| i.settings = Some(json!({}))),
        ok(),
    );
    for ms in [0u64, 999, 1000, 3_600_000, 3_600_001] {
        add(
            format!("push: stale_timeout_ms {ms}"),
            with("push", |i| {
                i.settings = Some(json!({"stale_timeout_ms": ms}))
            }),
            agree_if(
                !(1000..=3_600_000).contains(&ms),
                "settings.stale_timeout_ms",
            ),
        );
    }
    add(
        "push: stale_timeout_ms -1".into(),
        with("push", |i| {
            i.settings = Some(json!({"stale_timeout_ms": -1}))
        }),
        agree_on(&["settings.stale_timeout_ms"]),
    );
    add(
        "push: stale_timeout_ms 1.5".into(),
        with("push", |i| {
            i.settings = Some(json!({"stale_timeout_ms": 1.5}))
        }),
        agree_on(&["settings.stale_timeout_ms"]),
    );
    add(
        "push: stale_timeout_ms \"5000\" (string)".into(),
        with("push", |i| i.settings = Some(json!({"stale_timeout_ms": "5000"}))),
        discrepancy("stale_timeout_ms が文字列 \"5000\": 旧は断る（整数でない）。新は 10 進の文字列として受かる（Settings は型を持たない）", &["settings.stale_timeout_ms"], &[]),
    );
    add(
        "push: stale_timeout_ms true".into(),
        with("push", |i| {
            i.settings = Some(json!({"stale_timeout_ms": true}))
        }),
        agree_on(&["settings.stale_timeout_ms"]),
    );
    add(
        "push: unknown key".into(),
        with("push", |i| i.settings = Some(json!({"nope": 1}))),
        discrepancy("未知のキー: 旧は断る（reject_unknown_keys）。ドライバは未知のキーを断らない（DriverBase の約束・適合テスト C2）", &["settings"], &[]),
    );
    add(
        "push: host key (other layer's key)".into(),
        with("push", |i| i.settings = Some(json!({"host": "x"}))),
        discrepancy(
            "未知のキー（host）: 旧は断る。ドライバは断らない",
            &["settings"],
            &[],
        ),
    );
    add(
        "push: valid bridge".into(),
        with("push", |i| i.settings = Some(bridge(1000))),
        ok(),
    );
    add(
        "push: bridge period 300".into(),
        with("push", |i| i.settings = Some(bridge(300))),
        agree_on(&["settings.bridge.queries[0].period_ms"]),
    );
    add(
        "push: bridge not an object".into(),
        with("push", |i| i.settings = Some(json!({"bridge": 5}))),
        agree_on(&["settings.bridge"]),
    );
    add(
        "push: bridge as a JSON string".into(),
        with("push", |i| i.settings = Some(json!({"bridge": bridge(1000)["bridge"].to_string()}))),
        discrepancy("bridge が JSON を入れた文字列: 旧は断る（オブジェクトでない）。新は文字列をそのまま JSON として読んで受かる", &["settings.bridge"], &[]),
    );
    add(
        "push: settings not an object (array)".into(),
        with("push", |i| i.settings = Some(json!([1]))),
        discrepancy("settings がオブジェクトでない: 旧は断る。settings_from_flat は {} にして検証に渡す（旧い検証が先に断る前提）", &["settings"], &[]),
    );
    add(
        "push: settings null".into(),
        with("push", |i| i.settings = Some(Value::Null)),
        discrepancy("settings が null: 旧は断る（オブジェクトでない）。settings_from_flat は null を {} にする（§3.2「NULL なら {}」）", &["settings"], &[]),
    );
    // push の host・port・unit_id は正規化が既定に揃える。
    add(
        "push: host/port/unit_id junk (normalized)".into(),
        with("push", |i| {
            i.host = "x".into();
            i.port = 99_999;
            i.unit_id = 999;
        }),
        ok(),
    );

    // ---- hub / 未知 / 綴り ----
    add(
        "hub".into(),
        base("hub"),
        discrepancy("hub: 旧の許可リストに含まれて受かる。Hub のレジストリには hub が無い（設計 §4.1）ので protocol の項目エラー。Hub の REST は reject_hub_connection_protocol で元から断る", &[], &["protocol"]),
    );
    for protocol in ["bogus", "", "SLMP", "Modbus-TCP", "modbus", "slmp "] {
        add(
            format!("protocol {protocol:?}"),
            base(protocol),
            agree_on(&["protocol"]),
        );
    }
    rows
}

#[tokio::test]
async fn the_old_flat_validation_and_the_registry_reject_the_same_inputs() {
    let drivers = common::hub_drivers();
    let registry = drivers.registry.clone();
    let service = memory_service(None).await;

    let mut report = Vec::new();
    for row in rows() {
        let old = old_errors(&service, &row.input).await;
        let new = new_errors(&registry, &row.input);
        let (old_fields, new_fields) = (fields(&old), fields(&new));
        let expect = &row.expect;
        // 期待そのものの整合: Agree は old == new、それ以外は old != new。
        let expect_agree = expect.old == expect.new;
        assert_eq!(
            matches!(expect.verdict, Verdict::Agree),
            expect_agree,
            "{}: verdict {:?} contradicts the expected sets old={:?} new={:?}",
            row.label,
            expect.verdict,
            expect.old,
            expect.new
        );
        if matches!(expect.verdict, Verdict::DroppedFieldOldOnly(_)) {
            assert!(
                !expect.old.is_empty() && expect.new.is_empty(),
                "{}: DroppedFieldOldOnly means old rejects and new rejects nothing",
                row.label
            );
        }
        // 旧と新、それぞれの項目名の集合が期待と**完全に**一致すること。
        let want_old: BTreeSet<String> = expect.old.iter().map(|f| (*f).to_string()).collect();
        let want_new: BTreeSet<String> = expect.new.iter().map(|f| (*f).to_string()).collect();
        if old_fields != want_old || new_fields != want_new {
            report.push(format!(
                "{}: verdict {:?} ({}) expected old={want_old:?} new={want_new:?} but old={old_fields:?} new={new_fields:?}\n  old messages: {old:?}\n  new messages: {new:?}",
                row.label,
                expect.verdict,
                expect.verdict.reason()
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
    let service = memory_service(None).await;
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

/// R2b-1d の配線（設計 §4.2、2026-10-21 の決定「(1) が断ったら (2)〜(4) は走らせず、(1) の
/// エラーだけを返す」）: 本番と同じレジストリの `with_registry` の書き込みの入口は、表の全行で、
/// 旧い検証 (1) が断るならその `(項目, 文言)` をそのまま、通るならレジストリの検証の
/// `(項目, 文言)` を返す（どちらも通れば保存する）。今の画面とテストが見るエラーの文言は
/// (1) のまま。
#[tokio::test]
async fn the_registry_service_returns_the_old_errors_first_then_the_registrys() {
    let drivers = common::hub_drivers();
    let registry = drivers.registry.clone();
    let old_service = memory_service(None).await;
    let service = memory_service(Some(registry.clone())).await;
    let mut report = Vec::new();
    for row in rows() {
        let old = old_errors(&old_service, &row.input).await;
        let new = new_errors(&registry, &row.input);
        let combined = old_errors(&service, &row.input).await;
        let want = if old.is_empty() { new } else { old };
        if combined != want {
            report.push(format!(
                "{}: want {want:?} but the registry service returned {combined:?}",
                row.label
            ));
        }
    }
    assert!(report.is_empty(), "\n{}", report.join("\n"));
}
