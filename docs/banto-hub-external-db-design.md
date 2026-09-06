# banto-hub 外部 DB 連携 設計: DB Source（#228）と DB Sink（#229）

作成日: 2026-09-06
状態: **Draft・オーナー決定待ち（§6）**。コード調査（§3）は 2026-09-06 の main（`9c26b3a`、toolchain 1.98.1）に対して実施済み。実装は未着手。
対象: Issue [#228](https://github.com/tyaro/banto-industrial/issues/228)（外部 RDB の値をタグ空間へ取り込む Source）と [#229](https://github.com/tyaro/banto-industrial/issues/229)（タグ値を外部 RDB へ保存する Sink / Logger）。**2 件はペアで 1 設計**とし、DB 接続エンティティを共有する。

関連: [tag-server-design.md](tag-server-design.md)（タグ空間・書き込み安全の一次ソース。§2 非スコープの「ロガー作らない」決定を本書 §2.1 で扱う）、[banto-hub-t20-design.md](banto-hub-t20-design.md)（値表現と read-on-demand の先例）、[banto-hub-t21-design.md](banto-hub-t21-design.md)（構成操作の MCP と監査の型）、[plan.md](plan.md) §1（「外部時系列DB読み出し・保存」は 3〜4 案件で再利用される共通資産）。

---

## 1. 背景と狙い

工場案件では、PLC だけでなく MES・生産管理・既設 SCADA が持つ RDB の値を同じタグ空間に載せたい（#228）、逆にタグ値を顧客指定の RDB に定周期・変化時で記録したい（#229）という要求が繰り返し出る。plan.md §1 でもこの 2 つは「3〜4 本の案件で再利用される共通資産」に数えられている。

狙いは次の 2 点に絞る。

- **Source**: 「1 Query Group = 1 SELECT = N タグ」の形で RDB の値を現在値として公開し、REST / WS / MQTT / gRPC の利用者からは PLC タグと区別なく見えるようにする。
- **Sink**: タグ空間を購読して外部 RDB へバッチ INSERT する汎用ロガー。ChronoGazer の時系列ストレージ・トレンド表示は置換しない。

## 2. 既存決定との関係（オーナー確認が要る点）

### 2.1 「ロガー・帳票は作らない」決定（2026-08-04）と #229

[tag-server-design.md](tag-server-design.md) §2 非スコープに「**ロガー・日報・帳票機能: 作らない（2026-08-04 オーナー決定）。記録は ChronoGazer の商品価値**」がある。#229 の Sink は「タグ値を外部 DB へ保存する」機能であり、この決定と正面から重なる。

本書は #229 を **「ChronoGazer の記録機能の代替ではなく、顧客が既に持つ RDB へ値を流し込むための出口（MQTT 発行と同じ位置づけの外部インタフェース）」** と位置づけ、次を非スコープに固定することで線を引く提案とする。

- Hub 自身は履歴を保持しない（保存先は常に外部 DB、Hub 内 SQLite には書かない）
- 保存した履歴を Hub が読み返す API・トレンド表示・帳票は作らない
- 保持期間管理・間引き・バックフィルは作らない（ChronoGazer / tstore の領分）

**この位置づけで 2026-08-04 決定を部分的に覆すかは §6-1 のオーナー決定。** 覆す場合は tag-server-design.md §2 の該当行に日付付きで追記する（T20 が banto-tagclient-design.md §4.4 の旧決定を覆したときと同じ扱い）。

### 2.2 資格情報の保管（v1 平文・閉域 LAN 前提）

Hub には暗号化保管や OS keyring の機構が存在しない。MQTT のパスワードは `settings` テーブルに平文で保存され、その根拠は tag-server-design.md §5.6「v1 では平文 + 閉域 LAN 前提」である（`apps/banto-hub/core/src/settings.rs`）。DB の接続パスワードも同じ前提に乗せるか、ここで初めて暗号化機構を入れるかは §6-3 の決定。本書の推奨は **v1 は MQTT と同じ平文**（新機構は別 issue）。ただし config パッケージのエクスポートからは既存の除外リスト（`CONFIG_PACKAGE_EXCLUDED_SECRETS`）に倣って必ず除外する。

## 3. コード調査の結果（2026-09-06）

設計の前提となる現状。番号は後続の節から参照する。

1. **接続の protocol は Rust の enum ではなく `TEXT + CHECK`**。`crates/banto-tags/src/plc_connection.rs` の `ALLOWED_PROTOCOLS = ["modbus-tcp", "slmp", "virtual"]` が SQL の CHECK と鏡写しで、追加は「型変更ではなくマイグレーション」という明示設計。SQLite は CHECK を ALTER できないため、protocol を増やすにはテーブル再構築マイグレーションが要る（`0004` / `0007` に手順の先例）。
2. **タグ種別は `tags.tag_kind`（`plc` / `computed` / `internal`）**。`computed` は予約仮想接続 `calc`、`internal` は `mem` の配下にしか置けない（`validate_tag_kind_placement`）。
3. **PLC 以外の値は `ServerTagStore` に書く**（`apps/banto-hub/core/src/computed.rs`）。`set(tag_key, Option<f64>, Quality, ptime_ms)` は公開 API で、演算タグと内部タグが使う。読み出しの統一点 `read_current`（`apps/banto-hub/core/src/hub.rs`）は「`tag_kind == plc` なら収集キャッシュ、それ以外は `ServerTagStore`」と分岐しているため、**新しい tag_kind を足しても `read_current` は無改造で `ServerTagStore` を読む**。
4. **購読者への通知は push ではなく 250ms の poll**（`subscribe_core.rs` の `EVAL_TICK_MS`）。WS / gRPC / MQTT すべてが `read_current` を定期評価するので、`ServerTagStore` に書けば**配線ゼロで全購読者に見える**。
5. **`ServerTagStore` の値は `Option<f64>` のみ**。文字列は T20 で「分離経路（write = write_path、read = read-on-demand）」を採り、現在値キャッシュは f64 のまま温存している。
6. **サーバー側ストアの値には stale 導出が無い**。`effective_sample` は保存された quality をそのまま返す（収集キャッシュ側は `STALE_PERIOD_FACTOR = 2.5` で読み出し時に導出）。Source は失敗・切断時に自分で Bad / Stale を書く必要がある。
7. **PLC 収集パイプライン（`banto-collect` / `banto-broker`）はレジスタ一括読みの形**。`ClientFactory` / `BrokerSession` は「1 ソケット・アドレス範囲の read_batch」を前提にしており、「SELECT 1 本 → N 列 → N タグ」には合わない。演算タグの評価ループ（`runtime.rs` で `tokio::spawn`）が非 PLC ソースの先例。
8. **グループの周期は `period_ms` の CHECK で固定集合**（100 / 200 / 500 / 1000 / 2000 / 5000 / 10000 / 60000 ms）。DB のポーリングで 1 分超（5 分・1 時間）が要るなら CHECK の拡張＝テーブル再構築が要る。
9. **構成変更は既存の 1 経路に集約されている**。`*_tx` サービス → `preflight_transaction` → commit → `commit_catalog_and_notify`。REST と T21 MCP が同じ関数を使うため、既存エンティティに載せれば pending queue・監査・MCP は自動で効く。
10. **sqlx は `sqlite` feature のみ**。PostgreSQL は sqlx の `postgres` feature（+ TLS feature）で足りるが、**SQL Server は sqlx にドライバが無く `tiberius` 等の別クレートが要る**（依存増・非同期ランタイム統合の確認が要る）。
11. **config パッケージは TS 側の機能**（`apps/banto-hub/src/lib/banto/configPackage.ts`）。新エンティティは `plcConnections` / `mqtt` と同様に往復対象へミラーし、資格情報は除外リストへ足す。適用は非トランザクション（T19 の既知の隙間）。
12. **監査は log-before-write の 2 段**（`write_audit.rs` の `insert_pending` → 結果 `UPDATE`）。Sink の INSERT 試行を監査したい場合の型。
13. **CSV はヘッダ厳密検証**（列を足すと旧 CSV が import 不可＝非後方互換。#264 の教訓）。タグに新しい列を増やす設計は避けたい。

## 4. DB Source（#228）の設計

### 4.1 エンティティの配置（推奨: 案 A・既存 3 階層の流用）

| 案                                                               | 内容                                                                                                                                                                                                                             | 得るもの                                                                                                                                         | 失うもの / 代償                                                                                                                                                    |
| ---------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| **A. 接続 → グループ → タグの既存 3 階層をそのまま使う**         | 接続: `protocol = "postgres"`（将来 `"mssql"`）に DB 名・ユーザー・パスワードを持たせる。グループ: `period_ms` に加えて `query_sql` を持たせ「1 グループ = 1 SELECT」。タグ: `tag_kind = "db"`、`address` に**結果列名**を入れる | 管理 UI（Drawer・ツリー）・CSV・config パッケージ・pending queue・監査・T21 MCP の 31 ツールが**ほぼ無改造で効く**（§3-9）。catalog に自然に載る | protocol の CHECK 拡張＝テーブル再構築（§3-1）。グループに列追加。`address` の意味が kind で変わる（`db` のときは識別子として検証）。CSV の列は増やさない（§3-13） |
| B. `db_connections` / `db_queries` / `db_columns` の新テーブル群 | DB 専用のスキーマを新設し、タグは `tag_kind = "db"` で `db_columns` を参照                                                                                                                                                       | DB の概念（接続プール・クエリ・列）を PLC の語彙に押し込めない                                                                                   | UI・CSV・config パッケージ・MCP・pending queue を**すべて二重化**する。T21 の 31 ツールに DB 版を足すことになる。実装量は A の 2〜3 倍                             |

**推奨は A。** 理由は §3-9 と §3-11 のとおり、構成操作の経路が 1 本に集約されているので、その経路に乗る形が最も実装量が少なく、運用面（稼働中変更のキュー化・監査・MCP）が初日から揃う。DB 固有の概念は「接続 = DB 接続（プール 1 つ）」「グループ = クエリ 1 本と周期」「タグ = 結果列」に素直に写像でき、無理がない。

スキーマ変更（案 A）:

- `plc_connections`: `protocol` CHECK に `postgres` を追加（テーブル再構築）。列追加 `database TEXT`、`username TEXT`、`password TEXT`（平文、§2.2）。`host` / `port` は流用。`unit_id` / `word_order` / `simulation` は DB では無視。
- `collection_groups`: 列追加 `query_sql TEXT`（`db` 接続配下のグループでは必須、PLC 配下では NULL）。`period_ms` は流用。CHECK の拡張（5 分・1 時間）は §6-9 の決定。
- `tags`: `tag_kind` に `db` を追加。`address` は結果列名（識別子の正規表現で検証、大文字小文字は DB 側の規則に委ねず**そのまま比較**）。`writable` は `db` では常に false（v1 は読み取り専用、§6-10）。`data_type` は数値系と bool のみ（§4.4）。
- 配置制約: `db` タグは `protocol = "postgres"` の接続配下にしか置けない。`plc` タグは DB 接続配下に置けない（`validate_tag_kind_placement` の拡張）。

### 4.2 実行モデル

- DB 接続 1 つにつき tokio task 1 本（演算タグの評価ループと同じ `runtime.rs` の spawn 位置、§3-7）。task は接続プール（sqlx `PgPool`、最大接続数は小さく固定、例 2）を持ち、配下の有効グループを各 `period_ms` で回す最小デッドライン方式（`banto-collect` の task と同じ考え方だが実装は共有しない）。
- **1 周期 = 1 グループ = 1 SELECT**。結果は**先頭 1 行**を採用する。0 行はグループ全タグ Bad、2 行以上は先頭行を採用し `warn` を 1 回だけ出す（§6-8 の決定）。
- 列 → タグの対応は**列名**で行う（位置ではない）。SELECT に無い列名を持つタグは Bad。
- SQL は**固定文・パラメータ無し**を v1 とする。bind parameter を使う「動的値」は要件が出てから（例: `WHERE machine_id = ?` の値をタグや設定から供給する）。
- 文の検証はベストエフォート: 先頭が `SELECT` / `WITH`、単文（`;` を含まない）のみ受理。**本当の防御は DB 側の read-only ユーザー**とし、UI と docs に明記する。
- タイムアウト: `statement_timeout` 相当をクエリ周期未満に固定（例 `min(period_ms * 0.8, 30s)`）。1 本の遅いクエリが接続全体の周期を崩さないよう、グループごとに独立して待つ。
- 再接続: 接続失敗・クエリのトランスポートエラーは指数バックオフ（1s → 30s 上限、`banto-broker` の `backoff_delay` と同じ定数）で再試行し、その間は配下の全タグを Bad にする。DB 停止で Hub 本体を止めない（受け入れ条件）。

### 4.3 Quality の変換

`ServerTagStore` には stale 導出が無い（§3-6）ため、Source が書く。

| 事象                       | 値       | quality | 備考                                                |
| -------------------------- | -------- | ------- | --------------------------------------------------- |
| 正常に列が取れた           | 変換値   | Good    | `ptime_ms` はクエリ完了時刻                         |
| 列が NULL                  | None     | Bad     | タグ単位                                            |
| 列が無い / 型変換不能      | None     | Bad     | タグ単位（起動時と設定変更時に `warn`）             |
| 0 行                       | None     | Bad     | グループ単位                                        |
| クエリエラー・タイムアウト | 直前値   | Stale   | グループ単位。2 周期連続で失敗したら Bad に落とす   |
| 接続断（バックオフ中）     | None     | Bad     | 接続単位                                            |
| グループ / 接続が無効      | 既存規則 | Bad     | `effective_sample` の `!enabled` 規則がそのまま効く |

「Stale → 2 周期で Bad」は収集キャッシュの `STALE_PERIOD_FACTOR = 2.5` に合わせた運用感（1 回の失敗で値を捨てない）。

### 4.4 値の型（v1 は数値・bool のみ）

`ServerTagStore` が `Option<f64>` である（§3-5）ため、v1 で受ける列型は **整数・浮動小数・numeric/decimal（f64 へ変換）・bool（0/1）・timestamp（epoch ms）**とする。`text` 列は**v1 非対応**とし、`data_type = 'string'` の `db` タグは登録時に拒否する。

文字列（品名・ロット番号）は Source の実需として大きいので第 2 段で扱う。候補は 2 つ: (a) `ServerTagStore` の値を T20 で見送った「数値または文字列」の enum へ広げる（影響は WS / MQTT / gRPC の DTO まで及ぶ）、(b) T20 の read-on-demand と同型で「`db` タグの文字列は on-demand に SELECT を再実行して返す」。**どちらを採るかは要件が出てから決める（§6-5）。**

### 4.5 稼働中の設定変更・監査・MCP

案 A なら既存経路（§3-9）に自動で乗る。稼働中は pending queue にキューされ、apply 時に `commit_catalog_and_notify` が走る。Source の task は `commit_catalog` の完了を受けて配下のグループ集合を再読込する（演算タグの `ComputedEngine::commit` と同じタイミング）。監査は既存の構成監査（T21 §3.3）がそのまま記録し、**SQL 文の変更も監査行に残る**。MCP は `create_connection` 等が `protocol` と追加項目を受けるだけの拡張で済む。

### 4.6 接続テスト

`POST /api/plc-connections/{id}/test`（新設）で「接続 → `SELECT 1` → 配下の各グループの SQL を `LIMIT 1` 相当で 1 回実行 → 列名と型の一覧を返す」。UI の Drawer から呼び、列名をタグの `address` 候補として提示する。T21 に `test_connection` ツールとして追加。

### 4.7 API 面での見え方

REST / WS / MQTT / gRPC からは PLC タグと区別しない（受け入れ条件「Source 差異を外部 API に漏らさない」）。`value_source` の表示ラベル（`real` / `simulation` / `internal`）に `db` を足すかは §6-11。

## 5. DB Sink（#229）の設計

### 5.1 位置づけ

MQTT 発行（`apps/banto-hub/core/src/mqtt.rs`）と同型の**読み取り専用の消費者**。タグ空間には一切書かず、`read_current` を 250ms で評価して変化・周期を検出し、外部 DB へ INSERT する。収集・演算・書き込み経路に依存も影響も持たない（障害分離の要）。ChronoGazer との線引きは §2.1。

### 5.2 エンティティ

DB 接続は **#228 と同じ `plc_connections`（`protocol = "postgres"`）を共有**する（Source を使わず Sink だけを使う案件でも、接続の登録・接続テスト・資格情報の扱いが 1 つで済む）。Sink 固有に `logger_groups` テーブルを新設する。

| 列                  | 内容                                                                       |
| ------------------- | -------------------------------------------------------------------------- |
| `name`              | 一意                                                                       |
| `db_connection_id`  | `plc_connections.id`（`protocol = "postgres"` のみ許可）                   |
| `mode`              | `interval` / `on_change`（MQTT と同じ語彙）                                |
| `interval_ms`       | `interval` の周期。`on_change` では最短発行間隔（スロットル）              |
| `table_name`        | 保存先テーブル（識別子として厳格検証。スキーマ修飾 `schema.table` を許可） |
| `store_bad`         | Bad / Stale の行も保存するか（既定 false = Good のみ）                     |
| `enabled`           |                                                                            |
| `logger_group_tags` | 別テーブル。`logger_group_id` × `tag_id`（タグの削除で行も消える）         |

タグ選択は「グループ単位」だけを v1 とし、「接続配下のすべて」「名前のワイルドカード」は入れない（catalog の rename 追従を安定 ID で担保するため）。

### 5.3 保存スキーマ（v1 は long 固定、DDL は Hub が発行しない）

```text
ts (timestamptz) | tag_id (bigint) | external_name (text) | value (double precision) | quality (text)
```

- **long / narrow を v1 とする。** タグ追加でスキーマが変わらない・グループ間でテーブルを共有できる・Hub 側の実装が 1 種類で済む。wide（列 = タグ）は SQL 利用者には便利だが、グループごとの DDL 管理と rename 追従が要るので第 2 段（§6-6）。
- **Hub は CREATE TABLE を発行しない。** テーブルは利用者（DBA）が用意し、Hub は起動時と設定変更時に `SELECT ... LIMIT 0` で列の存在と型を検査して、合わなければグループを `error` 状態にして止める（他グループと収集には影響しない）。理由: Hub の DB ユーザーに DDL 権限を持たせない（「必要最小限の INSERT 権限」の受け入れ条件）ため。UI には推奨 DDL を**表示**する（§6-7）。
- `external_name` を毎行に持たせるのは、DB 側だけで人が読めるようにするため（tag_id だけだと Hub の catalog を引かないと意味が分からない）。rename 後の行は新しい名前になる（履歴の名前を書き換えない）。

### 5.4 バッチとバックプレッシャ

- グループごとに **bounded queue**（既定 10,000 行、設定可）を持ち、`flush_interval_ms`（既定 1,000）または `batch_size`（既定 500）で 1 回の multi-row INSERT にまとめる。
- DB 停止・INSERT 失敗は指数バックオフ（Source と同じ定数）。**キューが満杯なら最も古い行を捨てて `dropped` カウンタを増やす**（メモリを無制限に使わない。§6-8）。ディスクスプールは将来。
- 1 回の INSERT はトランザクション 1 つ。失敗したバッチは丸ごと再試行し、重複挿入を避けるため成功確認前にキューから外さない（at-least-once。DB 側で `(ts, tag_id)` の一意制約を張るかは利用者の選択）。

### 5.5 可観測性

- `GET /api/status`（T19 の状態画面）に Sink の節を足す: グループごとの `state`（running / backoff / error / disabled）、`queued`、`dropped`、`last_flush_at`、`last_error`。
- 行単位の INSERT は監査しない（量が多く監査の意味を薄める）。**設定変更は既存の構成監査**に載る。INSERT の試行・結果を追いたい場合は `write_audit.rs` の log-before-write の型（§3-12）で `hub_sink_audit` を足せるが、v1 では入れない（§6-12）。
- ログは `warn` を初回とバックオフ段階の変化時のみ（連続失敗で毎周期 warn を出さない。MQTT と同じ抑制）。

### 5.6 ライフサイクル

`HubRuntime::start` / `RunningHub::shutdown` で MQTT・gRPC と同じ位置（純粋な消費者として `manager` より先に止める）。停止時は残キューを最大 5 秒だけ flush して諦める（残りは `dropped` に計上して `warn`）。Sink の設定変更は収集に影響しないので **pending queue に載せず即時適用**とする（§6-13）。

## 6. オーナー決定項目

| #   | 項目                                                             | 推奨                                                                                      | 影響                                                                   |
| --- | ---------------------------------------------------------------- | ----------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| 1   | #229 が 2026-08-04「ロガー作らない」決定を覆すことの確認（§2.1） | §2.1 の線引き（外部 DB への出口のみ、Hub は履歴を持たず読み返さない）で**覆す**           | tag-server-design.md §2 に日付付きで追記。否なら #229 はクローズ       |
| 2   | v1 で対応する DB                                                 | **PostgreSQL のみ**。SQL Server は `tiberius` の依存・保守状況を S0 で実測してから第 2 段 | 依存増分と CI 時間。MySQL / SQLite は需要待ち                          |
| 3   | 資格情報の保管（§2.2）                                           | **MQTT と同じ v1 平文**。暗号化・keyring は別 issue                                       | config パッケージの除外リストに追加                                    |
| 4   | Source のエンティティ配置（§4.1）                                | **案 A**（既存 3 階層の流用）                                                             | protocol CHECK の再構築マイグレーション 1 本                           |
| 5   | 文字列列の扱い（§4.4）                                           | **v1 は数値・bool・timestamp のみ**。文字列は要件が出た時点で (a)/(b) を選ぶ              | `string` の `db` タグは登録拒否                                        |
| 6   | Sink の保存スキーマ（§5.3）                                      | **long 固定**。wide は第 2 段                                                             |                                                                        |
| 7   | Sink の DDL                                                      | **Hub は発行しない**（起動時検査＋推奨 DDL の表示のみ）                                   | DB ユーザーは INSERT 権限だけで済む                                    |
| 8   | キュー溢れ・複数行結果の扱い（§4.2 / §5.4）                      | Source: 先頭行採用＋warn 1 回。Sink: **最古を捨てて `dropped` 計上**                      |                                                                        |
| 9   | グループ周期の上限（§3-8）                                       | v1 は既存集合（最大 60 s）のまま。5 分・1 時間は要望が出たら CHECK を拡張                 | 拡張はテーブル再構築                                                   |
| 10  | Source タグの書き込み（UPDATE）                                  | **v1 は読み取り専用**（#228 の非スコープどおり）                                          | `db` タグは `writable = false` 固定                                    |
| 11  | `value_source` ラベルに `db` を足すか                            | 足す（`real` と区別できる方が現場の切り分けに効く）                                       | SDK（banto-tagclient）は未知ラベルを `Unknown(raw)` で保持するので互換 |
| 12  | Sink の INSERT 監査                                              | v1 は入れない（状態 API の `dropped` / `last_error` で足りる）                            |                                                                        |
| 13  | Sink 設定変更の pending queue                                    | **載せない**（収集に影響しないため即時適用）                                              |                                                                        |

## 7. スライス構成（案）

| slice | 内容                                                                                                                                                         | 完了条件                                                                                          |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| S0    | 依存の実測: sqlx `postgres` + TLS feature の依存増分・license（cargo-deny）・配布バイナリ増分。`tiberius` の同上（第 2 段の判断材料）                        | 実測値を本書 §3 末尾に追記。cargo-deny 緑                                                         |
| S1    | DB 接続エンティティ: protocol `postgres` のマイグレーション（再構築）、`PlcConnection` の追加項目、接続テスト API、config パッケージ往復と除外、MCP の受け口 | 既存 PLC 接続の CRUD・CSV・E2E に回帰なし。接続テストが `SELECT 1` と列一覧を返す                 |
| S2    | Source 本体: `query_sql` 列、`db` tag_kind と配置制約、ポーリング task、Quality 変換、バックオフ、`commit_catalog` 連動                                      | ローカル PostgreSQL（Docker）に対する統合テスト: 正常・NULL・0 行・クエリエラー・接続断からの復帰 |
| S3    | Source の UI / CSV / MCP: Drawer の DB フィールド、列名候補の提示、`db` タグの登録 UI、E2E                                                                   | 手動 smoke 手順を docs に追加                                                                     |
| S4    | Sink 本体: `logger_groups` テーブル、消費 task、long スキーマ INSERT、bounded queue、バックオフ、起動時のテーブル検査、状態 API                              | 統合テスト: interval / on_change・DB 停止中のキュー上限・復帰後の flush・停止時の flush           |
| S5    | Sink の UI / MCP / docs: グループ CRUD 画面、推奨 DDL 表示、状態画面の節、T21 ツール追加                                                                     |                                                                                                   |
| S6    | 実 DB 検証: 顧客相当の PostgreSQL（別マシン・LAN 越し）で Source / Sink を 24 h 連続動作。切断・再接続・DB 再起動                                            | 結果を real-machine 系 docs に記録                                                                |

S0 と S1 は実機不要。S2 / S4 の統合テストは CI で PostgreSQL のサービスコンテナを使う（GitHub Actions の `services:`）。

## 8. 更新対象ドキュメント

- [tag-server-design.md](tag-server-design.md) §2 非スコープ（§6-1 の決定を反映）、§4.1 タグ種別の表に `db` を追加、§5 外部 IF に Sink を追加
- [README.md](README.md) の文書一覧に本書を追加（本 PR で実施）
- [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md) の進捗追記
- [banto-hub-mcp-reference.md](banto-hub-mcp-reference.md) / [banto-hub-t21-design.md](banto-hub-t21-design.md)（MCP ツールの追加時）
- [banto-tagclient-design.md](banto-tagclient-design.md)（`value_source` に `db` を足す場合の互換性の注記）
