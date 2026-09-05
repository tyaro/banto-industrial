# banto-hub MCP リファレンス（MES / ゲートウェイ実装者向け）

作成日: 2026-09-05
状態: **現行**。T19 S5（UX-41）で実装、T20 で `read_tag_now` / `write_recipe` を追加。
2026-09-05 に実機 R08ENCPU（SLMP）で 6 ツールを検証済み（結果は §8）。
関連: [tag-server-design.md](tag-server-design.md)（タグ空間・書き込み安全の一次ソース）、
[banto-hub-t20-design.md](banto-hub-t20-design.md)（文字列・レシピ・ビットの設計）、
[banto-hub-operations.md](banto-hub-operations.md)（起動・ポート・運用）。

> この文書は MCP 経由でタグを読み書きする外部クライアント（MES、ゲートウェイ、
> 保全ツール、AI エージェント）の実装者向けの索引。タグ空間モデルや書き込み安全
> ゲートの**正**は tag-server-design.md 側にある。

## 1. エンドポイントと認証

- **エンドポイント**: `POST /mcp`
- **プロトコル**: JSON-RPC 2.0 over Streamable HTTP（手書き実装）。`initialize` /
  `tools/list` / `tools/call` に対応。
- **認証**: API キー（`bh_...`）を `Authorization: Bearer bh_...` ヘッダで渡す。
  管理者 UI / 管理 REST の Bearer トークンとは別物（API キーは機械クライアント専用）。
- **応答本文**: `tools/call` は MCP 標準の `result.content[0].text` に **JSON 文字列**を
  入れて返す（クライアントは `text` を再パースする）。

最小の呼び出し例:

```bash
curl -X POST http://<hub>/mcp \
  -H "Authorization: Bearer bh_xxxxxxxx" \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}'
```

## 2. スコープ（API キー）

API キーは発行時に**スコープの明示列挙**を持つ（`banto_hub_core::api_keys`）。

| スコープ                           | 意味                                                       |
| ---------------------------------- | ---------------------------------------------------------- |
| `read`                             | 全タグの読み取り                                           |
| `read:{connection}.{group}.{tag}`  | 単一タグの読み取り（完全一致）                             |
| `read:{connection}.{group}.*`      | グループ単位の読み取り                                     |
| `write:{connection}.{group}.{tag}` | 単一タグの書き込み（**ワイルドカード不可・明示列挙のみ**） |

- 外部名は常に `{connection}.{group}.{tag}` の3セグメント。**グループ名は全体で一意**。
- write スコープにワイルドカードは使えない（レシピで複数タグへ書くなら、その全タグを
  個別に列挙する）。

## 3. ロックダウンと書き込み受付（安全ゲート）

MCP は REST/gRPC と**同じ `execute_write` / `execute_write_batch` を通り、抜け道を作らない**
（tag-server-design.md §5.6、banto-hub-t20-design.md §3.7）。書き込みが実機へ届くには次を満たす:

1. **試運転モード（commissioning）**: 書き込みツールは実際に `execute_write(_batch)` を呼ぶ。
2. **ロックダウン後**: 書き込みツール（`write_tag_value` / `write_recipe`）は
   **アドバイザリのみ**（`execute_write` を呼ばず、推奨だけ返す）。読み取りツールは通常どおり。
   `get_server_status` の `lockedDown` で現在の状態が分かる。
3. **write-control**: `POST /api/write-control/enable` で書き込み受付を ON にする。
   **収集開始（新しい run）は `write_enabled` を False にリセットする**（安全設計）ので、
   有効化は**収集開始の後**に行う。
4. 各書き込みは per-tag の `writable` フラグ、write スコープ、シミュレーション/プロトコル、
   レート制限、値変換（型対称性・レンジ）を通る。詳細は tag-server-design.md §6。

## 4. 読み取りツール

### `list_tags`

入力: `{}`（引数なし）。応答: 登録タグの一覧。

```json
{
	"tags": [
		{
			"connection": "line1",
			"group": "g1",
			"name": "line1.g1.temp",
			"dataType": "f32",
			"tagKind": "plc",
			"unit": "℃",
			"enabled": true,
			"writable": true
		}
	]
}
```

### `read_tag_values`

収集キャッシュ（current_values）からの読み取り。**数値専用**（高速・ポーリング値）。
入力: `{"tags":["line1.g1.temp", ...]}`（省略時は全タグ）。

```json
{
	"values": [
		{ "tag": "line1.g1.temp", "value": 25.4, "quality": "good", "timestamp": 1788593514267 }
	]
}
```

- **文字列タグはキャッシュに載らない**（current_values は数値専用）。文字列は
  `quality:"bad"` / `value:null` になる — 文字列は必ず `read_tag_now` を使う。

### `read_tag_now`（T20 ①b）

PLC からの**その場読み**（キャッシュ非経由）。文字列タグの読み取りはこれを使う。
数値・bit タグにも使える（収集が持たない最新値を直接取得）。入力: `{"tag":"line1.g1.recipe_name"}`。

```json
{ "tag": "line1.g1.recipe_name", "value": "工程A" }
```

- 文字列はタグの `stringEncoding`（UTF-8 / Shift-JIS）に従ってデコードされる。

### `get_server_status`

サーバー状態（収集・接続・ロックダウン・CPU/メモリ = UX-46）。入力: `{}`。

```json
{
	"collection_state": "running",
	"collection_mode": "configured",
	"write_enabled": true,
	"lockedDown": false,
	"run_id": 1,
	"version": "0.1.0",
	"connections": [{ "id": 3, "name": "line1", "status": "connected", "simulation": false }],
	"system": {
		"cpu_percent": 0.72,
		"host_memory_total_bytes": 34038706176,
		"host_memory_used_bytes": 29188083712,
		"process_memory_bytes": 28868608
	}
}
```

## 5. 書き込みツール

### `write_tag_value`

単一タグへの書き込み。入力: `{"tag":"line1.g1.sp","value":<工学値>}`。
値の型は data_type に対応させる — 数値タグは数値、**bit タグは真偽値**（`true`/`false`）、
string タグは文字列。

```json
// 成功
{ "result": "ok", "tag": "line1.g1.sp" }
```

- **ワードデバイスのビット**（T20 ④）: bit タグのアドレスは `D100.0`〜`D100.F`
  （**16進**、SLMP の慣習）。書き込みは read-modify-write で、同じワード内の他ビットを保持する。

### `write_recipe`（T20 ③b）

複数タグへの一括書き込み（レシピ）。入力:

```json
{
	"writes": [
		{ "tag": "line1.g1.sp_a", "value": 111 },
		{ "tag": "line1.g1.sp_b", "value": 222 },
		{ "tag": "line1.g1.name", "value": "工程A" }
	]
}
```

応答は per-entry 封筒＋適用件数:

```json
{
	"applied": 3,
	"writes": [
		{ "ok": true, "tag": "line1.g1.sp_a" },
		{ "ok": true, "tag": "line1.g1.sp_b" },
		{ "ok": true, "tag": "line1.g1.name" }
	]
}
```

**原子性（重要・banto-hub-t20-design.md §3.3、オーナー承認 2026-09-04）**:

- **事前ゲートは all-or-nothing**。全エントリを「解決（catalog/writable/enabled/protocol）
  → 値変換（型対称性・**レンジ**）」で事前検証し、**1件でも NG なら1件も書かない**
  （監査行も残さない）。例えばレシピ中の1値がレジスタ型のレンジ外なら、レシピ全体が
  拒否され、有効な値も適用されない。拒否時は NG エントリが理由付きのエラー、他エントリは
  `batch_aborted`。
- 事前検証を全件通過した後の**実書き込みは per-entry 結果**を返す（同一接続はブローカーの
  1ジョブにまとめる）。ここでの失敗（PLC 通信エラー等）は per-entry に現れ、`applied` は
  実際に書けた件数になる。
- 重複タグ（同じ外部名が2回以上）はレシピ全体を拒否する。
- レシピは**書き込みプリミティブ**であり、名前付きレシピの保存は Hub 側では行わない
  （値セットはクライアントが保持してまとめて送る）。

> レンジ外による部分適用は 2026-09-05 の実機検証で検出し、`prepare_batch_entry`
> （事前ゲート）にレンジ検査を組み込んで是正した。単票 `execute_write` のゲート順
> （write_control の 503 が値エラーの 422 より優先）は変更していない。

## 6. データ型と値表現

| data_type               | 値の型       | 備考                                                                  |
| ----------------------- | ------------ | --------------------------------------------------------------------- |
| `i16`/`u16`/`i32`/`u32` | 数値（整数） | レンジ外・非整数は 422 `value_out_of_range`                           |
| `f32`                   | 数値         | —                                                                     |
| `bit`                   | 真偽値       | アドレスは `Dxxx.0`〜`Dxxx.F`（16進）                                 |
| `string`                | 文字列       | `stringEncoding`（utf8/shift_jis）でエンコード。読みは `read_tag_now` |

## 7. エラーコード（書き込み）

`write_tag_value` の失敗、および `write_recipe` の per-entry 失敗は `error` フィールドに
コードを持つ（REST/gRPC と共通）。主なもの:

- `value_out_of_range` — 値がレジスタ型のレンジ外／非整数（`detail` に範囲）
- `writes_disabled` — write-control が OFF
- `missing_write_scope` — API キーに当該タグの write スコープが無い
- `batch_aborted` — レシピの事前ゲート all-or-nothing で他エントリの NG により中止

## 8. 実機検証（2026-09-05、R08ENCPU / SLMP、MCP 経由のみ）

実 PLC（空プログラム・SLMP）に対し、MCP `POST /mcp`（`bh_` キー）だけで T20 の新機能を検証:

- **①文字列 R/W**: UTF-8 タグ・Shift-JIS タグそれぞれに日本語文字列を `write_tag_value`
  → `read_tag_now` で往復一致。`read_tag_values`（cache）では文字列が `bad` になり、
  `read_tag_now`（PLC 直読）で正しく取得＝設計どおり。
- **③ビット `.0`〜`.F`**: 同一ワードに `bit.0` / `bit.A` / `bit.F` を順に true →
  ワード値が `0x0001` → `0x0401` → `0x8401`。`.A`=0x400・`.F`=0x8000 で **16進アドレス**を
  実証。1ビット解除後もワード値は他ビットを保持（RMW）。
- **②レシピ**: 正常系は `applied` が全件。異常系（レンジ外を1件混入）は
  **all-or-nothing で全件中止**（§5 の是正後の挙動）。

（実機 IP・ポートは本文書には残さない。運用手順は banto-hub-operations.md、
実機接続の癖は運用メモを参照。）
