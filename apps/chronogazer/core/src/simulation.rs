//! 接続単位シミュレーションの「値が動かないタグ」の読み出し（#413、
//! 2026-09-23 オーナー決定）。
//!
//! ## 何のためか
//!
//! `simulation = true` の PLC 接続は、実機の代わりに `banto-collect` の
//! プロセス内シミュレータへ接続する（`banto_collect::simulation`）。
//! シミュレータが値を動かすのは**先頭 16 番地だけ**（Modbus 40001-/30001-
//! のランプ波・00001-/10001- のトグル、SLMP D0- のランプ波・M0- のトグル）で、
//! それ以外の番地のタグは収集されても**値が 0 のまま動かない**。黙っていると
//! 「シミュレーションで試したが値が来ない」を設定の誤りと見分けられない
//! ので、`/tags` 画面がシミュレーション接続の配下で範囲外のタグを一覧に
//! 出せるように、ここで判定して返す。
//!
//! ## 判定は Rust の 1 か所だけ
//!
//! 判定は **`banto_collect::simulation::classify_plc_tag` をそのまま使う**
//! （banto-hub の T15-2 プリフライトと同じ関数）。シミュレータが実際に
//! 値を書く範囲と同じファイルに置かれた判定で、アドレスの解釈も収集の
//! 構成ビルドと同じパーサーを通る。**画面（TS）に同じ判定を書き写さない** -
//! 写すとシミュレータの範囲が変わったときに片方だけ古くなる。
//!
//! ## なぜタグの DTO に載せず、専用の読み取りにしたか
//!
//! 判定には**タグの行だけでなく、その親の親（PLC 接続）の `protocol` と
//! `simulation`** が要る。タグの一覧（`GET /api/tags`）は `banto_tags::Tag`
//! をそのまま返していて、作成・更新の応答も同じ型なので、DTO に載せると
//! タグの書き込み経路すべてで接続まで辿ることになる。しかも接続の
//! `simulation` を切り替えると、**タグは 1 行も変わらないのに判定だけが
//! 変わる** - タグの行に焼き付けると古い値が残る。そこで 3 階層を読んで
//! その場で判定する読み取りを 1 本（`GET /api/simulation-coverage` と Tauri の
//! `simulation_coverage_list`）足し、タグの型・書き込み経路には触らない。
//!
//! ## 公開範囲
//!
//! レジストリの読み取りと同じ **viewer 以上**・監査しない（読み取りは
//! 監査しない、という全体の規約）。載せるのはタグ id・接続 id・範囲内か・
//! 理由文だけで、理由文の中身は `classify_plc_tag` が作る日本語（タグ自身の
//! アドレスとデータ型を含みうるが、どちらも `GET /api/tags` で viewer が
//! 既に読める）。接続先ホスト・資格情報は含まない。
//!
//! ## 返す範囲
//!
//! **レジストリ上で `simulation = true` の接続の配下のタグだけ**（有効・無効を
//! 問わない。無効なタグも、有効にした途端に動かないと分かる方がよい）。
//! 実機接続の配下のタグは判定の意味が無いので返さない。**走っている収集の
//! 状態ではなくレジストリの今の値**で判定する - 「この設定でシミュレーション
//! を動かしたら値が動くか」を編集中に知るための口だから（走っている収集が
//! シミュレータ相手かどうかは `crate::collect::ConnectionView::simulation`）。

use std::collections::HashMap;

use banto_collect::simulation::{classify_plc_tag, SimulationCoverage};
use banto_core::{BantoError, ListParams};
use banto_tags::{
    CollectionGroup, CollectionGroupService, PlcConnection, PlcConnectionService, Tag, TagService,
};
use serde::Serialize;

/// シミュレーション接続の配下のタグ 1 本の判定結果（公開用の形）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationCoverageEntry {
    pub tag_id: i64,
    pub plc_connection_id: i64,
    /// シミュレータがこのタグの番地の値を動かすか。
    pub supported: bool,
    /// `supported == false` のときの理由（`classify_plc_tag` の文言）。
    pub reason: Option<String>,
}

/// 3 階層の行から判定結果を作る（**純関数**）。並びはタグ id の昇順。
///
/// 親の収集グループ・接続が見つからないタグ（読み取りの間に消えた等）は
/// 黙って飛ばす - どの接続の配下か分からない以上、シミュレーションの
/// 判定そのものが成り立たない。
pub fn coverage_entries(
    connections: &[PlcConnection],
    groups: &[CollectionGroup],
    tags: &[Tag],
) -> Vec<SimulationCoverageEntry> {
    let simulated: HashMap<i64, &PlcConnection> = connections
        .iter()
        .filter(|conn| conn.simulation)
        .map(|conn| (conn.id, conn))
        .collect();
    let group_connection: HashMap<i64, i64> = groups
        .iter()
        .map(|group| (group.id, group.plc_connection_id))
        .collect();

    let mut entries: Vec<SimulationCoverageEntry> = tags
        .iter()
        .filter_map(|tag| {
            let conn_id = *group_connection.get(&tag.collection_group_id)?;
            let conn = simulated.get(&conn_id)?;
            let (supported, reason) =
                match classify_plc_tag(&conn.protocol, &tag.address, &tag.data_type) {
                    SimulationCoverage::Supported => (true, None),
                    SimulationCoverage::Unsupported { reason } => (false, Some(reason)),
                };
            Some(SimulationCoverageEntry {
                tag_id: tag.id,
                plc_connection_id: conn_id,
                supported,
                reason,
            })
        })
        .collect();
    entries.sort_by_key(|entry| entry.tag_id);
    entries
}

/// レジストリを読んで [`coverage_entries`] を返す。REST
/// （`GET /api/simulation-coverage`）と Tauri（`simulation_coverage_list`）の
/// 両方がこれを呼ぶ - 経路によって判定が割れないように。
pub async fn simulation_coverage(
    plc_connections: &PlcConnectionService,
    collection_groups: &CollectionGroupService,
    tags: &TagService,
) -> Result<Vec<SimulationCoverageEntry>, BantoError> {
    let connections = plc_connections.list(ListParams::default()).await?.rows;
    // シミュレーション接続が 1 本も無ければ、残り 2 つは読まない。
    if !connections.iter().any(|conn| conn.simulation) {
        return Ok(Vec::new());
    }
    let groups = collection_groups.list(ListParams::default()).await?.rows;
    let tags = tags.list(ListParams::default()).await?.rows;
    Ok(coverage_entries(&connections, &groups, &tags))
}

#[cfg(test)]
mod tests {
    use super::*;
    use banto_tags::{CollectionGroupInput, PlcConnectionInput, TagInput};

    async fn services() -> (PlcConnectionService, CollectionGroupService, TagService) {
        let pool = crate::db::init_db_memory().await.expect("init_db_memory");
        (
            PlcConnectionService::new(pool.clone()),
            CollectionGroupService::new(pool.clone()),
            TagService::new(pool),
        )
    }

    async fn connection(svc: &PlcConnectionService, protocol: &str, simulation: bool) -> i64 {
        svc.create(PlcConnectionInput {
            name: format!("{protocol}-{simulation}"),
            protocol: protocol.to_string(),
            host: "192.0.2.1".to_string(),
            port: if protocol == "slmp" { 5000 } else { 502 },
            unit_id: 1,
            enabled: true,
            simulation,
            word_order: String::new(),
            database: None,
            username: None,
            password: None,
        })
        .await
        .expect("create connection")
        .id
    }

    async fn group(svc: &CollectionGroupService, conn_id: i64) -> i64 {
        svc.create(CollectionGroupInput {
            name: format!("group-{conn_id}"),
            plc_connection_id: conn_id,
            period_ms: 1000,
            enabled: true,
            default_writable: true,
            query_sql: None,
        })
        .await
        .expect("create group")
        .id
    }

    async fn tag(svc: &TagService, group_id: i64, name: &str, address: &str) -> i64 {
        svc.create(TagInput {
            name: name.to_string(),
            collection_group_id: group_id,
            address: address.to_string(),
            data_type: "u16".to_string(),
            string_length: None,
            string_encoding: "utf8".to_string(),
            raw_lo: None,
            raw_hi: None,
            eng_lo: None,
            eng_hi: None,
            unit: None,
            decimals: 0,
            threshold_h: None,
            threshold_hh: None,
            threshold_l: None,
            threshold_ll: None,
            enabled: true,
            writable: false,
            tag_kind: "plc".to_string(),
            expression: None,
            retain: false,
            expected_revision: None,
        })
        .await
        .expect("create tag")
        .id
    }

    /// シミュレーション接続の配下だけが返り、範囲内/範囲外が
    /// `classify_plc_tag` のとおりに分かれる。実機接続の配下は返らない。
    ///
    /// 反証（回帰の検出）: `coverage_entries` の `.filter(|conn|
    /// conn.simulation)` を消すと実機接続のタグ（`real_tag`）が混ざって
    /// 落ちる。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_tags_under_simulation_connections_are_classified() {
        let (plc, groups, tags) = services().await;

        assert!(
            simulation_coverage(&plc, &groups, &tags)
                .await
                .expect("coverage")
                .is_empty(),
            "シミュレーション接続が無ければ 0 件"
        );

        let sim_modbus = connection(&plc, "modbus-tcp", true).await;
        let sim_slmp = connection(&plc, "slmp", true).await;
        let real = connection(&plc, "modbus-tcp", false).await;
        let sim_modbus_group = group(&groups, sim_modbus).await;
        let sim_slmp_group = group(&groups, sim_slmp).await;
        let real_group = group(&groups, real).await;

        let in_window = tag(&tags, sim_modbus_group, "in", "40016").await;
        let out_of_window = tag(&tags, sim_modbus_group, "out", "40017").await;
        let slmp_in = tag(&tags, sim_slmp_group, "d0", "D0").await;
        let slmp_other_device = tag(&tags, sim_slmp_group, "w0", "W0").await;
        let real_tag = tag(&tags, real_group, "real", "40100").await;

        let entries = simulation_coverage(&plc, &groups, &tags)
            .await
            .expect("coverage");
        let ids: Vec<i64> = entries.iter().map(|e| e.tag_id).collect();
        assert_eq!(
            ids,
            vec![in_window, out_of_window, slmp_in, slmp_other_device],
            "実機接続の配下（{real_tag}）は返さない"
        );

        let by_id: HashMap<i64, &SimulationCoverageEntry> =
            entries.iter().map(|e| (e.tag_id, e)).collect();
        assert!(by_id[&in_window].supported);
        assert_eq!(by_id[&in_window].reason, None);
        assert_eq!(by_id[&in_window].plc_connection_id, sim_modbus);
        assert!(!by_id[&out_of_window].supported);
        assert!(by_id[&out_of_window]
            .reason
            .as_deref()
            .is_some_and(|r| !r.is_empty()));
        assert!(by_id[&slmp_in].supported);
        assert!(!by_id[&slmp_other_device].supported);
        assert_eq!(by_id[&slmp_other_device].plc_connection_id, sim_slmp);
    }

    /// 公開する JSON の形（camelCase・余計なキーが無い）を固定する。
    #[test]
    fn the_entry_serializes_to_the_documented_shape() {
        let json = serde_json::to_value(SimulationCoverageEntry {
            tag_id: 7,
            plc_connection_id: 3,
            supported: false,
            reason: Some("理由".to_string()),
        })
        .expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({
                "tagId": 7,
                "plcConnectionId": 3,
                "supported": false,
                "reason": "理由"
            })
        );
    }
}
