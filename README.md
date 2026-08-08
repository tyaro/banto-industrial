# banto-industrial

工場・現場系アプリのための再利用資産層。
[banto](https://github.com/tyaro/banto)（汎用管理画面テンプレート）の上に、
ドメイン寄りの共通資産（タグレジストリ・PLC通信・時系列収集/読み出し）と
それを使う製品アプリ（記録計ほか）を蓄積する。

- 計画: [docs/plan.md](docs/plan.md)（I系 = 資産クレート、R系 = 記録計 **ChronoGazer**、
  W系 = 自動書き込みアプリ **relay-wright**、T系 = タグサーバー **banto-hub**）
- ChronoGazer（記録計）要件定義: [docs/recorder-requirements.md](docs/recorder-requirements.md)
- banto 側のスコープ整理: banto リポジトリの docs/template-scope.md

## 構成

```
crates/
  banto-tags/        I1: タグレジストリ（PLC接続/収集グループ/タグの定義・型・スケーリング）実装済み
  banto-plc/         I2/I2a: PLC通信（読み取り専用。trait + Modbus TCP + MELSEC MC/SLMP）実装済み
  banto-tstore/      I3a: 時系列ストレージ（日次ファイル+スキーマ凍結の自己記述的SQLite）実装済み
  banto-collect/     I3b: 収集エンジン（PLC定期読み出し→banto-tstore書き込み、現在値・イベント供給）実装済み
  banto-tsquery/     I4: 期間クエリ + サーバ側 min/max 間引き（read_range/read_decimated/aggregate/catalog）実装済み
  banto-plc-write/   I5: PLC書き込み（SLMP一括書き込み。読み取り側と分離した専用trait）実装済み
apps/
  chronogazer/       R系: デジタル記録計 ChronoGazer（Tauri + LAN、banto テンプレート由来）
  relay-wright/      W系: 条件付きPLC自動書き込みアプリ（Tauri、W1〜W5 実装済み・実機検証残）安全上の注意は同README参照
  banto-hub/         T系: タグサーバー banto-hub（Tauriなしのヘッドレス axum、REST/WS/MQTT/gRPC でタグ空間を外部公開。T0〜T4/T6〜T12 実装済み・残 T5）
```

### `banto-tags`（I1）

3層のエンティティモデル（`PlcConnection` → `CollectionGroup` → `Tag`）+
CRUD/一覧サービス + スケーリング純関数（`scale_raw`/`unscale`）。
マイグレーションはクレート内 `migrations/` に同梱し、消費側が
`banto_tags::migrate(&pool)` を起動時に呼んで適用する
（`apps/admin-template/core/src/db.rs` の方式を踏襲）。
詳細は [crates/banto-tags/src/lib.rs](crates/banto-tags/src/lib.rs) の
モジュールドキュメントを参照。

### `banto-plc`（I2 / I2a）

読み取り専用・一括読み出しの PLC 通信クライアント。プロトコル差し替えの
境界は [`PlcClient`](crates/banto-plc/src/client.rs) trait（`dyn` 互換に
手書き - I3 が `Box<dyn PlcClient>` で複数 PLC を並行保持する前提）。
同 trait の実装が2本ある:

- **Modbus TCP**（[`ModbusTcpClient`](crates/banto-plc/src/modbus/mod.rs)、I2）:
  外部 `modbus` クレート不使用の自前実装、FC1-4 のみ。デバッグ容易性を
  優先して先行実装（docs/plan.md I2）
- **MELSEC MC/SLMP**（[`SlmpClient`](crates/banto-plc/src/slmp/mod.rs)、I2a）:
  本命ターゲット。フレーミングは承認済み外部クレート `slmp`（MIT）を
  ラップし、この crate 側は「何を読むか」（プランニング）と「どう失敗した
  か」（エラー翻訳）だけを持つ。Modbus 側と方針が異なる理由は
  `slmp/mod.rs` のモジュールドキュメント

- アドレス表記（`crates/banto-plc/src/address.rs`）: `Address` は2表記の
  sum type。計装の参照番号方式（`0/1/3/4` + 4or5桁、`40001` → 保持レジスタ
  offset 0）と MELSEC デバイス方式（`D100`/`M50`/`X1A`、デバイス毎に
  10進/16進が決まる - `slmp/address.rs`）をそれぞれ純関数でパース。
  どちらを使うかは `PlcConnection.protocol` が決め、テキストからの推測は
  しない（2表記は重複しないので、プロトコル設定ミスは個別タグの Bad として
  露出する）
- 要求プランニング（`planning.rs` / `slmp/planning.rs`）: 近接タグを1回の
  要求へ結合（間隙許容・プロトコル毎の上限で分割）、応答→各タグへの逆写像
  まで純関数で設計。256タグ/100ms 収集の実現可否はここが握る
- デコード（`crates/banto-plc/src/decode.rs`）: i16/u16/i32/u32/f32 +
  32bit ワード順。両プロトコルで共用するが既定値が異なる
  （Modbus=HighLow / SLMP=LowHigh - MELSEC は下位ワードが先）
- シミュレータ（`simulator` feature、`modbus/simulator.rs` /
  `slmp/simulator.rs`）: in-process テストダブル。SLMP 側は実バイト列で
  4E バイナリフレームを話す（ラップ先クレートの内部を通すため）。I3 の
  結合テストや R4 の72hソークでも再利用予定
- 性能スモーク実測（ループバック、256タグ・1000回平均）: Modbus は
  3往復/回で約0.4ms/回、SLMP は1往復/回。いずれも 100ms/周期目標に対し
  十分な余裕（実PLC相手の実測はI3で再検証）

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

### `banto-plc-write`（I5）

PLC **書き込み**クライアント。`banto-plc`（読み取り）とは**別クレート・
別 trait**（[`PlcWriteClient`](crates/banto-plc-write/src/client.rs)）に
分離してあり、読み取り専用の消費者（ChronoGazer、banto-collect）は
書き込み API を一切リンクしない — 書き込めるのは意図してこの crate を
依存に加えたアプリ（relay-wright）だけ。SLMP 一括書き込み
（`slmp/planning.rs` で read 側と対称の要求結合）、`TagValue` → レジスタ/
ビット列エンコード（read 側 `decode` の逆写像、32bit ワード順対応）、
read/write 両対応のシミュレータ拡張を持つ。`Address`/`DataType`/
`SlmpConfig` 等の語彙は `banto-plc` から再利用し、依存は
banto-plc-write → banto-plc の一方向のみ。

詳細は [crates/banto-plc-write/src/lib.rs](crates/banto-plc-write/src/lib.rs) の
モジュールドキュメントを参照。

### `apps/chronogazer`（R系）

デジタル記録計 **ChronoGazer**（Tauri + LAN、banto テンプレート由来、
docs/plan.md §4）。PLC通信 + タグデータ保存 + リアルタイム/ヒストリカル/
ハイブリッドトレンド + 計器表示を1台に統合し、既設PLC + 現場PCでチャネル数
自由という価格・柔軟性の優位を狙う。

**現状は R1-A 段階（アプリ骨格のみ）**: ログイン・設定画面と、LANモード
（`banto-serve`）/ Tauri デスクトップの両起動経路は動く。I系クレート
（banto-tags/banto-plc/banto-tstore/banto-collect/banto-tsquery）は
依存には追加済みだが、**まだ REST/Tauri コマンドのどこにも配線されていない**
（`apps/chronogazer/core/Cargo.toml` の依存コメント参照）。監視・
ヒストリカル・イベントの各画面は実データ無しのプレースホルダ表示のまま。
実施計画は [docs/r1-plan.md](docs/r1-plan.md)（Phase R1-A〜R1-D）、要件定義は
[docs/recorder-requirements.md](docs/recorder-requirements.md)を参照。詳細は
[apps/chronogazer/README.md](apps/chronogazer/README.md)。

### `apps/relay-wright`（W系）

条件付き PLC 自動書き込みアプリ（Tauri + banto テンプレート由来、
docs/plan.md §4b）。タグレジストリの読み取り値を条件に、設定ルールに
従って別の PLC デバイスへ値を自動書き込みする。アーミング（再起動で
必ず disarmed に戻る）・dry-run・レート制限ブレーカ（トリップで自動
ディスアーム）・log-before-write・書き込みループのサイクル検出という
安全機構をエンジン（`apps/relay-wright/core/src/engine/`）に持つ。
**稼働中の実 PLC へ自動書き込みするアプリ**であるため、導入前に必ず
[apps/relay-wright/README.md](apps/relay-wright/README.md) の安全上の
注意を読むこと（下記「ライセンス」節にも要旨あり）。

banto のパッケージ/クレートの消費は **両方とも git タグ参照**
（2026-07-12 決定。GitHub 組織名 banto が取得不能だったため
レジストリ発行は棚上げ。banto の docs/publishing.md 参照）:

```sh
pnpm add "github:tyaro/banto#v0.1.0&path:packages/admin-core"
```

```toml
banto-core = { git = "https://github.com/tyaro/banto.git", tag = "v0.1.0" }
```

### `apps/banto-hub`（T系）

FA-Server 型の独立タグサーバー **banto-hub**（Tauri を使わないヘッドレスの
単一 exe、axum + SQLite。docs/tag-server-design.md §3.1）。I系クレート
（タグレジストリ・PLC通信・収集エンジン・時系列ストレージ）を束ね、
タグ空間（= banto-collect の現在値キャッシュ）を **REST / WebSocket /
MQTT publish / gRPC** の4経路で外部（MES・クラウド・自作画面等）へ公開する。
書き込みは per-tag opt-in（既定不可）+ API キースコープ + 監査 +
レート制限ブレーカ付きのパススルーのみ（条件付き自動書き込みは
relay-wright の専管のまま）。演算タグ・内部タグの一元実装、稼働中の
タグ定義変更（オンライン動的変更）にも対応する。

実装状況は **T0〜T4/T6〜T12 実装済み・残 T5**（配布・運用強化・実機検証）。
詳細設計は [docs/tag-server-design.md](docs/tag-server-design.md)、運用手順は
[docs/banto-hub-operations.md](docs/banto-hub-operations.md)、起動方法は
[apps/banto-hub/README.md](apps/banto-hub/README.md) を参照。

## ライセンス

本リポジトリは **MIT ライセンス**（[LICENSE](LICENSE)）。ライブラリ・
アプリともに自由に利用・改変・再配布・販売できる（docs/plan.md §2）。

商用のシステム構築（導入支援・カスタム開発・保守）は tyaro が有償で
提供する。

### relay-wright（PLC自動書き込み）に関する安全上の注意

`apps/relay-wright` は設定されたルールに基づき **稼働中のPLCへ自動的に
値を書き込む**アプリケーション。設定ミスや不具合は意図しない機器動作を
引き起こす可能性がある。MIT ライセンスにより**無保証（AS IS）**で提供
されるため、安全な導入（実機投入前の検証・アーミング/インターロック/
非常停止の確保・適用法令の遵守）は利用者の責任で行うこと。
詳細は [apps/relay-wright/README.md](apps/relay-wright/README.md) を参照。
