# T5（配布・運用強化）ローカルセッション引き継ぎ

> **📦 アーカイブ（2026-08-14）**: 役目を終えたセッション引き継ぎメモ。内容は
> [banto-hub-operations.md](banto-hub-operations.md)・[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)
> に反映済み。残る T5-5（72h soak 実行 + 実機サインオフ）の現行の追跡は
> [banto-hub-remaining-plan.md](banto-hub-remaining-plan.md) Phase 4。本書は経緯として保存。
> T5-4（本書 §3 表）/T5-5 の採番は docs/plan.md §4c もこの定義へ統一済み
> （2026-09-01、banto-hub-desktop-plan.md §16.5 の残件を解消）。

状態: **アーカイブ（役目終了・経緯として保存）**。
最終検証日(コード照合): 2026-09-01

作成: 2026-08-06（クラウドセッションの区切り）。banto-hub の T 系のうち
実機・Windows 依存のない範囲（T0〜T4 / T6〜T8）が main に入った時点の
引き継ぎ。次はローカル（Windows）環境で T5 を進める。

## 1. 現在地

banto-hub は次を備えた状態で main にある（各 PR: #26 設計+T0 / #27 T1 /
#28 T2 / #29 T3 / #30 T4 / #31 T6 / #32 T7 / #33 T8）:

- 外部 IF 4経路: REST（utoipa OpenAPI 付き）/ WebSocket 購読 /
  MQTT publish（rumqttc・retain・LWT）/ gRPC（proto/tagserver/v1）
- 収集: Modbus TCP + MELSEC SLMP（SLMP は banto-broker の共有単一
  セッション経由 — 読み書き同一セッション）
- 書き込み: API キー `write:` スコープ + 8段安全ゲート（受付は起動時必ず
  OFF・レート制限トリップ・log-before-write・監査）。ワードデバイスの
  ビット書き込み（`D100.5`、RMW + 確認読み）対応
- 演算タグ・内部タグ（banto-expr、`calc`/`mem` 予約接続、retain 復元）
- オンライン部分再構成（I7 — 変更接続だけ入れ替え、演算タグのみの変更は
  収集エンジン無接触）
- 管理 UI（SvelteKit、embed-ui feature で単一 exe 配信）
- テスト規模: ワークスペース合計 約1,000本（banto-plc 197 /
  banto-plc-write 97 / banto-tags 111 / banto-collect 47 / banto-broker 17 /
  banto-expr 118 / banto-hub-core 200+ / relay-wright-core 273 ほか）

作業ブランチ: `claude/tag-server-handoff-docs-fmv4ls`（main と同期済み。
ローカルでは同名ブランチを main から切り直して使うか、新ブランチでよい）。

## 2. ローカル環境セットアップ（Windows 想定）

```powershell
git clone https://github.com/tyaro/banto-industrial
cd banto-industrial
pnpm install          # クラウドと違い codeload.github.com に直接届くので素直に通るはず
cargo build           # Tauri アプリ含め全ワークスペースがビルド可能（GTK 制約はLinuxコンテナ固有）
pnpm --filter banto-hub build            # SvelteKit 静的ビルド → apps/banto-hub/build
cargo build -p banto-hub-core --features embed-ui --release
$env:BANTO_ALLOW_SETUP="1"; .\target\release\banto-hub.exe   # 初回セットアップ → http://127.0.0.1:8722
```

- E2E（Playwright）: `pnpm e2e`（chronogazer 用設定。banto-hub 用の
  e2e config は未作成 — T5 で追加候補）
- 注意: ルート package.json の engines は node >= 24（クラウドは 22 で
  動かしていた。ローカルは 24 系推奨）

## 3. T5 のスコープ（設計 doc §8/§9）とスライス案

| スライス | 内容                                                         | 備考                                                                                                                                                                                                                |
| -------- | ------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| T5-1     | Windows サービス化の検討・実装（`windows-service` クレート） | 対話セッション不在での動作検証が必要（サービスは Session 0）。コンソール起動 + タスクスケジューラ登録の手順書だけで済ます判断もあり — 実運用の要求次第                                                              |
| T5-2     | インストーラ（既存2アプリのインストーラ構成を踏襲）          | exe + 初期設定 + 自動起動登録                                                                                                                                                                                       |
| T5-3     | 運用ガイド（docs）                                           | ポート・API キー運用・書き込み受付の運用手順・ビット書き込みのワード専有規約（§6.1）・TLS はリバースプロキシ終端（§5.6）・MQTT/gRPC 設定                                                                            |
| T5-4     | ソークテスト                                                 | banto-collect の 72h ソーク雛形を流用し、収集 + WS 購読 + MQTT 発行を維持した連続稼働。出荷条件（§8）                                                                                                               |
| T5-5     | 実機検証（オーナー実施）                                     | W5 の残項目と共通: 実機 SLMP 互換・**同時セッション数上限**（broker 単一セッションの実機での価値実証）・TypedDevice/PLCString(Shift-JIS)。T8 追加分: 実機でのビット RMW・確認読み（PLC スキャンとの競合検出）の観察 |

## 4. 既知のバックログ（T5 と独立。優先度はオーナー判断）

- **I9**: Modbus 書き込み（banto-plc-write への FC5/6/15/16 + broker の
  プロトコル抽象化。ビット書き込みは FC22 Mask Write でアトミック化可能 —
  §6.1 記載）
- relay-wright タグモニタ UI の BitInWord 対応（ドライバは対応済み）
- MQTT のタグ毎発行モード設定 / 削除タグの retain クリア（§5.3 注記）
- broker: 削除セッションのタスク即時 join（現在は全ハンドル drop 待ち）
- SLMP の CPU 種別・アクセスルート・word order のレジストリ設定
  （現在は banto-plc 既定値固定 — T2-0 の既知制約）
- 既存2アプリの LAN モード既定ポート 8721 重複（§10-7 発見）
- `banto-tagclient` SDK の起票（T1 完了時起票と決定済み・未起票。
  SCADA 計画の具体化と同時に着手 — §7）
- hub の audit-log 保持設定 API / ui-settings ルート（chronogazer には
  ある。必要になったら）
- T6 残judgment: 演算/内部タグの tstore 記録要否（§10-12 残項目）

## 5. 運用ルール（ローカルセッションでも同じ）

- CLAUDE.md の AI 役割分担（実装 = sonnet サブエージェント、監査・設計 =
  上位モデル）と prettier 規約はローカルの Claude Code セッションでも
  そのまま適用される（リポジトリ直下の CLAUDE.md を自動読込）
- マイルストーン毎に PR → CI グリーン → オーナー判断でマージ、の
  リズムも継続を推奨
- サブエージェントが長スライスで無言停止することがある（本セッションで
  3回発生）。「git status のファイル mtime + cargo プロセスの有無」で
  生存確認し、停止なら SendMessage で再開（それでも駄目なら新エージェント
  に交代）が有効だった
