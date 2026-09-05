# 変更履歴 (Changelog)

banto-industrial のリリースノート。日付は JST。バージョンは [SemVer](https://semver.org/lang/ja/) 準拠（`publish = false` のワークスペースで、タグはリポジトリ状態の目印）。

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
