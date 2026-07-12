# banto-industrial

工場・現場系アプリのための再利用資産層。
[banto](https://github.com/tyaro/banto)（汎用管理画面テンプレート）の上に、
ドメイン寄りの共通資産（タグレジストリ・PLC通信・時系列収集/読み出し）と
それを使う製品アプリ（記録計ほか）を蓄積する。

- 計画: [docs/plan.md](docs/plan.md)（I系 = 資産クレート、R系 = 記録計 **ChronoGazer**）
- ChronoGazer（記録計）要件定義: [docs/recorder-requirements.md](docs/recorder-requirements.md)
- banto 側のスコープ整理: banto リポジトリの docs/template-scope.md

## 構成

```
crates/
  banto-tags/        I1: タグレジストリ（PLC接続/収集グループ/タグの定義・型・スケーリング）実装済み
  banto-plc/         I2: PLC通信（読み取り専用。trait + Modbus TCP 実装済み、MC/SLMP 続行）実装済み
  banto-tstore/      I3a: 時系列ストレージ（日次ファイル+スキーマ凍結の自己記述的SQLite）実装済み
  banto-collect/     I3b: 収集エンジン（PLC定期読み出し→banto-tstore書き込み、現在値・イベント供給）実装済み
  banto-tsquery/     I4: 期間クエリ + サーバ側 min/max 間引き（read_range/read_decimated/aggregate/catalog）実装済み
apps/
  chronogazer/       R系: デジタル記録計 ChronoGazer（Tauri + LAN、banto テンプレート由来）予定
```

### `banto-tags`（I1）

3層のエンティティモデル（`PlcConnection` → `CollectionGroup` → `Tag`）+
CRUD/一覧サービス + スケーリング純関数（`scale_raw`/`unscale`）。
マイグレーションはクレート内 `migrations/` に同梱し、消費側が
`banto_tags::migrate(&pool)` を起動時に呼んで適用する
（`apps/admin-template/core/src/db.rs` の方式を踏襲）。
詳細は [crates/banto-tags/src/lib.rs](crates/banto-tags/src/lib.rs) の
モジュールドキュメントを参照。

### `banto-plc`（I2）

読み取り専用・一括読み出しの PLC 通信クライアント。プロトコル差し替えの
境界は [`PlcClient`](crates/banto-plc/src/client.rs) trait（`dyn` 互換に
手書き - I3 が `Box<dyn PlcClient>` で複数 PLC を並行保持する前提）。
先行実装は Modbus TCP（[`ModbusTcpClient`](crates/banto-plc/src/modbus/mod.rs)、
外部 `modbus` クレート不使用の自前実装、FC1-4 のみ）。MELSEC MC/SLMP は
同 trait の実装として後続追加予定（docs/plan.md I2）。

- アドレス表記（`crates/banto-plc/src/address.rs`）: 計装の参照番号方式
  （`0/1/3/4` + 4or5桁、`40001` → 保持レジスタ offset 0）を純関数でパース
- 要求プランニング（`crates/banto-plc/src/planning.rs`）: 近接タグを1回の
  FC 要求へ結合（間隙許容・Modbus上限で分割）、応答→各タグへの逆写像まで
  純関数で設計。256タグ/100ms 収集の実現可否はここが握る
- デコード（`crates/banto-plc/src/decode.rs`）: i16/u16/i32/u32/f32 +
  32bit ワード順（HighLow既定/LowHigh）
- シミュレータ（`simulator` feature、`crates/banto-plc/src/modbus/simulator.rs`）:
  in-process Modbus TCP テストダブル。I3 の結合テストや R4 の72hソークでも
  再利用予定
- 性能スモーク実測（ループバック、256タグ×3往復/回・1000回平均）:
  約0.4ms/回 - 100ms/周期目標に対し十分な余裕（実PLC相手の実測はI3で再検証）

詳細は [crates/banto-plc/src/lib.rs](crates/banto-plc/src/lib.rs) の
モジュールドキュメントを参照。

### `banto-tstore`（I3a）

時系列データの保存層。**日次ファイル + スキーマ凍結**方式
（2026-07-12 決定、docs/recorder-requirements.md §8）:
データファイル（SQLite、`YYYYMMDD-NNN.sqlite3`）は作成時にスキーマが
確定し以後変更しない。収集グループ/タグの構成が変わっても既存ファイルは
`ALTER` せず、新しい連番ファイルへローテーションする。`banto-tags`/
`banto-plc` と異なり **タグレジストリ（I1）に一切依存しない** - 各
ファイルは `tstore_meta`/`tstore_groups`/`tstore_columns` テーブルを
同梱する自己記述的な構造で、後続の I4（クエリ層）はレジストリ DB へ
接続しなくてもファイル単体を解釈できる。

- [`config::StoreConfig`](crates/banto-tstore/src/config.rs): ファイル
  生成に使う構成スナップショット（I3b がタグレジストリの行から組み立てる）
- [`writer::TsWriter`](crates/banto-tstore/src/writer.rs): 追記 +
  バッファリング（既定 1秒/500行、`WriterOptions` で調整可）+
  ローカル深夜0時での自動ローテーション（[`clock::Clock`](crates/banto-tstore/src/clock.rs)
  trait 経由でテスト注入可能）
- [`reader::TsReader`](crates/banto-tstore/src/reader.rs): 1ファイル
  単位の最小限の読み出し（範囲クエリ・間引きは I4 の仕事）
- [`files`](crates/banto-tstore/src/files.rs): `list_data_files`/
  `prune_files`（保持期限超過ファイルの自動削除、当日は対象外）
- ローカル暦日変換（[`date::LocalDate`](crates/banto-tstore/src/date.rs)）は
  Howard Hinnant の暦日アルゴリズムを純整数演算で実装し、追加の日付
  クレートに依存しない。OS のローカル UTC オフセット取得のみ `time`
  クレートを使用（Windows 専用製品という前提での判断 - `Cargo.toml`
  参照）

詳細は [crates/banto-tstore/src/lib.rs](crates/banto-tstore/src/lib.rs) の
モジュールドキュメントを参照。

### `banto-collect`（I3b）

収集エンジン。I1/I2/I3a を束ねる初のクレート: タグレジストリから
**有効な構成のスナップショット**（[`build_config`](crates/banto-collect/src/config.rs)
→ `CollectorConfig`）を組み立て、PLC 接続毎に1タスクを起動して
グループ周期で一括読み出し → スケーリング適用 → `banto-tstore` へ追記する。
UI と独立に 24/365 自走し（recorder-requirements.md §4）、停止指示
（`Collector::stop`、writer flush 保証付き）まで回り続ける。

- **並行構造**: 接続毎に1タスクがソケットを専有（`Box<dyn PlcClient>`、
  プロトコル分岐は factory 関数に隔離）。グループ周期の多重化はタスク内の
  最小デッドライン方式で、発火後の次回は常に「今 + 周期」— 追い付き連射を
  しない（取りこぼしは gap として記録されるのが記録計として正しい）
- **断絶時挙動**: PLC 断でも周期ティックは止めず**全NULL行を記録し続け**、
  接続は別サブタスクで指数バックオフ（1s→2s→…上限30s、成功でリセット）
  再接続。`plc_disconnected`/`plc_reconnected` イベント発行
- **現在値キャッシュ**（[`CurrentValuesHandle`](crates/banto-collect/src/current.rs)）:
  タグ毎の最新値 + 品質。Good/Bad は格納、**Stale は読み出し時判定**
  （最終更新から周期×2.5超）— R1 のデジタル/バー/計器・ヘルス表示が消費
- **イベント2系統**（[`EventSink`](crates/banto-collect/src/event.rs)）:
  `tokio::broadcast` によるライブ配信 + `collect_events` テーブルへの永続化
  （収集開始/停止・PLC断/復旧・しきい値 entered/cleared。しきい値は
  スケーリング後の値の状態変化エッジのみ — ACK等のアラーム管理は非スコープ §7）
- テストは `banto-plc` の `simulator` feature を再利用（結合11件 +
  100ms×3グループのミニソーク + `#[ignore]` の60秒版 = 将来の72hソークの雛形）

詳細は [crates/banto-collect/src/lib.rs](crates/banto-collect/src/lib.rs) の
モジュールドキュメントを参照。

### `banto-tsquery`（I4）

時系列クエリ層。`banto-tstore`（I3a）のデータディレクトリを、タグレジストリ
（I1）に接続せず自己記述的メタデータだけで読む点は banto-tstore と同じ設計。
4つのクエリを提供する `TsQuery`:

- `read_range`: 生データ範囲取得（CSV出力向け）。行数上限（既定10万行）を
  超えるとエラーで `read_decimated` の利用を促す
- `read_decimated`: サーバ側 **min/maxエンベロープ間引き**（トレンド用）。
  各ビンは平均ではなく実際の min/max を返す — 瞬間スパイクを潰さない設計
  （記録計の存在意義）。間引きはファイル毎に SQLite の `GROUP BY` で実行し、
  Rust側は複数ファイルの結果を時刻順マージするのみ（大範囲でも生データ行を
  Rust側に吸い上げない）。ビン幅はグループの `period_ms`（ファイル間で
  異なる場合は最大値）未満に細かくならないようクランプし、クランプが効いて
  なお実データ点数が少ない場合はビン境界に丸めず実サンプル時刻をそのまま
  返す最適化（ズームイン時の階段状表示を回避）付き
- `aggregate`: 期間集計（日報用）。タグ毎 min/max/avg/count（NULL除外）
- `catalog`: 利用可能なグループ/タグ/データ期間の一覧（UIの期間選択初期化用）

**欠測（gap）は隠さない**: ビン内の有効サンプル数が0のタグ、またはビンに
行が1つも無い区間（収集停止等）は `BinValue::Gap` として明示的に返す
（フロントが線を切れるように）。ファイルを跨ぐ範囲（日次ローテーション、
タグ構成変更による強制ローテーション）では `tag_key` でマッチし、
あるファイルに存在しないタグは gap になる。

`TsReader`（I3a）の `SqlitePool` は非公開のため、`read_decimated`/
`aggregate`/`catalog` はカスタム集計SQLを実行できない - この crate は
対象ファイルを自前で読み取り専用オープンし、`tstore_groups`/
`tstore_columns` を直接読む（`read_range` のみ `TsReader::read_range` に
委譲 - カスタムSQL不要なため）。テーブル名/列名はメタテーブルから読み戻した
値を SQL識別子として使う前に形式検証する防御的実装（`plan.rs`）。

詳細は [crates/banto-tsquery/src/lib.rs](crates/banto-tsquery/src/lib.rs) の
モジュールドキュメントを参照。

banto のパッケージ/クレートの消費は **両方とも git タグ参照**
（2026-07-12 決定。GitHub 組織名 banto が取得不能だったため
レジストリ発行は棚上げ。banto の docs/publishing.md 参照）:

```sh
pnpm add "github:tyaro/banto#v0.1.0&path:packages/admin-core"
```

```toml
banto-core = { git = "https://github.com/tyaro/banto.git", tag = "v0.1.0" }
```

## 権利

本リポジトリは自社著作物（All rights reserved）。案件アプリへは
依存ライブラリとして利用許諾で提供し、譲渡対象に含めない
（docs/plan.md §2）。
