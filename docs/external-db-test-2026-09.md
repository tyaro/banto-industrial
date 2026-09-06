# 外部 DB 連携 検証手順（S3 手動 smoke / S7 実 DB 検証）

作成日: 2026-09-07
状態: **手順のみ（未実施）**。S3 の完了条件「手動 smoke 手順を docs に追加」と S7（実 DB 検証、オーナー同席）の手順書。結果は §5 に記録する。
対象: [banto-hub-external-db-design.md](banto-hub-external-db-design.md) の DB Source（#228）と DB Sink（#229）。設計の決定事項は同書 §6、実装の所在は §7。

関連: [real-machine-test-2026-09.md](real-machine-test-2026-09.md)（実機検証の記録の書き方の先例）、[banto-hub-operations.md](banto-hub-operations.md)（Hub の起動・サービス化）。

---

## 1. 前提と機材

| 項目        | 内容                                                                                                                                            |
| ----------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| PostgreSQL  | 15 以上。ローカル smoke は `127.0.0.1:5432`（テスト用、`postgres/postgres`）。S7 は **別マシン（LAN 越し）**の PostgreSQL                       |
| DB ユーザー | Source 用: `SELECT` のみの読み取り専用ユーザー。Sink 用: 保存先テーブルへの `INSERT` のみ。**Hub とサイドカーは DDL を発行しない**（設計 §5.3） |
| Hub         | `v0.2.0-alpha.1` 以降の main（S0〜S6 込み）。収集は Running にできる状態（PLC 接続は無くてもよい）                                              |
| サイドカー  | `banto-hub-sink.exe`（`cargo build --release -p banto-hub-sink`）と exe 隣の `banto-hub-sink.toml`                                              |
| API キー    | サイドカー用に **`admin` + `read`** の両スコープ（設計 §5.2 訂正、2026-09-06）。API キー画面のプリセット（sink 画面のリンク）で発行できる       |
| 注意        | Windows のプロファイルロックはマシン全体（`Global\BantoHub.<profile>`）。評価用 Hub が動いているマシンで別 Hub を起動しない                     |

Source 側のサンプル（読み取り専用ユーザーで参照できるビュー）:

```sql
CREATE TABLE machine_status (machine_id int PRIMARY KEY, temperature float8, pressure numeric(8,2), running bool, updated_at timestamptz);
INSERT INTO machine_status VALUES (1, 41.5, 101.30, true, now());
```

Sink 側のサンプル（サイドカーが推奨 DDL として表示するものと同じ）:

```sql
CREATE TABLE tag_history (ts timestamptz NOT NULL, tag_id bigint NOT NULL, external_name text NOT NULL, value double precision, quality text NOT NULL);
```

## 2. DB Source の smoke（S3 完了条件）

| #    | 手順                                                                                                                                    | 期待                                                                                               |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------- |
| A-1  | 接続を作成: protocol `PostgreSQL（DB Source）`、host / port / database / username / password                                            | 一覧に DB バッジ付きで表示。応答に `passwordSet: true`、パスワードは表示されない                   |
| A-2  | Drawer の「接続テスト」                                                                                                                 | `PostgreSQL 15.x ...` のサーバー版数                                                               |
| A-3  | 接続配下にグループを作成: 周期 1000ms、SQL `SELECT temperature, pressure, running, updated_at FROM machine_status WHERE machine_id = 1` | ツリーに `SQL` バッジ                                                                              |
| A-4  | タグ登録 Drawer で「列を取得」                                                                                                          | 4 列が候補に出る（すべて `supported`）。`text` 列があれば未対応として灰色                          |
| A-5  | `db` タグを 4 つ登録（address = 列名、data_type は数値 / bool / 数値）                                                                  | `writable` は表示されず、値の出所ラベルは `db`                                                     |
| A-6  | 収集を開始                                                                                                                              | 1 周期以内に 4 タグが Good。状態画面の `dbSource` 節が `connected`                                 |
| A-7  | DB 側で `UPDATE machine_status SET temperature = 42.0`                                                                                  | 次の周期で値が追従                                                                                 |
| A-8  | DB 側で `UPDATE ... SET pressure = NULL`                                                                                                | pressure だけ Bad、他は Good                                                                       |
| A-9  | DB 側で `DELETE FROM machine_status`（0 行）                                                                                            | 4 タグとも Bad（グループ単位）。`INSERT` で復帰                                                    |
| A-10 | PostgreSQL を停止                                                                                                                       | 4 タグ Bad、`dbSource` は `backoff`、ログは初回と段階変化時のみ。Hub の他機能は継続                |
| A-11 | PostgreSQL を再起動                                                                                                                     | 自動で `connected` に戻り Good（操作不要）                                                         |
| A-12 | 収集を停止                                                                                                                              | 4 タグ Bad、`dbSource` は `stopped`、`pg_stat_activity` に `banto-hub-db-source` の接続が無い      |
| A-13 | 稼働中にタグを 1 つ追加（pending queue → apply）                                                                                        | 追加タグだけが値を取り始め、他タグは途切れない                                                     |
| A-14 | 誤った SQL（存在しないテーブル）のグループを追加                                                                                        | そのグループのタグは Bad で `lastError` に理由。テーブルを作ると **設定変更なしで 5 秒以内に復帰** |

## 3. DB Sink の smoke と S7

| #    | 手順                                                                                                                                                                                    | 期待                                                                                                                      |
| ---- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| B-1  | sink 画面で sink group を作成: 接続（A-1 と同じ）、mode `interval` 1000ms、table `tag_history`、タグに A-5 の 4 つ                                                                      | 推奨 DDL が表示される。即時適用（pending queue に載らない）                                                               |
| B-2  | API キー画面（sink 画面のリンク）で `admin` + `read` のキーを発行                                                                                                                       | 平文キーは発行時のみ表示                                                                                                  |
| B-3  | `banto-hub-sink.toml` に `hub_url` と `api_key` を書き、`banto-hub-sink.exe run` をコンソールで起動                                                                                     | 起動ログにパスワード・キーが出ない。テーブルが無ければ推奨 DDL を warn で 1 回表示                                        |
| B-4  | `SELECT count(*), max(ts) FROM tag_history`                                                                                                                                             | 1 秒ごとに 4 行ずつ増える。`quality = 'good'`、`external_name` が `接続.グループ.タグ`                                    |
| B-5  | 状態画面の「DB Sink」節                                                                                                                                                                 | サイドカー `online`、group `running`、`queued` ≈ 0                                                                        |
| B-6  | mode を `on_change` に変更（sink 画面）                                                                                                                                                 | 30 秒以内に反映。値が静止していれば行が増えない。A-7 の UPDATE で 1 行だけ増える                                          |
| B-7  | 保存先テーブルを `DROP`                                                                                                                                                                 | group `error` + `lastError`、キューに溜まる（`queued` 増加）。`CREATE` し直すと自動復帰し溜めた行が書かれる               |
| B-8  | PostgreSQL を停止して 2 分、再起動                                                                                                                                                      | `backoff` → 復帰。`dropped` は `queue_max_rows`（既定 10,000）を超えなければ 0                                            |
| B-9  | Hub を再起動                                                                                                                                                                            | サイドカーは SDK のバックオフで待ち、Hub 復帰後に購読を再開。プロセスは落ちない                                           |
| B-10 | サイドカーを Ctrl+C で停止                                                                                                                                                              | 最大 5 秒で flush して終了。状態画面は 15 秒後に `unknown`                                                                |
| B-11 | 管理者で `banto-hub-sink.exe install` → `banto-hub-sink.exe grant-service-acl`（同じディレクトリの `banto-hub-elev.exe` を呼ぶ。MSI では install の後に実行）→ `sc sdshow BantoHubSink` | サービス登録。`BantoHub Operators` の ACE（`CCLCRPWP`）が Hub と同じく付く（#316）。elev が無い場合は明確なエラーで止まる |
| B-12 | デスクトップシェルの「サービス一覧」                                                                                                                                                    | `BantoHub` と `BantoHubSink` の状態が出る。Operator ユーザーで start / stop できる                                        |
| B-13 | **S7 本番**: 別マシンの PostgreSQL に対して Source と Sink を **24 時間**連続                                                                                                           | 途切れ・`dropped`・再起動回数（`restarts`）を状態画面と DB 側の行数で確認。切断・再接続・DB 再起動を各 1 回以上含める     |

## 4. 記録すべきもの

- Hub と サイドカーの版（コミット）、PostgreSQL の版、ネットワーク構成（同一機 / LAN）
- 各項目の合否と、期待と違った挙動（機材の癖と製品の問題を分けて書く。先例: real-machine-test-2026-09.md §「機材の癖」）
- 24 時間の間の `dropped` / `restarts` / `lastError` の推移と、DB 側の行数・欠落
- サイドカーのログのうち warn 以上

## 5. 結果

未実施。
