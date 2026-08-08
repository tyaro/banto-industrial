# H10 ③ 提案: API キー read スコープのタグ単位化(catalog の扱い)

状態: **完了(2026-08-08、案 B、PR #75 マージ済み)** — catalog は全タグ(PLC
アドレス含む)を出し続け、per-tag read スコープは**値の読み取り**のみ絞る
最終検証日(コード照合): 2026-08-08
関連: [docs/improvement-plan.md](improvement-plan.md) H10、
[docs/tag-server-design.md](tag-server-design.md) §5.6/§9、PR #74(H10 ①②)

## 0. これは何か

H10 ③(read スコープのタグ単位化)の実装前に、オーナー決定が必要な論点を
整理した比較案。改善計画 H10 の決定(2026-08-08)で「read スコープのタグ
単位化: catalog API の扱い(スコープ外タグを一覧から隠すか、一覧には出すが
値を読めなくするか)の比較案を実装前にオーナーへ提示してから着手する」と
されたことに基づく。**2026-08-08 に案 B が決定した(§4)。以後この線で実装
(サブエージェント → 上位レビュー → PR #75)へ進む。**

## 1. 現状(コード照合済み)

- read 認可は**単一の真偽値** `ApiKeyContext::has_read_scope()`(スコープ配列に
  `read` を含むか)を認証層で 1 回だけ判定する(`rest.rs:3498`、`grpc.rs:352`)。
  read のハンドラ自身はスコープを一切見ない
- スコープ文法(`api_keys.rs:118` `validate_scope`)は 2 形式のみ:
  - `read` — 全タグ一括(タグ非依存の真偽値)
  - `write:{conn}.{group}.{tag}` — タグ単位・**完全一致・ワイルドカード無し**
  - `read:{...}` 形式は存在しない
- API キーで読める全経路は、いずれも**全タグ**を返す。スコープでの絞り込みは
  無く、クライアント指定の `connection`/`group`/`tags` でしか絞れない:
  - REST: `GET /api/v1/tags`(catalog。`TagEntry` に external_name・**PLC
    アドレス**・data_type・writable・expression・安定 id を含む)、
    `GET /api/v1/values`(全現在値)、`/api/v1/values/{tag}`(単一)、
    `GET /api/v1/stream`(WebSocket。全 `TagMap` に対して購読解決)
  - gRPC: `GetCatalog` / `ReadValues` / `StreamValues`
  - read ハンドラは `ApiKeyContext` を受け取らない/受け取っても捨てている
    (REST は extensions に在るが `v1_write_value` だけが取り出す。gRPC は
    computes して破棄)
- catalog は設計上 PLC アドレスを既定で含む(tag-server-design.md §9、
  「専用スコープ `catalog:full` は設けない」の既存決定)
- external_name = `{conn}.{group}.{tag}`(`hub.rs:432`、`write:` と同一の綴り)。
  unique だが**リネームで変わる**(安定 id は `(conn_id, group_id, tag_id)` の
  三つ組)
- スコープは `api_keys.scopes`(JSON 配列 TEXT、`db.rs:153`)に保存、**長さ
  上限なし**
- 既存テスト: write 専用キーは `/api/v1/tags`(`rest.rs:4360`)・`/api/v1/stream`
  (`stream.rs:695`)で 403 になる(read ゲートの前例が既にある)

## 2. 決めること(決定済み)

- **主論点**: スコープ外タグを catalog でどう扱うか
  - 案 A(隠す): スコープ外タグは一覧に出さない
  - **案 B(出すが値は読ませない)**: 一覧には全タグを出し、スコープ外の値読みを拒否
    ← **採用(§4)**
  - 案 C(出すがアドレス等を redact): 中間案
- **サブ決定**: S1 文法、S2 後方互換、S3 単一読みの応答、S4 ストリーム/バルク(§5)

## 3. 案 A vs 案 B(比較・判断材料)

| 観点                           | 案 A: 隠す                                                       | 案 B: 出すが値は読ませない                             |
| ------------------------------ | ---------------------------------------------------------------- | ------------------------------------------------------ |
| 情報開示                       | スコープ内タグのみ可視。PLC アドレス・タグ名・構造の漏洩を最小化 | 全タグ定義(PLC アドレス含む)を全 read キーに開示       |
| 最小権限                       | ◎ need-to-know を満たす                                          | △ 値は守るが「存在・アドレス」は守らない               |
| 発見可能性(タグ選択・割付確認) | ✗ スコープ付きキーは全体像を見られない                           | ◎ 全タグ・全アドレスを発見でき、割り付けミスに気づける |
| 一貫性                         | catalog = 読める集合                                             | catalog は「存在」、スコープは「値アクセス」の別軸     |
| 実装範囲                       | catalog + 値 + stream + gRPC を全て絞る                          | 値読み経路のみ絞る、catalog は不変(小さい)             |

## 4. 決定: 案 B(catalog は全タグ+アドレスを出し、値のみ絞る)(2026-08-08 オーナー決定)

**オーナー決定(2026-08-08)**: 案 B を採用。catalog(`GET /api/v1/tags` /
gRPC `GetCatalog`)は**全タグを PLC アドレス込みで**返し続ける。per-tag read
スコープは**値の読み取り**(単一・バルク・ストリーム)だけを絞る。

理由(オーナー): **「PLC アドレスも見えた方が割り付けミスに気づきやすい」**。
catalog は SCADA/HMI 開発でのタグ選択・アドレス突き合わせに使う「発見」の面で
あり、その用途では全タグ・全アドレスが見える方が実務価値が高い。既存の設計方針
(tag-server-design.md §9「catalog は PLC アドレスを既定で含む」「専用スコープ
`catalog:full` は設けない」)とも整合する。

帰結:

- catalog は従来どおり(全タグ・全アドレス、read 系スコープを持つ任意のキーへ開示)
- 制限するのは**値の読み取り経路のみ** — スコープ外タグの値は読めない
- 「一覧に出るのに値は 403」は、この用途では意図した挙動(発見 ≠ 値アクセス)
- 情報開示のトレードオフ(タグ在庫・アドレスが read キーに開示される)はオーナー受容

## 5. サブ決定(案 B 前提)

- **S1 文法**: `read:{conn}.{group}.{tag}`(完全一致、`write:` と対称)を追加。
  加えて **`read:{conn}.{group}.*` のグループ・ワイルドカードを read に限り許可**
  する(採用。異議あれば完全一致のみに変更可)。理由: read は一括操作で
  WebSocket の GroupWildcard(`{conn}.{group}.*`)と整合し、スコープ配列の肥大も
  防ぐ。write は誤書き込みの被害が大きいのでワイルドカード無しのまま(非対称)
- **S2 後方互換**(互換要件): 素の `read` = 全タグの値も catalog も従来どおり。
  既存キーは無影響
- **S3 単一値読み**(スコープ外タグ `GET /api/v1/values/{tag}`): **403**。案 B では
  タグは catalog に見えている=存在は既知なので、値だけ拒否する 403 が自然
  (404 は「見えるのに無い」で不整合)
- **S4 ストリーム/バルク**: `GET /api/v1/values`(bulk、`?tags=` 無し)はスコープ内
  のみ返す。`?tags=` でスコープ外を明示指定したら **403**(単一と同じ)。
  WebSocket / gRPC ストリームは購読解決結果をスコープで交差(∩)。gRPC
  `ReadValues` も同様

## 6. 実装スケッチ(案 B)

- スコープ文法に `read:{conn}.{group}.{tag}`(+ S1 の `read:{conn}.{group}.*`)を
  `validate_scope` へ追加。`ApiKeyContext` に:
  - `has_any_read()` = 素の `read` **or** 任意の `read:*` を持つ(認証層の
    「read 経路に入れるか」の 403 判定用。write 専用キーは従来どおり 403)
  - `can_read_value(external_name)` = 素の `read` **or** 該当する
    `read:{name}`/ワイルドカード一致(値経路の per-tag 判定用)
- **catalog は絞らない**: `v1_tags`/`GetCatalog` は `has_any_read()` だけを要求し、
  従来どおり全タグ(アドレス込み)を返す
- **値経路のみ絞る**: `v1_value_single`(スコープ外 → 403)、`v1_values`(bulk、
  スコープ内のみ返す。`?tags=` でスコープ外指定は 403)、`v1_stream`/`StreamValues`
  (`subscribe_core` の resolve をスコープで交差)、gRPC `ReadValues`。ctx を各値
  ハンドラへ通す(REST は extensions に既在、gRPC は既に computes)
- session 認証の read(ctx 無し)は全アクセス維持(admin UI 不変)
- external_name はリネームで変わる → スコープから外れて **fail-closed**(安全側で
  値が読めなくなる)。仕様として記録。安定 id ベースは複雑なので今回は採らない
- テスト: `read:{tag}` キーで catalog は全タグ見える / in-scope 値は読める /
  out-of-scope 値は 403 / stream は購読解決で除外、素の `read` は全取得(不変)、
  write 専用キーの既存 403 テストも不変。gRPC 同様

## 7. 付随(この実装では扱わない)

- **スコープ配列長の上限**: 現状無し。per-tag read で長くなりうるが、S1 の
  ワイルドカード採用で緩和される。明示上限を設けるかは別途
- **案 C(アドレス redact)**: オーナー決定で不採用(アドレスは見せる方針)

## 8. 決定記録(オーナー、2026-08-08)

- 主論点: **B**(catalog は全タグ+PLC アドレスを出し、値のみ絞る)。
  理由: アドレスが見えると割り付けミスに気づきやすい
- S1 文法: `read:{tag}` + read に限り `read:{conn}.{group}.*` ワイルドカードも許可
  (採用、異議あれば完全一致のみへ)
- S3 単一/バルクのスコープ外値読み: **403**(B ではタグは可視=存在既知)
- 情報開示(タグ在庫・アドレスの read キーへの開示)はオーナー受容済み
