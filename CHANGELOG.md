# 変更履歴 (Changelog)

banto-industrial のリリースノート。日付は JST。バージョンは [SemVer](https://semver.org/lang/ja/) 準拠（`publish = false` のワークスペースで、タグはリポジトリ状態の目印）。

## v0.2.0-alpha.5 — 2026-09-08（アルファ）

機能追加 1 件（Modbus の 64bit データ型）と、その作業中に発見した既存バグの修正 1 件（Modbus のワード順）。配布物の構成・前提ランタイムは alpha.3 以降と同じ。

### 追加

- **Modbus のタグに 64bit データ型 `i64` / `u64` / `f64` を追加**（#325）。1 タグ = **4 レジスタ（64bit）**を占有する。オムロンの電力量モニタ＆ロガー KM-D1-ETN のように、全計測項目を 4 ワードの IEEE754 倍精度で公開する機種を **1 タグとして読める**ようになった（従来は `u16` × 4 本のタグに分けて、クライアント側で `f64::from_bits` で組み立てる回避策が必要だった）。
  - **Modbus 接続配下のタグ限定**。SLMP / 予約接続（calc・mem）/ DB 接続（postgres）配下では `dataType` のバリデーションエラーになる。強制は `collection_groups` → `plc_connections` を JOIN する配置チェックで行っており、SQL の `CHECK` 制約には入れていない（`tag_kind` の calc/mem/postgres 規則と同じ方式）。管理 UI のデータ型プルダウンも、収集グループの接続が Modbus のときだけ 64bit 型を出す。
  - ワード順は**接続の設定**に従う（`high_low` = 先頭レジスタが最上位ワード / `low_high` = 4 ワードの並びを反転）。KM-D1-ETN は `low_high`（メーカーマニュアル KANC-718B §12.5）。
  - **書き込みにも対応**。`i64`/`u64` は整数・範囲チェック、`f64` は有限性のみ（非整数値を受け付ける）。
  - 連番登録・構造体登録・オフセットコピーのアドレス自動増分、タグ CSV の入出力も 64bit 型（+4）に対応。
  - **既知の制限**: 値は従来どおり `f64` で運ばれるため、`i64` / `u64` は **2^53（9,007,199,254,740,992）を超える整数で精度が落ちる**。積算カウンタ用途ではこの上限に注意すること。
  - relay-wright の書き込みターゲット（`write_targets`）は 64bit 型を受け付けない（SLMP 前提の relay-wright 固有リソースのため）。

### 修正

- **Modbus 接続のワード順設定が収集経路で無視されていた**（#325 の作業中に発見した既存バグ）。`plc_connections.word_order`（migration 0010）について、次の 3 つが噛み合っていなかった。
  - 収集ポーリング（`banto-collect`）は列を読まず **HighLow 固定**で動いていた。
  - read-on-demand と書き込み（`banto-broker`）は列を読んでおり、列の既定 `'low_high'` がそのまま効いて **LowHigh** で動いていた。
  - 管理 UI の接続フォームはワード順を **SLMP 選択時しか表示していなかった**ので、Modbus 接続には事実上必ず `'low_high'` が保存されていた。

  結果、既定設定の Modbus 接続では**同じ `u32`/`f32` タグの値が、収集経路と即時読み取り／書き込み経路でワード反転して食い違って**いた。オーナー決定として **Modbus は `'high_low'`（Modbus/IEEE 慣習）に統一**した。収集経路が列を読むよう修正し、broker 側の解釈をそれに揃えている。

  - **更新時の注意**: `banto_tags::migrate`（Hub 起動時に自動実行）で、**既存の `modbus-tcp` 接続の `word_order` が一律 `'high_low'` に書き換わる**（migration 0017）。これは収集経路の現行動作と蓄積済み履歴データの解釈を変えないための「実態への同期」だが、**MCP や REST を直接叩いて Modbus 接続に明示的に `'low_high'` を設定していた場合、その設定は失われる**。更新後に設定し直すこと。
  - **管理 UI の接続フォームで Modbus でもワード順を選べる**ようにした。新規 Modbus 接続の既定は `'high_low'`、SLMP は従来どおり `'low_high'`。
  - 構成パッケージのインポートでも、`wordOrder` を持たない旧パッケージの Modbus 接続は `'high_low'` として読み込む（当時の実挙動に合わせるため）。

### 既知の制限（アルファ）

alpha.4 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装）。加えて上記の `i64`/`u64` の 2^53 精度制限、および **64bit 型の実機検証（KM-D1-ETN 実機での読み取り）は未実施**。

## v0.2.0-alpha.4 — 2026-09-08（アルファ）

`v0.2.0-alpha.3` の不具合修正のみ。配布物の構成・前提ランタイムは alpha.3 と同じ。

### 修正

- **サービス起動失敗時に `Stopped` を二重報告していた**（#330）。起動失敗パスの `return` が `runtime.block_on(async move { .. })` に渡した async ブロックからしか抜けず、`run_service_body` 末尾の `report_status(Stopped, Win32(0))` が必ず実行されていた。実機ではログに `サービス状態の報告に失敗しました: IO error in winapi call` が残っていた（alpha.3 の実機検証で発見、設計 §8.3）。
  - 2 回目の報告は**正常終了（`Win32(0)`）**なので、Win32 API 呼び出しが失敗してくれるおかげで #327 のサービス固有終了コード（プロファイルロック保持 = `2` / その他の `HubStartError` = `3` / tokio ランタイム構築失敗・`Configured` 収集の開始失敗 = `1`）が SCM に残っていたにすぎない。API の失敗頼みをやめ、`Stopped` を一度報告したら以降の報告を送らないゲートで明示的に止める。
  - 併せて、起動失敗で終わったときにログへ `Windows サービスを停止しました` を出さなくなった（起動できていないのに「停止しました」と出るのは誤解を招くため）。**このログ行を「サービスが終了した」の目印にしている運用があれば影響する**。正常な停止時には従来どおり出力される。

### 既知の制限（アルファ）

alpha.3 と同じ（実 DB 検証 S7 未実施、`admin` スコープ API キーはサーバー全権、サイドカーはループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提、OPC UA #201 / SQL Server / SLMP イベント PUSH #258 は未実装）。

## v0.2.0-alpha.3 — 2026-09-07（アルファ）

第 3 アルファ。`v0.2.0-alpha.2` 以降の 10 コミット分。**一体インストーラ（シェル・Hub・elev・サイドカー同梱）**を完成させ、**VC++ 再頒布可能パッケージの前提を排除**した。評価用であり、実 DB 検証（S7）・72h soak・実機サインオフ（#210）等のリリースゲートが未完了なのは alpha.2 と同じ。

### 一体インストーラ（[docs/banto-hub-installer-design.md](docs/banto-hub-installer-design.md)）

- **NSIS インストーラ 1 本に 4 exe を同梱**（#322、I1）: `banto-hub-shell.exe`（main）/ `banto-hub.exe` / `banto-hub-elev.exe` / `banto-hub-sink.exe` を `C:\Program Files\BantoHub\`（PerMachine）に配置。WebView2 は `EmbedBootstrapper`。PREINSTALL で稼働中のサービス（`BantoHubSink` → `BantoHub`）とシェルを停止し、POSTINSTALL で両サービスの登録・Operators への ACL 付与・ProgramData 作成・停止していたサービスの再開まで行う（**収集は勝手に始めない**方針は継続）。
- **サイドカー設定の探索順**（#321、I2）: `BANTO_HUB_SINK_CONFIG` → `%ProgramData%\BantoHub\` → exe 隣。`banto-hub-sink.toml.example` を同梱。
- **リリースビルドの一本化**（#323、I3）: `scripts/build-release.ps1` で UI ビルド → 4 exe → インストーラ生成 → リリース名へのリネームと `SHA256SUMS.txt` 生成までを 1 コマンドに。
- **Windows 実機検証**（#326、I4）: 新規インストール / 稼働中の上書き / Hub 単体インストーラからの更新 / アンインストールが設計どおり動くことを確認（設計 §8）。
- 追従修正: プロファイルロック保持中の `sc start BantoHub` が `START_PENDING` で固まる問題を即時失敗に変更（#327）。サイドカーのサービスログを `%ProgramData%` へ移し、silent インストール時のデスクトップショートカットを削除（#328）。

### VC++ ランタイム前提の排除（設計 §4.7、決定 11）

- MSVC 向けビルドで **C ランタイムと UCRT を静的リンク**（`.cargo/config.toml` の `+crt-static`）。4 exe から `VCRUNTIME140.dll` / `VCRUNTIME140_1.dll` / `api-ms-win-crt-*.dll` のインポートが消え、**Visual C++ 再頒布可能パッケージが入っていない PC でも動く**。
- 主目的は **exe 単体配布**を救うこと。インストーラ経由なら redist を同梱すれば済むが、単体 exe にはインストーラが無く、利用者（オフライン工場 PC を想定）が自力で導入する手段を持たない。
- 併せて Windows 8.1 / 7 での KB2999226（Universal CRT 更新）前提も消える。代償として vcruntime の更新は Windows Update ではなく再ビルド・再配布で届けることになる。サイズ増は 1 exe あたり +110〜145 KB。

### 配布物（Windows x86_64、ローカルビルド）

- `BantoHub_<ver>_x64-setup.exe` — NSIS インストーラ。**alpha.2 の「Hub 本体のみ」から変わり、シェル・Hub・elev・サイドカーの 4 exe を同梱**してサービス登録まで行う。
- `banto-hub-<ver>-windows-x86_64.exe` / `banto-hub-shell-<ver>-windows-x86_64.exe` / `banto-hub-elev-<ver>-windows-x86_64.exe` / `banto-hub-sink-<ver>-windows-x86_64.exe` — 単体配布（サービス化しない評価用途向け、決定 9 で継続）。
- `SHA256SUMS.txt`。
- **前提ランタイム**: VC++ 再頒布可能パッケージは不要。WebView2 のみ、未導入の PC ではインストーラが取得しに行く（単体 exe でシェルを使う場合は WebView2 が要る）。
- Tauri バンドル（`tauri.conf.json`）の版数は数値制約のため `0.2.0`（プレリリース識別子なし）。

### 既知の制限（アルファ）

- alpha.2 の既知の制限はそのまま残る（**外部 DB 連携の実 DB 検証 S7 未実施**、`admin` スコープ API キーはサーバー全権、サイドカーは DB パスワードを平文で受け取るためループバック運用前提、72h soak・実機サインオフ #210・性能ハーネス #211 未実施、通信は平文 + 閉域 LAN 前提）。
- 未実装: OPC UA Server（#201）、SQL Server（設計 §6-2、第 2 段）、SLMP イベント PUSH（#258）。

## v0.2.0-alpha.2 — 2026-09-07（アルファ）

第 2 アルファ。`v0.2.0-alpha.1` 以降の 36 コミット分。**外部 DB 連携（#228 DB Source / #229 DB Sink）を実装**し、**デスクトップシェル（Tauri）を配布物に含めた**。評価用であり、実 DB 検証（S7）・72h soak・実機サインオフ（#210）等のリリースゲートは未完了。

### 外部 DB 連携（[docs/banto-hub-external-db-design.md](docs/banto-hub-external-db-design.md)）

- **DB 接続**（`protocol = postgres`）: 既存の接続→グループ→タグの 3 階層に PostgreSQL 接続を追加。パスワードは応答に出さず `passwordSet` のみ、更新は省略で保持・空文字で消去。接続テスト（`SELECT version()`）と MCP `test_saved_connection`。
- **DB Source**（Hub 内）: グループの SQL（`querySql`、1 グループ = 1 SELECT）と `db` タグ（address = 結果列名、数値 / bool / 日時、読み取り専用）。describe + cast 方式で列型を吸収、Quality 変換（NULL / 0 行 / クエリ失敗 = Stale → 2 回で Bad / 接続断 = バックオフ）、収集の開始 / 停止に連動、task の異常終了は supervisor が再生成。UI（Drawer の SQL 入力・列名候補・ツリーの SQL バッジ・状態画面の dbSource 節）、CSV（`tagKind=db` を既存列で受理）、config パッケージ。
- **DB Sink**（別プロセスのサイドカー `apps/banto-hub-sink`）: Hub 側に sink group の設定（`/api/sink/groups`、pending queue に載らず即時適用）と `GET /api/sink/config`（`admin` + `read` の API キー、ループバック前提）/ `PUT /api/sink/status`。サイドカーは banto-tagclient SDK で購読し、long 形式（`ts, tag_id, external_name, value, quality`）へバッチ INSERT（上限付きキュー、at-least-once、1s→30s バックオフ、テーブル検査と推奨 DDL の表示、DDL は発行しない）。Windows サービス `BantoHubSink`（`install` / `grant-service-acl` は同梱の `banto-hub-elev` で Operators の ACE を付与）。UI（sink 画面・推奨 DDL・API キーのプリセット・状態画面の DB Sink 節）とデスクトップシェルの**サービス一覧**（Hub / Sink の SCM 状態と起動停止）。
- **MCP**: sink group 管理 5 ツールを追加し **計 37 ツール**（[docs/banto-hub-mcp-reference.md](docs/banto-hub-mcp-reference.md)）。
- **CI**: ubuntu の PostgreSQL サービスコンテナで Source / Sink の統合テストを常時実行。
- 検証手順: [docs/external-db-test-2026-09.md](docs/external-db-test-2026-09.md)（S3 の手動 smoke A-1〜A-14、S7 の B-1〜B-13）。

### その他

- Rust toolchain を 1.94.1 → **1.98.1** に更新（#294）。sysinfo 0.39、vite-plugin-svelte 7、eslint 10.9 ほか dependabot 8 件。
- banto-tagclient SDK（#123）は機能・実 Hub / LAN 検証・配布サイズ・`v0.1.0` 固定まで完了しクローズ。設計文書 §4.5 に実機で判明した挙動（on-change 配信・書き込み直後の旧値）を記録。
- `Cargo.lock` を版数に同期（#292）。sink group 更新の SQLite `database is locked` 競合を `BEGIN IMMEDIATE` で修正（#309）。

### 配布物（Windows x86_64、ローカルビルド）

- `banto-hub-<ver>-windows-x86_64.exe` — Hub 本体（UI 埋め込み）。
- `banto-hub-shell-<ver>-windows-x86_64.exe` — **デスクトップシェル（Tauri、UI 埋め込み）**。同じディレクトリに `banto-hub-elev.exe` を置く。
- `banto-hub-elev-<ver>-windows-x86_64.exe` — UAC ヘルパ。
- `banto-hub-sink-<ver>-windows-x86_64.exe` — DB Sink サイドカー。
- `BantoHub_<ver>_x64-setup.exe` — NSIS インストーラ（**Hub 本体のみ**。シェル / サイドカーは未同梱、T17 §2.3 のとおり）。
- `SHA256SUMS.txt`。
- Tauri バンドル（`tauri.conf.json`）の版数は数値制約のため `0.2.0`（プレリリース識別子なし）。

### 既知の制限（アルファ）

- **外部 DB 連携の実 DB 検証（S7、別マシン PostgreSQL・24h）は未実施**。シェルのサービス一覧からの Sink の起動停止も実サービス未検証。
- `admin` スコープの API キーはサーバー全権。サイドカーは DB パスワードを平文で受け取るため Hub と同一マシンのループバック運用が前提。
- 72h soak・実機サインオフ（#210）、Windows 実機往復・性能ハーネス（#211）は未実施。通信は平文 + 閉域 LAN 前提。
- 未実装: OPC UA Server（#201）、SQL Server（設計 §6-2、第 2 段）、SLMP イベント PUSH（#258）。

## v0.2.0-alpha.1 — 2026-09-06（アルファ）

初のアルファ評価版。**banto-hub（タグサーバー）**を中心に、MELSEC SLMP / Modbus TCP からの収集・書き込みと多様な外部インターフェースを、**実機検証済み**で提供する。**評価用**であり、72h soak・実機サインオフ（#210）等のリリースゲートは未完了。`v0.1.0`（2026-09-02、最初のリリースタグ）以降の 46 コミット分。

### banto-hub（タグサーバー）

- **収集**: MELSEC SLMP / Modbus TCP ドライバ（いずれも実機 R08ENCPU で検証済み）。broker 抽象化で read/write が1セッションを共有。
- **データ型**: i16 / u16 / i32 / u32 / f32 / bit、文字列（UTF-8 / Shift-JIS）、ワードデバイスのビット（`Dxxx.0`〜`.F`、16進）。
- **書き込み**: 単票・レシピ一括（原子的な事前ゲート＝1件 NG なら無書込）。安全ゲート（writable / スコープ / レート制限 / 値変換 / write-control）。
- **タグ登録 UX（T18）**: グリッド編集・TSV 貼付・連続登録・CSV 入出力・接続/グループ Drawer とツリー。
- **運転（T16/T17）**: デスクトップシェル＋Windows サービスモード、profile 排他、Desktop↔Service 切替。試運転モードとロックダウン（tag-server-design.md §5.6）。
- **UI/UX 群（T19、UX-30〜48）**。
- **外部インターフェース**: REST / WebSocket / MQTT / gRPC / **MCP**。
  - **MCP**: データ面（`list_tags` / `read_tag_values` / `read_tag_now` / `get_server_status` / `write_tag_value` / `write_recipe`）＋**構成補助（管理面）ツール**（接続 / グループ / タグ CRUD・設定 gRPC/MQTT/retention・収集 start/stop・write-control・API キー発行/失効・lock_down）＝**計 31 ツール**（T19 S5 / T20 / T21）。`admin` スコープ・全操作の監査（`origin=mcp`）・不可逆操作の `confirm` で保護。

### 実機検証（2026-09-06）

- **SLMP（R08ENCPU）**: 全データ型の read/write、文字列/レシピ/ビット、MCP 管理面での構築まで（実バグ0）。
- **Modbus TCP（port 502）**: 保持レジスタ/コイル write（FC5/6/16）・保持/コイル/入力 read を、MCP 管理面経由で構築して検証（#219、実バグ0）。

### 権限・安全

- role（viewer / editor / admin）と `require_editor` ゲート、admin 限定ルートの REST テストを整備（#231）。
- API キーのスコープ（`read` / `write:{tag}` / `admin`）と監査。

### 既知の制限（アルファ）

- **72h soak・実機サインオフ（#210）**、Windows 実機往復・性能ハーネス（#211）は未実施（リリースゲート）。
- 通信は v1 = **平文（HTTP / gRPC 平文）＋閉域 LAN 前提**。TLS は未対応。
- **`admin` スコープの API キーはサーバー全権に相当**（構成変更・別 admin キー発行・lock_down）。ロックダウン後も admin スコープで構成可（意図的緩和）。**配布は厳格に管理**すること。
- 未実装: OPC UA Server（#201）、外部DB連携（#228 / #229）、SLMP イベント PUSH（#258）。
- banto-tagclient SDK は実装途上（S4a まで実装、実 Hub/LAN 統合は今後、#123）。
- **GUI バンドル（Tauri / MSI installer）の版数は本タグでは未同期**。本アルファはソース＋hub バイナリの評価用で、配布物（installer）ビルドは別途。

### 関連ドキュメント

- 全体地図: [docs/README.md](docs/README.md)
- 設計の正: [docs/tag-server-design.md](docs/tag-server-design.md)
- 運用ガイド: [docs/banto-hub-operations.md](docs/banto-hub-operations.md)
- MCP インターフェース: [docs/banto-hub-mcp-reference.md](docs/banto-hub-mcp-reference.md)

## v0.1.0 — 2026-09-02

最初のリリースタグ（T0〜T18-6・H 系・banto-tagclient S4a 相当）。
