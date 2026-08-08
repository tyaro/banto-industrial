# H10 ③ 提案: API キー read スコープのタグ単位化(catalog の扱い)

状態: **オーナー判断待ち(2026-08-08)** — 下記の主論点(案 A/B)とサブ決定を
決めてから実装に着手する
最終検証日(コード照合): 2026-08-08
関連: [docs/improvement-plan.md](improvement-plan.md) H10、
[docs/tag-server-design.md](tag-server-design.md) §5.6/§9、PR #74(H10 ①②)

## 0. これは何か

H10 ③(read スコープのタグ単位化)の実装前に、オーナー決定が必要な論点を
整理した比較案。改善計画 H10 の決定(2026-08-08)で「read スコープのタグ
単位化: catalog API の扱い(スコープ外タグを一覧から隠すか、一覧には出すが
値を読めなくするか)の比較案を実装前にオーナーへ提示してから着手する」と
されたことに基づく。**この案の決定後に実装(サブエージェント → レビュー →
PR)へ進む。**

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
  「専用スコープ `catalog:full` は設けない」の既存決定)。
  → **今は read キー 1 本で全プラントのタグ名・PLC アドレス・構造を列挙できる**
- external_name = `{conn}.{group}.{tag}`(`hub.rs:432`、`write:` と同一の綴り)。
  unique だが**リネームで変わる**(安定 id は `(conn_id, group_id, tag_id)` の
  三つ組)
- スコープは `api_keys.scopes`(JSON 配列 TEXT、`db.rs:153`)に保存、**長さ
  上限なし**
- 既存テスト: write 専用キーは `/api/v1/tags`(`rest.rs:4360`)・`/api/v1/stream`
  (`stream.rs:695`)で 403 になる(read ゲートの前例が既にある)

## 2. 決めること

- **主論点**: スコープ外タグを catalog でどう扱うか
  - **案 A(隠す)**: スコープ外タグは一覧に出さない
  - **案 B(出すが読ませない)**: 一覧には全タグを出し、スコープ外の値読みを拒否
- **サブ決定**(実装に必要、A/B と併せて): S1 文法、S2 後方互換、S3 単一読みの
  応答、S4 ストリーム/バルク

## 3. 案 A vs 案 B(比較)

| 観点                          | 案 A: 隠す                                                           | 案 B: 出すが読ませない                                 |
| ----------------------------- | -------------------------------------------------------------------- | ------------------------------------------------------ |
| 情報開示                      | スコープ内タグのみ可視。**PLC アドレス・タグ名・構造の漏洩を最小化** | 全タグ定義(PLC アドレス含む)を全 read キーに開示       |
| 最小権限                      | ◎ need-to-know を満たす                                              | △ 値は守るが「存在・アドレス」は守らない               |
| 一貫性(catalog = 読める集合)  | ◎ 一致                                                               | ✗「一覧に出るのに 403」で直感に反する                  |
| 発見可能性                    | クライアントは全体像を見られない(admin へ問い合わせ)                 | クライアントが全タグを発見でき、要求すべき範囲が分かる |
| catalog revision / キャッシュ | キー毎に異なる catalog(revision は全体値のまま = 要注意)             | 全キー同一 catalog(単純)                               |
| 実装範囲                      | catalog + bulk + 単一 + stream + gRPC を全てスコープで絞る           | 値読み経路のみゲート、catalog は不変(小さい)           |

- **A の要点**: per-tag read の目的(最小権限の**情報**アクセス)を実際に達成する。
- **B の要点**: 単純だが、catalog が PLC アドレスを含むため「値は守るが偵察情報は
  全開示」になり、per-tag read の主目的の多くを損なう。

## 4. 推奨: 案 A(隠す)

理由:

1. per-tag read の目的は最小権限の**情報**アクセス。A は達成し、B は PLC
   アドレス・タグ在庫を全 read キーへ開示して目的を損なう(catalog は設計上
   アドレスを含むため、B では値だけ守っても偵察情報が全開示になる)
2. 一貫性: catalog に見える集合 = 読める集合。「見えるのに 403」を作らない
3. 既存の「write 専用キーは catalog で 403」の自然な延長(全拒否 → スコープ絞り)
4. 後方互換: 素の `read` は従来どおり全 catalog。`read:{tag}` を持つキーのみ絞る

## 5. サブ決定(案 A 採用を前提とした推奨つき)

- **S1 文法**: `read:{conn}.{group}.{tag}`(完全一致、`write:` と対称)を追加。
  加えて **`read:{conn}.{group}.*` のグループ・ワイルドカードを read に限り許可**
  することを推奨する。理由: read は一括操作で WebSocket の GroupWildcard
  (`{conn}.{group}.*`)と整合し、スコープ配列の肥大も防ぐ。write は誤書き込みの
  被害が大きいのでワイルドカード無しのまま(read と write で非対称にする)
- **S2 後方互換**: 素の `read` = 全タグ(従来動作不変)を維持する。既存キーは
  無影響。これは推奨というより互換要件
- **S3 単一値読み**(スコープ外タグ `GET /api/v1/values/{tag}`): **404 を推奨**
  (案 A の「隠す」と一貫し、存在を確認させない)。案 B を採るなら 403
- **S4 ストリーム/バルク**: WebSocket / gRPC ストリームは購読解決結果を
  スコープで交差(∩)し、`GET /api/v1/values`(bulk)はスコープ内のみ返す。
  gRPC `ReadValues`/`GetCatalog` も同様

## 6. 実装スケッチ(案 A の feasibility)

- `ApiKeyContext` に `can_read(external_name) -> bool` を追加 = 素の `read` を
  持つ **or** `read:{external_name}`/該当ワイルドカードに一致。認証層の 403 判定用に
  `has_read_scope` は「read 系スコープを 1 つ以上持つ」に一般化する
- read ハンドラに ctx を通す。REST は既に extensions に在り `v1_write_value`
  だけが取り出しているので、read ハンドラでも取り出す。gRPC は既に
  `authenticate` が返す ctx を捨てているので束縛して使う。session 認証の read
  (ctx 無し)は全アクセスを維持(admin UI 不変)
- 絞る対象: `v1_tags`/`GetCatalog`、`v1_values`/`ReadValues`、`v1_value_single`、
  `v1_stream`/`StreamValues`(`subscribe_core` の resolve をスコープで交差)
- external_name はリネームで変わる → スコープから外れて **fail-closed**(安全側で
  読めなくなる)。仕様として記録する。安定 id ベースのスコープはより複雑なので
  今回は採らない
- テスト: `read:{tag}` キーで in-scope は読める / out-of-scope は catalog に
  出ない・単一は 404・stream の購読解決から除外、素の `read` は全取得(不変)、
  gRPC も同様。write 専用キーの既存 403 テストは不変

## 7. スコープ外(この提案では扱わない)

- **案 C(一覧には出すが PLC アドレス等を redact)**: 「存在は見せるが偵察情報は
  隠す」中間案。A より実装が複雑。必要ならオプションとして後日検討
- **スコープ配列長の上限**: 現状無し。per-tag read で長くなりうるが、S1 の
  ワイルドカード採用で緩和される。明示上限を設けるかは付随論点として別途

## 8. 決定記入欄(オーナー)

- 主論点(A / B / C): _______
- S1 文法(`read:{tag}` のみ / ワイルドカードも許可): _______
- S3 単一読みの応答(404 / 403): _______
- 備考: _______
