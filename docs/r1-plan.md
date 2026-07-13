# R1 実施計画 — ChronoGazer アプリ骨格 + 設定 + 監視画面

作成日: 2026-07-13（I0〜I4 完了時点で司令塔が事前設計。新セッションへの引き継ぎ文書を兼ねる）
前提: [recorder-requirements.md](recorder-requirements.md)（R0、未決事項ゼロ）と [plan.md](plan.md) §4。
実施プロセスは banto と同じ（司令塔が設計・分割 → 実装は general-purpose(sonnet)、
難所は opus に委譲 → 検証 → Phase 毎コミット → PR + CI → ユーザーマージ）。

## 消費するバージョン

- banto は **git タグ `v0.1.1`**（MIT 統一後の main。`v0.1.0` は MIT 前なので使わない）
  - npm: `pnpm add "github:tyaro/banto#v0.1.1&path:packages/admin-core"` 等
  - Rust: `{ git = "https://github.com/tyaro/banto.git", tag = "v0.1.1" }`
- I 系クレート（banto-tags/plc/tstore/collect/tsquery）は同一ワークスペースの path 依存

## Phase 分割

### R1-A: アプリ骨格（テンプレートコピーの実地検証を兼ねる）

- banto-industrial に pnpm ワークスペースを新設（現状 cargo のみ。
  pnpm-workspace.yaml + ルート package.json。banto の lint/format 構成も
  この機に持ち込む — prettier/eslint 設定は banto からコピー）
- banto の README「テンプレートから自分のアプリを作る」手順どおりに
  `apps/admin-template` → `apps/chronogazer` をコピー・リネーム
  - **既知の追加作業**（手順は同一リポジトリ内コピーを想定しているため）:
    `@banto/*` の `workspace:*` 参照 → git 依存（`#v0.1.1&path:`）への
    書き換え、Rust の path 依存 → git tag 依存への書き換えが必要
  - デモリソース（items 一式・ダッシュボードパネル）は手順どおり削除。
    ナビは 監視/ヒストリカル(R2 プレースホルダ)/イベント/設定系 に置換
- Rust 側: `apps/chronogazer/core`（admin-template-core 由来。items を除去し
  banto-tags のマイグレーション適用 + I 系クレート依存を追加）
- 完了条件: デスクトップ（tauri dev）と LAN（banto-serve 相当）の両モードで
  ログイン → 空の監視ページ表示まで動く。CI に frontend/E2E ジョブ追加。
  **README 手順の穴をリスト化**（→ banto へのフィードバック PR を別途作成）

### R1-B: 設定画面（レジストリ CRUD + 表示グループ）

- banto-tags の PlcConnection / CollectionGroup / Tag CRUD を REST + Tauri
  両経路で公開（banto の users/audit ルーターの流儀。editor 以上、監査記録）
- **表示グループは新エンティティ**（chronogazer 固有、収集グループとは別物 —
  R0 §2: 表示の単位・最大8ペン・表示種別 トレンド/デジタル/バー/計器）。
  app DB にテーブル + CRUD + 画面
- 画面: PLC接続 / 収集グループ / タグ（一覧グリッド + フォーム、
  BantoGrid/BantoForm）/ 表示グループ（ペン割当 UI）
- 完了条件: 全 CRUD が両経路で動き、検証エラーが人間可読で出る。
  viewer は閲覧のみ・変更 403

### R1-C: 収集ランタイム統合

- Collector のライフサイクル管理: 起動時に build_config → start、
  設定変更後の「収集を再起動」操作（editor 以上、監査記録、
  tstore は構成ハッシュ変化で自動ローテーション）
- 接続状態/ヘルス表示（status()）、collect_events のイベント一覧ページ
  （banto 監査ログページの流儀）、現在値 API（CurrentValuesHandle →
  REST ポーリング + Tauri。SSE 化は R1-D の必要に応じて）
- 完了条件: シミュレータ（banto-plc simulator を dev 用 PLC として起動する
  dev コマンド/フィーチャを用意）相手に、設定 → 収集開始 → データファイル
  生成 → イベント記録まで一巡

### R1-D: 監視画面（グループ表示4種 + リアルタイムトレンド）

- 表示種別: トレンド（M13 LineChart ストリーミング、既定窓10分・選択可、
  しきい値は bands）/ デジタル（数値大表示 + 品質色分け）/ バー（縦バー +
  しきい値色）/ 計器（charts Gauge）。デジタル/バーは chronogazer 内の
  軽量コンポーネント（テンプレートに入れない — 4条件を満たさない）
- 現在値ポーリング（グループ周期に同期、Stale/Bad の視覚化）+
  トレンド初期窓は I4 read_decimated → 以後 append
- グループ切替（タブ + コマンドパレット）、キオスク（認証無効モード）確認
- 完了条件: シミュレータ相手に 4 種すべてが実データで動き、
  断線 → Bad 表示 → 復旧が目視確認できる。ブラウザ実機検証必須

## 検証方針

- 各 Phase で `pnpm check` / `pnpm lint` / `cargo test --workspace` +
  ブラウザ実機（preview ツール、LAN モードは実バックエンド）
- R1 完了時にミニソーク: シミュレータ相手に収集 30 分 + UI 閲覧で
  行欠落・メモリ増加がないこと（72h ソークは R4）

## 新セッションの開始手順（引き継ぎ）

1. 永続メモリ（banto-industrial-repo / model-delegation-rules /
   pr-merge-workflow）が読み込まれていることを前提に、本ファイルと
   recorder-requirements.md を読む
2. R1-A から順に、Phase 毎に feature ブランチ → 実装委譲 → 検証 →
   PR + CI → ユーザーのマージ承認、で進める
3. README コピー手順の穴が見つかったら banto へのフィードバック PR を
   忘れずに（R1-A の完了条件）
