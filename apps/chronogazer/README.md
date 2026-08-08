# ChronoGazer

デジタル記録計（ペーパーレスレコーダのソフトウェア版）。PLC通信 + タグ
データ保存 + リアルタイム/ヒストリカル/ハイブリッドトレンド + 計器表示を
1台に統合する、banto-industrial 最初の製品アプリ（docs/plan.md §4）。
要件定義は
[../../docs/recorder-requirements.md](../../docs/recorder-requirements.md)。
banto テンプレート（Tauri + SvelteKit）由来。

## 現状（R1-A 段階）

**アプリ骨格のみ実装済み**で、記録計としての中核機能（収集・トレンド表示）
はまだ動かない。

- 動くもの: ログイン、設定画面（テーマ・認証・バックアップ等、banto
  テンプレート由来の機能）、LANモード（`banto-serve`）/ Tauri デスクトップ
  の両起動経路
- 動かないもの: 監視・ヒストリカル・イベントの各画面は実データ無しの
  プレースホルダ表示（`src/routes/(app)/monitor`・`historical`・`events`
  それぞれの `+page.svelte` に「R1-A: プレースホルダ」と明記）。I系クレート
  （banto-tags/banto-plc/banto-tstore/banto-collect/banto-tsquery）は
  `core/Cargo.toml` に依存としては追加済みだが、**まだ REST/Tauri
  コマンドのどこにも配線されていない**（同ファイルの依存コメント参照）。
  収集ランタイム統合は R1-C、実データでの監視画面は R1-D で入る予定

今後の実施計画（R1-B 設定CRUD → R1-C 収集ランタイム統合 → R1-D 監視画面）は
[../../docs/r1-plan.md](../../docs/r1-plan.md) を参照。

## 起動方法

デスクトップ（Tauri）:

```sh
pnpm --filter chronogazer tauri dev
```

LANモード（Tauri無し、REST + 静的配信のみのバイナリ。webkit2gtk が無い
コンテナ環境でも動く開発用サーバー。設定画面の LAN モードトグルが将来
呼び出すのと同じ経路のプレビュー用途）:

```sh
cargo run -p chronogazer-core --bin banto-serve
# 実フロントエンドを埋め込む場合は先に `pnpm --filter chronogazer build`
# を実行してから `--features embed-ui` を付けて起動する
```

`banto-serve` の既定は `PORT=8721` / `BANTO_BIND=0.0.0.0`（LAN 越しの
動作確認をしやすくするための開発用デフォルト。デスクトップ版 Tauri アプリ
の LAN モード既定は `127.0.0.1`）。`BANTO_DB`（既定
`./banto-dev.sqlite3`）・`BANTO_ALLOW_SETUP=1`（初回セットアップの許可）も
環境変数で指定できる（`core/src/bin/banto-serve.rs`）。

## 関連ドキュメント

- 要件定義: [../../docs/recorder-requirements.md](../../docs/recorder-requirements.md)
- 実施計画（R1-A〜R1-D）: [../../docs/r1-plan.md](../../docs/r1-plan.md)
- 全体計画（R系マイルストーン）: [../../docs/plan.md](../../docs/plan.md) §4
