# 作業の引き継ぎ（STATUS）

最終更新: 2026-10-21 18:20 UTC（R2b-1d の A を実装中）

`/clear` の直前に更新し、`/clear` の直後はこれを読んで続きから始める（2026-10-21 オーナー指示）。
CLAUDE.md から読み込まれる。**事実は git・GitHub・docs が正で、ここは「次に何をするか」の索引**。
古くなった行は消す（履歴は git に残る）。

## いまの状態

- main: `ae3e58f9`（R2b-1c まで）。banto-hub の版は v0.2.0-alpha.33（R2a 完了時）。
- 進行中: **R2b-1d**（[connection-settings-design](banto-hub-connection-settings-design.md) §10。戻せない DB の切り替え）
  - worktree `/home/user/bi-r2b1d`、ブランチ `wip/r2b1d`（origin/main `ae3e58f9` から）。
  - A（migration `0025`・`banto-tags` の型と保存の層・平文の拒否・秘密の行の寿命・テストの生 SQL・ChronoGazer の機械的な追従）を上位モデルのサブエージェントが実装中。終わるとローカルに 1 コミットして報告が来る。
  - 報告が来たら: 差分を監査（§6.2 の写し方・`sqlite_sequence`・トリガ 6 本・平文/空に潰さない・トランザクションの外の読み書き）→ B を委譲。
  - B: Hub の起動時の手順（§6.1〜§6.4: バックアップ `.partial`・写しでの予行と preflight・`vacuum_pending` の印）、`with_registry` を Hub の全入口へ、読む側（§5.4: sink / bridge config の `unavailableConnections`・`unavailableSources`）、§4.4 の除外の理由、`upgrade.rs`、§10 の R2b-1d の申し送り 2 つ（`flat_view` の一致の保証・sink テストの `HubDrivers` の追従）。
  - A と B を 1 つの PR（コミットは分ける）で `claude/lucid-planck-aed924` に push → draft PR → 購読。
- その後: R2b-1e（DPAPI 封印・後処理）→ R2b-1f（docs・CHANGELOG・版の 4 点セット。**#768（MCP の push/settings）と #769（statement_timeout 30 秒・PgBouncer の注意）も CHANGELOG に載せる**）。

## R2b-1 の済んだ PR

| PR   | 内容                                                                    |
| ---- | ----------------------------------------------------------------------- |
| #767 | 詳細設計（Q1〜Q15 推奨どおり、レビュー反映済み）                        |
| #771 | R2b-1a `DriverKind::HostProvided`・push/postgres/virtual の登録         |
| #772 | R2b-1b `secrets` モジュール（`ConnectionSecrets`、DPAPI / plain-v1）    |
| #774 | R2b-1c `settings_json`・`with_registry`・旧い検証との一致の表           |
| #768 | MCP の接続ツールに push・settings（別セッション作、衝突を解いてマージ） |
| #769 | PostgreSQL のサーバー側 `statement_timeout`（引き継いでマージ）         |
| #770 | ingest のテストのフレーク（送り手のタイムアウト）                       |

## 決めたこと（この間の判断。詳細は各 docs）

- §4.2: 旧い検証 (1) が断ったら (2)〜(4) は走らせない（R2b-1c の食い違い (a)〜(e) を画面に出さない）。
- §5.4: 読めない接続は sink / bridge の設定に載せず `unavailable*` に出す（古いサイドカー + `trust` 認証の対策）。
- §6.5: VACUUM の印は「清掃待ち」`secrets.vacuum_pending`（書き換えと同じトランザクションで立て、成功後に消す）。
- 版上げは R2b-1 の最後（R2b-1f）でまとめて（Q13）。機能 PR では上げない。

## 残っていること（R2b-1 の外）

- 実機の確かめ B-16〜B-28（オーナー同席）。
- chronogazer の `tag_commands_refuse_an_address_the_connection_cannot_read` が 1 回だけ 600 秒 TIMEOUT（テストは ok、プロセスが終わらない）。再発したら junit と一緒に調べる。
- リモートのブランチ `claude/ingest-timeout-flake`・`claude/ingest-reapply-flake` は 403 で消せない（オーナーが GitHub で消す）。

## 進め方（毎回同じ）

- オーナーの常設指示: 「CI 通ってレビューで直す指摘が無ければマージして次へ」「マージ後の後片付けは自動で可」。
- 後片付け: PR の購読を外す → 確認予定（`send_later`）を消す → worktree とブランチを消す → `claude/lucid-planck-aed924` を origin/main に `reset --hard` して `push --force-with-lease`。
- 実装は CLAUDE.md の役割分担（sonnet / 上位）でサブエージェントへ。worktree ごとに `CARGO_TARGET_DIR` を分け、`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0`（ディスクは 6 GB 程度）。
- prettier（worktree には node_modules が無い）: main の checkout の `node_modules/.pnpm/prettier@*/node_modules/prettier/bin/prettier.cjs` を、リポジトリの `.prettierrc.json` の設定で実行する。
- push 直後の古い SHA の ci-gate の failure は取り消し（cancelled）。ジョブのログで確かめる。
- PR を ready にすると Codex のレビューが走る。オーナーのレビューは PR のコメントで来る。
