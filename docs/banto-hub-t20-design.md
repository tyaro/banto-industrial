# banto-hub T20 設計: 文字列・構造体登録・レシピ・ビットデバイス

作成日: 2026-09-04
状態: **T20 全機能完了（2026-09-05）**: ④ ワードデバイスのビット16進＋レジスタ・ダイアレクト抽象 / ② 構造体タグ登録＋オフセットコピー / ③ レシピ一括書き込み（REST＋MCP・UI は不採用） / ① 文字列 read/write（案A・分離経路: write＝write_path、read＝read-on-demand）。オーナー決定は §2、設計判断は §3・§8、調査結果は §7。①完了時の小さな宿題（監査テキスト保持・エンコ選択 UI・config 往復・read-on-demand 監査の方針）は 2026-09-05 に片付け済み（§3.1 末尾）。
対象: 4つの新機能（①文字列 read/write、②構造体タグ登録＋オフセットコピー、③レシピ一括書き込み、④ワードデバイスのビット `.0〜.F`）

関連: [tag-server-design.md](tag-server-design.md)（タグ空間・書き込み安全の一次ソース）、[banto-hub-t19-design.md](banto-hub-t19-design.md)（直前の UI/UX 群）、[banto-tagclient-design.md](banto-tagclient-design.md) §4.4（③が覆す旧決定）。

---

## 1. 背景と狙い

T19 で UI/UX を整えた後、オーナーから4つの機能追加要望が出た（2026-09-04）。産業用途で
「設定値一式をまとめて流す（レシピ）」「文字列（品名・ロット等）を読み書きする」「PLC の
ワードデバイスのビットを個別に操作する」「構造体的なタグ群を素早く登録する」実需要に応える。

**重要な前提（調査で確定、2026-09-04）**: ①と③は**ドライバ層（`banto-plc` / `banto-plc-write` /
`banto-broker`）が既に下地を持っている**。①③④の難所は主に **hub のパイプライン（収集キャッシュ・
REST/MCP の値 DTO・書き込み経路）**の側にある。

## 2. オーナー決定（2026-09-04）

| 項目 | 決定                                                                                                                                              |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------- |
| ①    | 文字列 read/write を実装。**文字コードは既定 UTF-8、タグ単位で Shift-JIS も選択可**                                                               |
| ②    | 構造体タグ登録を実装。**テンプレートの永続保存は不要**だが、**オフセットコピーは有用**                                                            |
| ③    | レシピ一括書き込みを実装。**[banto-tagclient-design.md](banto-tagclient-design.md) §4.4「実装しない」決定（2026-09-01）を覆す**（需要が出たため） |
| ④    | ワードデバイスのビットを **16進 `.0〜.F`** で指定可能にする                                                                                       |

## 3. 各機能の現状と設計

### 3.1 ①文字列 read/write

**現状（調査済み）**:

- **ドライバ層は対応済み**: `banto-plc` に `PlcValue::Str(String)` と `StringReadRequest`、
  `banto-plc-write` に `StringWriteRequest`（`words`・`value: String`、Shift-JIS bytes + 0x00
  パディング、registry 上限 128 words / wire 上限 960）がある。registry も `data_type = 'string'`
  ＋ `string_length` を許可。
- **hub パイプラインは未対応**: 収集キャッシュ `banto_collect::current::Sample.value` は
  **`Option<f64>`**。`PlcValue::Str` は f64 変換で `None` に落ちる（`banto-plc` の
  `PlcValue::Str(_) => None`）。REST/MCP の値 DTO も数値前提（`{ v: f64 }`）。書き込み側は
  `write_path::RequestedValue` が **`Num`/`Bool` のみ**で String variant が無く、
  `parse_requested_value` も文字列を扱わない。

**つまり文字列は hub レベルでは read も write も通っていない。** 難所はドライバではなく hub の
値表現。

**設計**:

- **値表現**: 収集キャッシュと値 DTO を「数値または文字列」を運べる形に拡張する。素直な案は
  `Sample` の値を `f64` から `enum SampleValue { Num(f64), Str(String), Bit(bool)… }` 相当へ
  広げる（`Copy` を失うため影響範囲を確認）。あるいは**文字列タグ専用の別キャッシュ**を持ち、
  数値パイプラインは f64 のまま温存する（影響を局所化できるが二重管理になる）。
  **未決（§3 の詰め対象）**: どちらを採るか。
- **read**: 収集タスクが `PlcValue::Str` を上記の値表現に載せ、REST `GET /api/v1/values/*` と
  MCP `read_tag_values` が文字列値を返せるようにする。
- **write**: `RequestedValue::Str(String)` を追加し、gate 8 の型対称性（string タグ ↔ string 値）を
  拡張。`parse_requested_value` が JSON 文字列を受ける。`execute_write` は string タグに対して
  `banto-plc-write` の `StringWriteRequest`（＝`BatchWriteRequest::String`）を組み立ててブローカーへ
  渡す（既存経路に載る）。
- **文字コード**: **既定 UTF-8、タグ単位で Shift-JIS を選択可**。registry に**タグ単位のエンコーディング
  設定（新カラム、例 `string_encoding TEXT DEFAULT 'utf8' CHECK IN ('utf8','shift_jis')`）**を足す。
  現行ドライバは Shift-JIS 固定なので、UTF-8 対応と選択の配線が要る。
  **注意**: 上流 `banto-plc-write` の Shift-JIS 固定を UTF-8 選択可へ広げる必要がある。共有クレートの
  変更になるので relay-wright への影響（文字列書き込みを使っていないか）を確認する。

**スライス感**: 中〜大。値表現の拡張が要になるため、read パイプライン → write → エンコーディング選択、と
段階を分ける。

**方向の確定（オーナー決定 2026-09-05）: 案A（分離経路）。** 文字列タグは収集（記録計）
パイプラインから意図的に除外されている（config.rs S1 制約・数値専用の境界。「文字列の消費者は
S2 エンジン＝relay-wright」）。この境界を尊重し、**書き込みは write_path 経由**、**読み取りは
その場読みの新経路**（①b）で扱い、current_values/tstore/収集の string スキップには触れない。

**①a（文字列 write）: 完了（2026-09-05）。** `banto-plc-write` に `StringEncoding`(Utf8/ShiftJis)
を追加し `StringWriteRequest.encoding` で選択（`encode.rs` の SHIFT_JIS 固定を解消）。共有型なので
relay-wright の2箇所は `ShiftJis` を明示して**挙動保全**（Shift-JIS のまま・テスト全通過）。
banto-tags に migration 0013 で `string_encoding` 列（既定 utf8・CHECK）を追加し `Tag`/`TagInput`
へ配線。write_path は `RequestedValue::Str` を追加、gate 7（`convert_value`）が string タグに文字列
のみ受け入れ（型対称）、`build_plc_string_write_request` で `BatchWriteRequest::String` を組み立て
（単票・バッチ・MCP が自動対応）。監査 `value_requested`(REAL) は文字列では NULL だが、**宿題#1（2026-09-05）で専用列 `value_requested_text`（`db.rs` の後追い ADD COLUMN・冪等）を追加し、文字列書き込みのテキストを監査に残すよう解消**（`RequestedValue`/`ConvertedValue::as_audit_text` 経由で全6監査サイトに配線・`set_result` 非干渉・write-audit UI にも表示）。UTF-8/Shift-JIS が実機
シミュレータのワイヤに正しいバイトで届くことをテスト固定。エンコーディング選択の登録 UI は**宿題#2（2026-09-05）で全登録経路（単票・連続・構造体・CSV）＋設定バックアップ/復元に追加**（併せて `tagCellEdit`/`tagBulkOps` の暗黙 utf8 リセット・`tagCsvDiff` の変更誤判定・`configPackage` 往復欠落の潜在バグも修正）。

**①b（文字列 read-on-demand）: 完了（2026-09-05）。** current_values/tstore を経由せず PLC から
直接その場読みする経路。`StringEncoding` を banto-plc に移設（write の①a と read で共有、
banto-plc-write は re-export）、`decode_string_value` がエンコーディングに従って UTF-8/Shift-JIS を
選ぶ。relay-wright の文字列読み（poller.rs）は `ShiftJis` を明示して挙動保全。hub は
`read_path.rs::execute_read_now`（catalog 解決 → 非スポーン peek handle → `BatchReadRequest`
組み立て〈String はタグの string_encoding〉→ 数値は scale 適用）を追加し、REST
`GET /api/v1/values/{tag}/read-now`（read スコープ・per-tag can_read_value）と MCP `read_tag_now`
で公開。write→read-now で UTF-8/Shift-JIS が往復すること、collection cache が持たない値を
read-now が返せること（cache 非経由の証明）をテスト固定。**これで T20 完了。**

**宿題（2026-09-05・完了）**: ①完了時に残した小さな限界を片付けた — (1) 監査の文字列テキスト保持（宿題#1、上記）、(2) エンコーディング選択の登録 UI ＋ config 往復（宿題#2、上記）、(3) read-on-demand の監査は**意図的に残さない**（`GET .../read-now`・MCP `read_tag_now`）: `hub_write_audit` は「書き込み（変更）」の記録であり、collection cache 読みを含め読み取りは元々監査対象外＝一貫した設計。読み取りアクセスログが要る場合は別機能として設計する。

### 3.2 ②構造体タグ登録（デバイス自動割付・手動割付・オフセットコピー）

**現状**: 一括作成 API（`POST /api/tags/batch`）と、フロントの `continuousRegistration.ts`
（`buildContinuousParams`/`generateContinuousTags`、アドレスの算術で連番タグを生成）がある。
構造体という概念・自動割付ロジックは無い。

**設計（オーナー決定: テンプレート永続保存なし・オフセットコピーは有用）**:

- **構造体の一括登録**: 複数フィールド（名前・型・任意の相対オフセット）をまとめて登録する
  UI。ベースアドレスから**ワードサイズ考慮の連番割付**（i16/u16/bit=1ワード相当、i32/u32/f32=2、
  string=`ceil(string_length*? / 2)` ワード、bit は M デバイス or ワードデバイスのビット）を
  自動で行う「自動割付」と、各フィールドのアドレスを個別指定する「手動割付」の両方。
- **オフセットコピー**: 既に登録済みのタグ群（＝構造体1インスタンス）を選び、**アドレスに一定
  オフセットを加えて複製**する（例: `D3000` 起点の10タグを `+100` して `D3100` 起点に複製）。
  これは `continuousRegistration` の「アドレス算術」を群単位へ一般化したもの。**テンプレートを
  保存せずに** 実インスタンスからの複製で再利用性を得る、というオーナー方針に合致。

  **命名ルール（オーナー決定 2026-09-05、②b で確定・実装済み）**: コピー先の名前はタグごとに
  適応的に決める。(a) 元タグ名が**デバイス名由来**（名前がアドレス表記そのもの、例 `D3000` に
  名前 `D3000`/`d3000`）なら、**新アドレスのデバイス名**にする（大文字/小文字の流儀を踏襲。
  例 +100 で `D3100`）。(b) それ以外の**意味名**（例 `temp01`）なら、**末尾に数字を付ける**
  （末尾が数字なら増やす・zero-pad 幅を保持、無ければ 2 から。既存と衝突しない値まで進める）。
  デバイス名由来で名前が既存と衝突する場合は自動改名せずエラーにする（アドレス由来名が黙って
  別名になるのは分かりにくいため。②b 実装時の判断）。

- **衝突検出**: 自動割付・オフセットコピーとも、既存タグとアドレス範囲が重なる場合に警告/拒否。
- 実装は主にフロント＋既存 `tags/batch`。新規サーバー API は原則不要（割付・衝突判定はクライアント側、
  最終保存は batch）。

**スライス感**: 中。フロント中心。`continuousRegistration` の一般化。

### 3.3 ③レシピ一括書き込み（§4.4 の旧決定を覆す）

**旧決定**: [banto-tagclient-design.md](banto-tagclient-design.md) §4.4 オーナー決定1（2026-09-01）
「バッチ・レシピ書き込みは実装しない（需要が出るまで保留）」。**本 T20 でこれを覆す**（需要が出た）。
tagclient 設計文書の当該記述も「T20 で実装へ方針転換」と追記して整合させる。

**現状（調査で判明した強い下地）**: **ブローカーの `write(requests: Vec<BatchWriteRequest>)` が
既に複数リクエストの一括書き込みを完全サポート**（`Job::Write`、`BatchWriteRequest` は
Numeric/String/BitInWord 混在可、`plan_slmp_write_batch`/`write_batch_mixed`）。hub は現状
**毎回1要素の Vec** を送っているだけ。

**設計**:

- **hub にレシピ書き込み経路**を足す。REST（例 `POST /api/v1/values/batch`）と MCP ツール
  （例 `write_recipe`）で、`[{tag, value}, ...]` を受ける。
- **各エントリは execute_write と同じ8段ゲートを通す**（catalog/writable/enabled/simulation/
  protocol/write_enabled/rate limit/値変換）。§3.7（MCP は抜け道を作らない）と同じ思想を
  バッチにも適用する。ロックダウン後の MCP は既存どおりアドバイザリ（T19 S5）。
- **原子性（未決・§3 の詰め対象）**: PLC 書き込みは本質的に非トランザクショナル（SLMP に
  トランザクションは無い）。全 all-or-nothing は不可能に近い。方針候補:
  - (a) **best-effort ＋ per-entry 結果一覧**（各タグの ok/拒否理由を返す）。ゲート検証は
    書き込み前に全件行い、1件でもゲート NG なら**1件も書かない**（＝「事前検証は all-or-nothing、
    実書き込みは best-effort」）。実書き込み中の PLC エラーは以降を中断して結果に記録。
  - (b) 同一接続分をブローカーの1回の `write_batch` にまとめて**接続内では直列・原子性寄り**に
    する（`BatchWriteRequest` の Vec を丸ごと1ジョブで送る）。複数接続に跨るレシピは接続ごとに
    分割。
  - **決定（2026-09-04、オーナー承認）**: (a) の事前ゲート all-or-nothing ＋ 同一接続は (b) で1ジョブ化 ＋ per-entry 結果を返す、で確定。
- **名前付きレシピの保存（決定: しない、2026-09-04 オーナー承認）**: 今回は**一括書き込み
  プリミティブに留める**（クライアントが値セットを持ち、まとめて送る）。名前付きレシピの
  永続化は入れない（②のテンプレート保存不要と同じ思想）。需要が出れば別途。

**スライス感**: 中。ドライバ下地があるので hub の endpoint ＋ ゲート束ね ＋ 結果集約が主。

**③a（バッチ書き込みコア）: 完了（2026-09-05）。** `write_path.rs` を副作用の無い解決フェーズ
（`resolve_write_target`＝gate 1〜4）と値変換（`convert_value`＝gate 7）に分割し、単票
`execute_write` は**元のゲート順（1〜4→5→6→7→8）を厳密に維持**（リファクタで挙動を変えない。
「write_control off ＋ 型不一致」が 503 のままであることを回帰ガードで固定）。`execute_write_batch`
は全エントリを解決＋変換で事前検証（1件でも NG なら無書込＝all-or-nothing、成功予定は
`BatchAborted`）、gate 5/6 をバッチ単位で判定、gate 8 は**接続ごとに1回 `handle.write(Vec)`**。
per-entry 結果を返す。**同一バッチ内の重複タグは拒否**（`DuplicateTagInBatch`。レシピで同一
タグに2値は曖昧、かつレート制限 peek の粒度も正確になる）。REST `POST /api/v1/values/batch`
（単票と同じ認証規律・per-entry の `write:{tag}` スコープ検査・常に200の per-entry 封筒）。
事前ゲート all-or-nothing は「1件 NG → 監査行数不変＋シミュレータ値不変」でテスト固定。

**原子性の是正（2026-09-05、実機検証で発覚）**: 当初 gate 7 のうち数値レンジ検査
（`validate_numeric_range`）が commit フェーズの `build_plc_write_request` でしか
行われず、事前ゲート（`prepare_batch_entry`＝resolve＋`convert_value`）に含まれて
いなかった。このためレシピ中の1値がレジスタ型レンジ外のとき、有効な値だけが先に
書かれる**部分適用**が実機で発生した（applied=2 を確認）。`prepare_batch_entry` に
レンジ検査を組み込み、レンジ NG も all-or-nothing で全件中止するよう是正
（単票 `execute_write` のゲート順＝write_control off の 503 が値エラーの 422 より
優先、は不変）。実機で bad レシピ→applied=0・他タグ不変を再確認。回帰テスト
`range_out_of_bounds_entry_aborts_the_whole_batch...`（tests/t20_batch_write.rs）で固定。

**③b（MCP `write_recipe`）: 完了（2026-09-05）。** `execute_write_batch` を叩く MCP ツール `write_recipe`（`{writes:[{tag,value}]}`）を追加。`write_tag_value` と同型の安全ポリシー: **ロックダウン後はアドバイザリのみ**（`execute_write_batch` を呼ばず、推奨レシピを助言。監査/レジスタ不変をテスト固定）、ロックダウン前は per-entry の `write:{tag}` スコープ検査→`execute_write_batch`。応答は per-entry 封筒＋`applied` 件数。

**③c（レシピ UI）: 不採用（オーナー決定 2026-09-05）。** タグサーバーの責務は
一括書き込みの手段（REST `POST /api/v1/values/batch` ＋ MCP `write_recipe`）を提供する
ところまでで、レシピの編集・保存・呼び出しといった UI は**下流アプリ（レシピ DL アプリ等）の
領分**とする。よって **③ は ③a＋③b で完了**。

### 3.4 ④ワードデバイスのビット `.0〜.F`（16進）

**現状（ほぼ実装済み）**: ビット付きアドレス（T8、`tag-server-design.md` §6.1）は読み書きとも
実装済み。`banto-plc-write` に `BatchWriteRequest::BitInWord`（SLMP はビット専用書き込みが無いため
**read-modify-write-confirm**）、テスト `bit_in_word_write_through_the_broker_lands_and_reads_back`
もある。**ただしビット指数の基数が 10進**: `banto-plc/src/slmp/address.rs:364` は
`let bit: u8 = bit_text.parse()`（＝10進 `.parse::<u8>()`）で、`D100.15` は通るが `D100.F` は通らない。
Modbus 側（`banto-plc/src/address.rs:163`）も同様。

**設計（オーナー決定: 16進）**:

- ビット指数を **16進 `.0〜.F`（0〜15）** で受けるよう `u8::from_str_radix(bit_text, 16)` に変える。
  範囲 0x0〜0xF を検証。SLMP の MELSEC 標準表記に一致する。
- **意味の変化（要注意）**: 現状 10進の `D100.10`〜`D100.15` は、16進化すると `.10` 以上は
  範囲外（0xF まで）になり**拒否**される（`.10`＝16進で 16 は範囲外）。既存の10進表記タグが
  あれば移行が要る。若いプロダクトなので実害は小さい見込みだが、**既存タグの棚卸し**を行う。
  Modbus 側の基数を 16進に揃えるか 10進のまま残すかは要検討（レジスタ内ビットの慣習）。
- UI（アドレス入力のヘルプ・バリデーション）とドキュメント（§6.1）を 16進表記に更新。

**スライス感**: 小。パーサの基数変更＋範囲＋テスト＋UI/doc。既存の書き込み経路（RMW）はそのまま。

## 4. スライス構成（提案）

規模と依存の少なさから、着手しやすい順:

1. **④ビット16進**（小・独立・下地あり）
2. **②構造体登録＋オフセットコピー**（中・フロント中心・`continuousRegistration` の一般化）
3. **③レシピ一括書き込み**（中・ドライバ下地あり・hub endpoint＋ゲート束ね）
4. **①文字列 read/write**（中〜大・値表現の拡張が要・共有クレートのエンコーディング変更を含む）

①③④は独立。②も独立。並行可能だが、①は共有クレート（`banto-plc-write` のエンコーディング）に
触れるため relay-wright への影響確認を伴う。

## 5. 既知事実（調査結果、2026-09-04）

- **文字列**: ドライバは `PlcValue::Str` / `StringReadRequest` / `StringWriteRequest` を持つ（Shift-JIS
  固定・128 words 上限）。hub の `Sample.value` は `Option<f64>` で文字列を運べない。`RequestedValue`
  は `Num`/`Bool` のみ。
- **バッチ**: ブローカー `write(Vec<BatchWriteRequest>)` が混在バッチを完全サポート。hub は1要素 Vec を
  送っているだけ（＝レシピは hub の配線で足りる）。
- **ビット**: T8 でビット付きアドレスの読み書き実装済み。基数が 10進なのが唯一の差分。
- **構造体**: `continuousRegistration.ts` がアドレス算術による連番生成を持つ（オフセットコピーの下地）。
- **③が覆す旧決定**: banto-tagclient-design.md §4.4 オーナー決定1。

## 6. 着手前に詰める未決点

- **①**: 文字列値の表現（`Sample` を enum 化するか、文字列専用キャッシュを別立てするか）。
- **③**: 原子性・名前付きレシピ非保存とも **2026-09-04 オーナー承認で確定**（§3.3）。残りは REST/MCP の
  具体的な request/response 形の詰めのみ。
- **④**: Modbus 側のビット基数を 16進へ揃えるか 10進のまま残すか。既存10進タグの棚卸し。

## 7. 更新対象ドキュメント

- [tag-server-design.md](tag-server-design.md) §6.1（ビット表記を16進へ）、書き込み安全（バッチ/文字列）。
- [banto-tagclient-design.md](banto-tagclient-design.md) §4.4（レシピ「実装しない」→ T20 で実装へ方針転換を追記）。
- [docs/README.md](README.md) 文書地図に本書を追加。
