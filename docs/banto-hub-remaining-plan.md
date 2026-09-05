# banto-hub 残作業 実行計画（2026-08 全量監査ベース）

作成日: 2026-08-12
状態: **オーナー承認・実行中**。2026-08-12 に docs 14 文書 + コード + GitHub を全量監査した
結果を実行順に整理したもの。**進捗（2026-08-12）: Phase 0（#119）、監査フォロー①=stale ガード（#121）・
②最小対応（#121）・③稼働中 import ガード（#126）、profile_lock フレーク（#122）、Swagger UI（#124）、
P3-a audit retention（#125）、P3-b SLMP word order（#127）、P3-c SLMP 構造化エラー（#129、文言パース完全削除、
[h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md) 参照）マージ済み。A群の自己完結分は完了。
**進捗追記（2026-08-14）**: Phase 1 docs 整合（#128）完了。B群の T18 は
[banto-hub-t18-design.md](banto-hub-t18-design.md) にて T18-2〜T18-5b 完了（#132〜#154。T18-5a 第2段は
実測ファースト判定=目標達成、windowed 化バックログ降格）。②failed→再試行（再キュー）導線を
`claude/pending-requeue` ブランチで実装完了（下記 Phase 0.5 参照）。P3-c の broker
session/transport 共通化は `banto_plc::dial_slmp` への集約で完了（2026-08-14、
improvement-plan.md §H9）。`classify_slmp_error` の重複統合も
`banto_plc::SlmpErrorClass`/`classify_slmp` への集約で同日完了し、P3-c は
完全完了（残スライスなし）。
**進捗追記（2026-09-01）**: P2-a（切替ウィザード UI の Windows 実機経路）は
2026-08-31/09-01 のオーナー実機検証で完了（下記 Phase 2 参照）。B群の T18 は
desktop-plan §9.5（TAG-UX-7〜9）を受けて 2026-08-27 に T18-6（接続/グループ
Drawer とツリー意匠）も完了（#173/#174/#176/#177）。banto-tagclient SDK は
Issue #123 として起票済み（起票は完了・着手は SCADA 計画具体化と同時、
下記「別途スケジュール」参照）。残るは Phase 2 実機検証系（T18-5c/d 含む）と
Phase 4 リリースゲート（H7① 実機 soak）のみ。**
**進捗追記（2026-09-02〜06）**: その後 T19（UX-30〜48 の UI/UX 群、S1〜S5）・T20（文字列
read/write・構造体タグ登録＋オフセットコピー・レシピ一括書き込み・ワードデバイスのビット
.0〜.F、原子性バグ修正 #265）・T21（構成補助 MCP＝管理面 31 ツール、#266〜#274）を実装。
現時点の残件は実機・需要待ちの #219/#210/#211/#123/#201（詳細は各設計書・README.md）のみ。
個別スライスの詳細設計は既存の
[plan.md](plan.md)・[tag-server-design.md](tag-server-design.md)・[banto-hub-desktop-plan.md](banto-hub-desktop-plan.md)・
[banto-hub-t16-design.md](banto-hub-t16-design.md)・[banto-hub-t17-design.md](banto-hub-t17-design.md)・
[improvement-plan.md](improvement-plan.md) を正とし、本書はそれらの残項目の**優先順位と着手順**を定める索引。
最終検証日(監査): 2026-08-12（進捗行の同期: 2026-09-06）
基準コミット: 889f622（監査時）／`d5ba55e`（T18-5a 判定記録後の現 main、2026-08-14）

## 0. 前提と現況

- 現 main（`69271f6`）は本セッションの成果を反映済み: #119（pending queue + 構成パッケージ）、
  #120（tags-revision フレーク）、#121（① stale ガード + ② 最小対応）、#122（profile_lock フレーク）、
  #124（Swagger UI）、#125（P3-a audit retention）、#126（③ 稼働中 import ガード）、
  #127（P3-b SLMP word order）、および docs・オーナー決定・Issue #123（SDK 起票）。
- dependabot 群（2026-08-09 発生）は消化済み。
- テスト用 PLC: 三菱 R08ENCPU、IP `192.168.11.200`（検証・テスト環境のため記載可、2026-08-12
  オーナー）。SLMP ポート `3100`〜`3110` / `3200` / `3201` / `5200` が利用可。安全アドレス帯は
  `D3000-4999` / `M1000-2000`。複数ポートを多重接続・多重クライアント検証（1 ポートあたり
  1 接続の制約実証）に使える。

## 1. 実行フェーズ

### Phase 0 — PR #119 の緑化とマージ（完了：2026-08-11）

PR #119 には main 未反映の重要実装が含まれていた。マージが全ての起点。**完了**。

- **P0-a（完了）**: E2E smoke fail は決定的回帰だった。新 spec `banto-hub-pending-apply-cancel`
  がファイル名順で smoke より先に走り、`beforeAll` の `fetchAuthToken` が共有 DB を初期化して
  いた。`banto-hub-status-pending-apply-cancel` にリネームし順序契約を doc コメントに明文化
  （コミット `f36d72f`）。露出した tags-revision のトースト蓄積フレークは別ブランチ
  `claude/strange-euclid-00ca47`（`f985eb2`）で堅牢化済み。
- **P0-b（完了）**: 上位モデル監査を実施。pending queue の状態機械（遷移ガード）・認証の一貫性・
  DB migration・構成パッケージ export の秘匿除外（allowlist 方式で安全）は問題なしと確認。
  3 件のフォロー項目を検出（下記 Phase 0.5）。
- **P0-c（完了）**: マージ済み（merge commit `ec1a546`）。PR 内 docs 追補で「運転中一律ロック」→
  「pending queue + 手動適用」方針改定と T17-5 は各設計書へ反映済み。

取り込んだ主実装: TAG-P0-3 の pending queue 方式（一律 409 拒否からの改定）、
T17-5 構成パッケージ export/import、`.gitattributes`。

### Phase 0.5 — pending queue の監査フォロー（P0-b 検出）

- **① 重大 → 完了（#121）**: pending apply 時にサーバー最新状態への再検証が無く、`plc_connections` /
  `collection_groups` の apply が無警告で上書きしていた問題を、per-resource フィンガープリント方式
  （enqueue 時に対象の現在値を保存 → apply 直前に再突合、不一致/消失なら Conflict 失敗）で解消。
  グローバル revision 突合は apply が revision を上げるためキュー複数適用を壊す点を回避（回帰テスト済み）。
- **② 中 → 完了（`claude/pending-requeue`、2026-08-14）**: `failed` 行のキャンセル導線（cancel を
  Failed→Canceled 拡張）と `failure_reason` への実エラー保持は #121 で実装済み。**`failed` からの
  「再試行（再キュー）」導線を追加実装**: `requeue_pending`（Failed→Pending、`base_fingerprint`・
  `payload`・`base_configured_revision` は据え置き）、`POST /api/pending-changes/{id}/requeue`、
  status 画面の「再試行」ボタン。fingerprint を再計算しないことで、再 apply 時に既存の
  ステイルガードが再チェックされ、一過性失敗（収集稼働中の 409 等）は再試行で回復し、真の
  コンフリクト（enqueue 後の別経路変更）は再度安全に fail する。
- **③ 中 → 完了（#126）**: 稼働中 import の誤成功表示（`tagRegistryAdmin.ts` が 202（queued）を作成済み型と
  偽り `configPackageAdmin` が依存項目をサイレントスキップ、UI は無条件に成功トースト）を、import は収集停止中
  のみ許可する事前ガード＋戻り値型の是正（202 を `QueuedWhileRunningError` で弾く）で解消。

### Phase 1 — docs 整合の是正（完了：PR #128、2026-08-12）

CLAUDE.md の状態欄同期規約（H8）に反する乖離を一括是正。実装を伴わない docs 修正のため
メインセッション直で可。**下記5件すべて #128（コミット `d54d848`）で反映済み**（2026-08-14 に
全件を現行 docs と突き合わせて解消を再確認済み）。

1. [banto-hub-t14-design.md](banto-hub-t14-design.md) の状態行「実装未着手」→ T14 完了を反映。
2. [improvement-plan.md](improvement-plan.md) H5 欄を更新（banto-hub 分の E2E は 9 spec +
   vitest 8 ファイルで実質完了。relay-wright 分のみ未着手として残す）。
3. [ux-plan.md](ux-plan.md) に T13-2/T13-3 → T18-4/T18-2 移管を注記し二重管理を終える。
4. [banto-hub-operations.md](banto-hub-operations.md) §3 に H10③ の read スコープ拡張
   （`read:{conn}.{group}.{tag}` 完全一致 / `read:{conn}.{group}.*` ワイルドカード）を追記。
5. [banto-hub-t17-design.md](banto-hub-t17-design.md) §10 の「未了」リストを実機検証結果
   （Operators 委任 OK）に合わせて更新。

### Phase 2 — 実機検証（テスト PLC 活用）

Phase 0 マージ後の main を実機で確認。テスト PLC の複数ポートを使う。

- **P2-a（完了、2026-08-31/09-01 オーナー実機検証）**: 切替ウィザード UI の Windows 実機経路
  （Desktop→Service UI 操作・自動起動 UAC・Service→Desktop 逆経路・UAC キャンセル時の異常系）。
  [banto-hub-t16-design.md](banto-hub-t16-design.md)「切替ウィザード UI 実装メモ」で当時未検証と
  記録されていた経路だが、実機で3層の不具合（remote origin ケイパビリティ・ACL 未宣言・管理 UI の
  `/api/v1/*` 誤用）を発見・修正した上ですべて確認済み。
- **P2-b**: profile 先行作成 → Desktop Hub 起動のフル E2E（[banto-hub-t17-design.md](banto-hub-t17-design.md)
  §12 の任意追加確認）。
- **P2-c**: テスト PLC `192.168.11.200` の複数ポート（例 `3100`〜`3105`）で SLMP 収集を
  同時に張り、「1 ポートあたり 1 接続」制約と多重クライアント購読の維持を実測。`5200`
  （実 R08ENCPU）も 1 本含める。

### Phase 3 — バックログ実装（採番済み・実装系）

- **P3-a（完了、2026-08-12、ブランチ `claude/audit-retention`）**: audit ログ retention の配線
  （低リスク・影響大）。[audit.rs:200](../apps/banto-hub/core/src/audit.rs) の `prune` は休眠実装で
  REST/起動パスに未配線 → 監査ログ無制限成長、だった問題を解消。chronogazer/relay-wright と同等の
  `AuditSettings`（`crate::settings`、既定 90日/100,000件）を追加し、`GET/PUT /api/audit-log/config`
  （admin 限定）と `crate::runtime::HubRuntime::start` の起動時剪定 + `POST /api/audit-log/list` の
  opportunistic 剪定を配線した（[docs/banto-hub-operations.md §9](banto-hub-operations.md)参照）。
- **P3-b（完了、#127）**: SLMP の word order を接続設定から露出。banto-broker
  （`SessionDirectory::ensure_connection`）と banto-collect（`config::slmp_config_for`、
  relay-wright/chronogazer が使う経路）の両方が `host`/`port` 以外を `SlmpConfig::default()` 固定に
  しており、ワード順の異なる機種で u32/f32 の値化けに直結していた問題を解消。`plc_connections.word_order`
  （migration `0010`、既定 `low_high` で後方互換）→ フォーム（"slmp" 選択時のみ）まで end-to-end 配線。
  **残: CPU 種別 / アクセスルート（network/PC/IO/area id）は別スライス候補**（`slmp_config_for` の
  doc comment に "Known limitation" として明記）。
- **P3-c（完全完了、文言パース削除2026-08-12・transport共通化/classify_slmp_error統合2026-08-14）**:
  H9 SLMP 構造化エラー（[improvement-plan.md](improvement-plan.md) §H9）。オーナーが
  `tyaro/slmp`（git 依存、tag `v0.2.0`）で構造化エラーを実装したのを受け、banto 側（dep 更新・
  `END_CODE_MARKER` 等の文言パース完全削除・tripwire 構造化版への置換・`deny.toml` の git 依存許可）
  を同日中に完了。API 仕様と受け入れ条件は
  [h9-slmp-structured-error-spec.md](h9-slmp-structured-error-spec.md)（状態: 実装済み）。broker
  の session/transport 共通化（banto-broker/banto-plc/banto-plc-write の SLMP connect 重複）は
  `banto-plc` の共有ヘルパー `dial_slmp` への集約で完了。`classify_slmp_error` の重複統合
  （banto-plc/banto-plc-write の同一5分岐 match）も `banto-plc` の `SlmpErrorClass`/`classify_slmp`
  への集約で完了し、残スライスなし（詳細は improvement-plan.md §H9）。

### Phase 4 — リリースゲート（T5 の唯一の残り）

- **P4-a**: 72h soak 出荷判定の記録テンプレート整備（[banto-hub-operations.md](banto-hub-operations.md)
  §12 が未定義）。
- **P4-b**: T5-5 / H7① = テスト PLC を使った 72h soak 実行 + 実機最終サインオフ。
  ハーネス `tests/soak.rs`（`#[ignore]`、`SOAK_DURATION_SECS=259200`）は完備、実行のみ残。

### 別途スケジュール（未着手スライス・オーナー起票判断）

- T16-3（共通運転バー、依存 T18-1）は未着手のまま残る。T18-2〜T18-6（初回導線／複製・一括・
  CSV／モニタ導線／性能・出荷検証／接続・グループ Drawer とツリー意匠）は
  T18-5c/d（実機/soak・性能ハーネス基準機実行）を除き完了済み
  （[banto-hub-t18-design.md](banto-hub-t18-design.md) 参照）。
- NSIS インストーラからの `banto-hub-elev.exe` 呼び出し統合。
- banto-tagclient SDK（Issue #123）は **S4a まで実装完了**（2026-09-01。読み取り専用 DTO・
  REST/WS transport・再接続・rebinding 等。README.md 参照）。残課題は実 Hub/LAN 統合検証・
  S4 互換 tag 固定・配布サイズ確認等（S4b-1 以降）。

### 保留・オーナー決定（2026-08-12 に一部を決定）

**2026-08-12 オーナー決定:**

- **コード署名／WebView2 Fixed Version 同梱** → 当面は未署名のまま検証／社内配布のみで運用。
  SmartScreen 警告は既知の制約として許容。証明書調達は外部顧客配布が具体化した時点で再判断
  （[banto-hub-t17-design.md](banto-hub-t17-design.md) §5 に記録）。
- **仮想サービスアカウント採否** → LocalSystem を維持（実機検証済み構成を優先）。最小権限化は
  将来の堅牢化タスクとして切り出し（同 §5 に記録）。
- **ux-plan §5 UI バックログ** → **Swagger UI 同梱のみ着手**（低コスト・高価値、`GET /api/v1/openapi.json`
  を Swagger UI で提供）。スパークライン／イベント一覧／手動書き込みは引き続き保留。
- **banto-tagclient SDK**（Issue #123）→ 起票済みの決定を受けて **S4a まで実装完了**
  （2026-09-01）。残課題は実 Hub/LAN 統合検証・S4 互換 tag 固定・配布サイズ確認等。

**引き続き保留（着手時期未定）:** NSIS「インストール後に実行」チェックボックス（現状の運用回避で
許容）、UAC split-token 判定（見送り済み）、OPC UA、MQTT 組み込みブローカー、タグブリッジ・
スクリプト、ux-plan §5 の残り3件、API キースコープ配列上限、一回限り ticket プロトコル。

## 2. 運用の型

CLAUDE.md 準拠: 実装・ファイル操作は sonnet サブエージェント（worktree 分離）、監査・レビュー・
マージ判断は上位モデル（Fable/Opus 系）が担当。各フェーズは PR 単位で CI green → オーナー確認 →
マージ。実機検証（Phase 2 / P4-b）はオーナーの Windows 実機とテスト PLC が必要なため対話で進める。
