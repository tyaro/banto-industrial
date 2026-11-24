# banto v8.0.0 への追従の計画

作成日: 2026-11-24
状態: **PR1 は本ブランチ `deps/banto-v8` で実装済み・レビュー待ち**（2026-11-25。段階 1〜4 の docs まで。PR0a は #1336、PR0c は #1337 で済み。オーナー決定と、PR1 の受け入れ条件 3 件（§5）の結果は §7）

関連: [banto v8.0.0 のリリースノート](https://github.com/tyaro/banto/releases/tag/v8.0.0)、banto の `docs/upgrading.md` 4.2 節、banto の ADR-0034（持ち主の id と組み込みの `admin`）・ADR-0035（書き込みを切断から切り離す）。

## 1. 前提

- いまは banto v7.9.0 を参照している（Cargo の 4 crate、`@banto/*` の git 依存）。v7.10.0・v7.11.0 は足すだけの変更で、壊れる所は見つかっていない。
- v8.0.0 は major。DB の基準を作り直した（`0003_users.sql`・`0005_audit_log.sql` の差し替え、`0014_ui_settings.sql` の追加）。データの移行は無い。
- Hub・ChronoGazer とも、1 つのプロファイルに SQLite が 1 つで、banto の表・アプリの表（0101 以降）・banto-tags・banto-collect が同じファイルに入っている。アプリの migration は 0101 から振るので、番号はぶつからない。
- Rust 側と npm 側は同じ版に揃える（ui-settings の wire の形が変わる）。版を上げる PR は 1 本にまとめる。

## 2. オーナー決定（2026-11-24）

1. **DB は作り直す**。上流と同じく、v8 より前の DB は起動時に分かりやすい文言で断る。migration は上流と byte 等価の写しを保つ。自前の移行は書かない。Hub のタグ・接続・収集グループ、ChronoGazer の表示グループ・ペン・自由トレンド・記録計の設定は消える（試験機の v0.3.0-alpha.4 の設定は入れ直す）。ChronoGazer の時系列のファイルは DB の外なので残すが、新版は別の場所を使い、既定では読まない（§5 の 1）。
2. **Hub のサービス（ヘッドレス）の最初の管理者は、CLI の `create-admin` で作る**（上流の `banto-serve create-admin` と同じ形）。初回セットアップの画面・`/api/auth/setup` は無くす。インストーラの案内も直す。
3. **ChronoGazer のデスクトップの localDesktop の opt-in は、v8 の「ログインなし＝組み込みの `admin`」に寄せる**。「未初期化なら」という判定は無くなるので、自前の判定を減らす。

## 3. 影響（2026-11-24 の調べ）

| 項目                                                                                      | 必要な変更                                                                                                      | 難度   |
| ----------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------- | ------ |
| migration 0003・0005 の差し替え、0014（と 0015）の追加                                    | 両アプリの `core/migrations-sqlite` の写しを直す。旧い DB の判定（`InitDbError::Legacy`）を v8 より前にも広げる | 高     |
| 初回セットアップの廃止（`setup_first_user`・`is_initialized`・`allow_setup`）             | Hub は `create-admin`、CG は local_grant の作り直し。試験は `create_user(.., Role::Admin)` へ                   | 高     |
| 組み込みの `admin`（同じ名前のアカウントは作れない）                                      | 試験・E2E で作るユーザーの名前を替える（33 ファイル）。ユーザー管理の画面で `kind = 'builtin'` の行を扱う       | 中     |
| `users` への直の INSERT に `account_uid`                                                  | 試験の INSERT に足す                                                                                            | 低     |
| 監査の主体（`record_write`・`actor_identity`・`AuditEntry`・`GrantSpec`・`LoginOutcome`） | handler を `Extension<AuditActor>` に、`AuditEntry::by` / `record_as` へ。ログインなしの actor は `admin` に    | 中〜高 |
| `Identity`・`UserIdentity` の欄の追加、持ち主のキー `acct:<accountId>`（#1260 の本体）    | 欄を足す。CG の Tauri の `auth_identity` に `accountId` を載せる。画面の試験の期待値を替える                    | 中     |
| ui-settings をサーバーの表へ、`UiSettingsProvider` を revision つきに                     | CG の Tauri の `ui_get`・`ui_set`（と delete）を写し直す。画面は `.value` で読む。デモの模擬も直す              | 中     |
| dock の保存形式を `version` 4 だけに、`onScreen` が必須                                   | 試験の fixture を直す。保存済みの配置は既定に戻る                                                               | 低     |
| `BackupService::with_migrations`・`apply_pending_restore_at_startup` の引数               | CG の目録を渡す。旧版のバックアップは戻せない                                                                   | 中     |
| 書き込みを切断から切り離す層（ADR-0035）と本文 2 MiB の上限                               | `banto_server::start` の経路は自動。Hub の TLS の経路（`axum::serve`）に掛からない差を確かめる                  | 中     |
| `libsqlite3-sys` 0.40.0（RUSTSEC-2026-0459）                                              | `Cargo.toml` の直の指定 0.37 を 0.40 に、`deny.toml` の除外を外す                                               | 低〜中 |
| routes.txt のスナップショット                                                             | `/api/auth/setup` が消え `DELETE /api/ui-settings/{key}` が増える。作り直す                                     | 低     |
| Node 26                                                                                   | ルートの package.json は上流を取り込んでいないので必須ではない。揃えるなら CI と `engines`                      | 低     |

`ReachCheckClock` の削除・`dockSlot` まわりの削除は、industrial に使う所が無い。

## 4. PR の分け方

1. **PR0a（v7.9 のまま、機械的。#1336 で済み。ユーザー名だけを `testadmin` に替え、`setup_first_user` は PR1 で替える）**: 試験・E2E で作るユーザーの名前を `admin` から替える。v7.9 でもできるなら、試験の `setup_first_user` を `create_user(.., Role::Admin)` に替える。
2. **PR0c**: `libsqlite3-sys` 0.40 と `deny.toml` の除外の削除（済み。sqlx-sqlite 0.9.1 は `>=0.30.1, <0.41.0` を受けるので v7.9 のまま出せた。Cargo.lock は libsqlite3-sys の項だけを手で書き換えた）。
3. **PR1（必須・大）**: Cargo と npm のタグを同時に v8.0.0 へ。§3 の必須の行を全部と、§5 の受け入れ条件。Hub と CG は別の worktree ではなく 1 本の worktree（`deps/banto-v8`）で順に作った（cargo のビルドが重なって PC が重くなるため）。1 つの PR に集める。上位モデルが監査する。
4. 以降は任意（回り道の置き換え）: `isOnScreen`（`dockPanelOnScreen.ts` を消す）、`onRowContextMenu`（`/tags` の `rowClass`）、系列色 16（`--bi-chart-9〜16`）、`narrowViewport`（`mobileNav*`・`viewportWatch` の写し）、CG の `run_detached` を外す、`createDockLayoutStore`・`onLayoutCommit`、`@banto/charts/testing` で描画の回帰試験、デモの `storage: 'memory'`、Node 26。

## 5. PR1 の受け入れ条件（2026-11-24 オーナーのレビュー、#1331）

1. **[P1] ChronoGazer の旧い履歴は残すが、新しい DB からは既定で読まない**。収集は設定 DB の連番から `grp:<id>`・`tag:<id>` を作り（`crates/banto-collect/src/config.rs`）、`crates/banto-tsquery/src/plan.rs` はファイルをその組で照合し、設定 DB の世代やタグ名を照合しない。DB を作り直して別の設備の新しいタグに同じ id が付くと、旧い値がそのタグの履歴になる。
   - 旧い `data.dir`（既定の場所と、設定で変えた場所の両方）を消さずに残し、新版は別の場所を使う。案内だけで済ませない。
   - id を使い回す試験で、履歴・日報・CSV に旧い値が混ざらないことを確かめる。
2. **[P2] 旧い形式の拒否と、予約済みのリストアの順序**。ChronoGazer の起動は「排他 → 予約の適用 → `init_db`」（`apps/chronogazer/core/src/engine.rs`）。旧い形式の検知を `init_db` に足すだけでは、リストアが先に通る経路を取りこぼす。
   - v7 の実プロファイルと、v7 の予約があるプロファイルで、新版が旧い DB・履歴に書かずに安全に止まることを試験にする。
   - 新版のバックアップは正しく戻せることを試験にする。
3. **[P2] 新しい DB の認証の初期値を、サービスの起動まで通して確かめる**。上流の `0015_auth_mode_defaults.sql` は、設定の行もアカウントも無い DB に `auth.disabled=true` を入れる。
   - Hub のサービスでは、`create-admin` の前の遠隔のアクセスを管理者として通さない。CLI はサービスと同じ DB・権限を使う。作った後は期待どおりログイン必須になる。これを PR1 の試験にする。
   - ChronoGazer の localDesktop は、今の peer と Host のループバックの条件を残し、LAN 側へ管理者の権限を広げないことを確かめる。

## 6. PR1 でほかに確かめること

- 認証モードの読み取りが `auth.disabled` の行を必須にするか（0015 を写すかは §5 の 3 と合わせて決める）。
- Hub の試運転の grant・`DisabledNoRevocation`（`commissioning.rs`・`service_elevated.rs`）が本番の経路で ui-settings を読み書きできるか。
- 2 MiB を超える本文を受ける口が無いか（Hub の ingest は 1 MiB の上限、CG のバックアップのリストア）。
- #1260 は上流（C3）で片付いた。`acct:` の形に追従した時点で閉じるかはオーナー。

## 7. PR1 で決めたこと・確かめたこと（2026-11-25）

### オーナー決定（2026-11-25）

- ChronoGazer は未リリースなので、旧い DB からの移行は作らない。CG の migration の記録の表を既定の `_sqlx_migrations` に戻し、banto v8 の復元の検査（表の名前が固定）を通す。旧い表名 `_sqlx_migrations_chronogazer` の DB は v8 より前として断る。
- Hub の `create-admin` は、管理者の作成・ログイン必須（`auth.disabled=false`）・試運転の口のロックダウン（`commissioning.locked_down=true`）を 1 度に行う。

### §5 の結果

1. **旧い履歴の分離**: 保存先に世代 id（設定 `data.store_generation`）を持たせ、保存先に印 `.chronogazer-store-generation` を置く。既定の保存先は `./data/history`（旧い `data.dir` は消さず残し、新版は使わない）。印の無い中身のある保存先・別の世代の保存先は、何も書かずに止まる（起動時の窓 `StoreDirRefused`）。DB を作り直すと世代が替わるので保存先を選び直す。別のインストールの DB のバックアップを戻したときも、保存先で止まる。印は「無ければ作る（`create_new`）、あれば読み直して比べる」で置き、空・書きかけ（改行で終わらない）の印は自動では書き直さずに止まる（オーナーのレビュー P2、2026-11-25。別々の DB のプロセスは profile の排他を共有しないので、書き直すと 2 つが同じ保存先で収集を始めうる。ほかに ChronoGazer が使っていないことを確かめてから印のファイルを消すか、空の保存先を選ぶ）。試験: `a_leftover_empty_marker_is_never_claimed`（空の印に 2 つの世代が同時に当たってもどちらも通さない）、`a_recreated_db_does_not_mix_the_previous_history_into_reused_ids`（id を使い回しても旧い値が混ざらない）、`a_pre_v8_store_is_refused_and_the_new_default_leaves_it_alone`、`choosing_another_generations_store_in_the_recovery_window_writes_nothing`、`a_new_folder_is_claimed_and_another_generation_is_refused`、`an_unmarked_folder_with_content_is_refused_untouched`。
2. **旧い形式の拒否とリストアの順序**: 予約の適用より前に、今の DB と予約のファイルを読み取り専用（immutable）で確かめ、旧形式なら予約を当てず・消さずに止まる（`refuse_legacy_profile`）。試験: `a_banto_v7_database_is_refused_without_being_touched`、`a_backup_that_could_not_start_is_refused_at_staging`、新版のバックアップの往復 `a_backup_taken_by_this_version_is_restored_on_the_next_start`（ignore にしていた復元の試験 7 本も戻した）。`immutable` は `-wal` を読まない（WAL を読むには `-shm` を作る・書くことがある）ので、予約があるときは今の DB を「無い／v8／旧形式／確かめられない」に分け、確かめられない（開けない・読めない・空でない `-wal` か `-journal` が残っている）なら、DB・`-wal`・予約を変えずに止まる（`InitDbError::Undetermined`、起動時の窓は終了だけ。オーナーのレビュー P2、2026-11-25）。予約のファイルも、空でない `-wal`・`-journal` があれば同じく止まる（SQLite のファイルでない予約は今までどおり banto の検査が当てずに消す）。予約が無いときは今までどおり後の `init_db` が判定する。試験: `a_reservation_over_a_db_with_a_leftover_wal_is_not_applied`、`a_reservation_over_an_unreadable_db_is_not_applied`。
3. **新しい DB の認証の初期値**: 新しい DB の Hub は試運転モードで、LAN の bind を断り loopback だけで起動する。上流の 0015 の `auth.disabled=true` は Hub の認証には読まない（AuthState は既定のログイン必須のまま）。`create-admin` と起動は同じ root の解き方（`profile_paths::resolve_hub_root_from_env`）。試験は `apps/banto-hub/host/tests/first_admin_service.rs`（作る前は資格情報なしで 401、試運転の grant は loopback だけ、動いている間の `create-admin` は何も書かない、作った後はログイン必須で grant は閉じる）。ChronoGazer の localDesktop は、LAN にログインなしでも管理者の権限を広げない（`no_login_gives_a_lan_peer_no_admin_rights`）。

- **Hub の時系列ファイルは世代の対象外**（独立レビュー P3、2026-11-25）: Hub の記録（`profiles/<id>/data`）は ChronoGazer の `data.store_generation` のような印を持たず、DB を作り直すと同じ鍵 `grp:<id>`・`tag:<id>` で追記が続く。今は Hub に履歴を読む口が無いので画面には混ざらない。運用の手順（DB を作り直すときはデータディレクトリも退ける）は `docs/hub/banto-hub-operations.md` §1 に書いた。**#1306（ヒストリアン）で Hub の記録を読む前に、ChronoGazer と同じ世代の印を入れる。**
- **ChronoGazer のサービスの最初の管理者**（独立レビュー P1、2026-11-25）: 新しい DB はログインなしで始まり、サービスにはログイン必須へ切り替える画面・REST の口が無い（`PUT /api/settings/auth` はログインなしの間 400）ので、loopback の peer と Host なら誰でも admin の `localDesktop` を受け取れたままになっていた。Hub と同じ形の `chronogazer-server create-admin --username <名前> [--display-name <表示名>]` を足した（`apps/chronogazer/core/src/first_admin.rs`。起動と同じ A1 の排他・同じ DB の開き方、予約されたリストアがあれば止める、管理者の作成と `auth.disabled=false` と監査を 1 つのトランザクション。パスワードの入力は Hub の `password_input.rs` を共有）。開発用の `banto-serve create-admin` も同じ関数に寄せた（前は `create_user` の後に別の文で `auth.disabled=false`）。作った後の起動では `localDesktop` は設定 `auth.local_grant`（既定 off）どおり。試験: `first_admin::tests::the_local_grant_follows_the_setting_after_create_admin`、`tests/chronogazer_server.rs` の `create_admin_closes_the_no_login_grant_of_the_service`。

### §6 の結果

- `auth.disabled` の行は上流の認証モードの読み取りで必須。0015 を両アプリの migrations-sqlite に写した（上流と byte 等価）。
- 試運転の grant は、本番の経路で ui-settings を読み書きできる（`first_admin_service.rs`）。
- 2 MiB を超える本文を受ける口は無い。
- ADR-0035 の書き込みの切り離しは、Hub の TLS の経路（loopback の 8723）には掛からない。上流が `detach_writes` を公開していないため。banto に提案する（提案前。Issue ができたら番号をここに足す）。

### E2E

- 起動の前に `e2e/serve-with-first-admin.mjs` で `create-admin` を行う（上流と同じ形）。
- Hub の E2E では、試運転の grant の経路と `lockdown-by-account` の 1 本が通らなくなったので skip にした（単体試験には残っている）。
