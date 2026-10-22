/**
 * T19 S1-b（UX-34、docs/banto-hub-t19-design.md §2・§3.3、2026-09-02
 * オーナー決定「`writable` の既定 ON。ただし収集グループ単位で変更可
 * （条件付き適用）」）: 新規タグフォームの `writable` チェックボックスの
 * 既定値を決める、依存ゼロの純関数モジュール。
 *
 * **既定 ON をグローバルに適用すると登録が失敗するタグがある**
 * （design §3.3）:
 *
 * - **computed タグ**は `writable` にできない
 *   （`crates/banto-tags/src/tag.rs`「computed タグは writable にできません
 *   （値は式が決まります）」）。**この判定は `tag_kind` を見るだけで済み、
 *   プロトコル知識を必要としない**ため本モジュールが直接持つ。
 * - **Modbus の読み取り専用領域**（`1xxxx`=discrete input /
 *   `3xxxx`=input register）は登録時に拒否される（2026-09-01 オーナー
 *   決定A）。
 *
 * **2026-09-02 オーナー判断（S1-b0 分離）: 上記2つ目（アドレス領域による
 * 判定）は本モジュールに実装しない。** UI 側にアドレスの文法を写すと
 * プロトコルが増えるたびに複数箇所を直す羽目になるため、方針は「プロトコル層
 * （ドライバ）で定義し、UI はサーバーから受け取ったデータを使うだけ」に統一する。
 *
 * **R2b-2d（2026-10-22）で配線した**: そのデータ供給は
 * `POST /api/tags/parse-addresses`（`tagRegistryAdmin.ts::parseTagAddresses`、
 * 保存と同じ解析）の `writableArea` で、`tags/+page.svelte` が結果を
 * 第2引数に渡す。これで以前の `GET /api/tags/address-writable` は消えた。
 *
 * そのため {@link canDefaultWritable}/{@link writableDefaultBlockedReason}
 * は**アドレス文字列を一切受け取らない**。受け取るのは `writableArea`
 * （`boolean | undefined`）という**外部から渡されるフラグ**だけ:
 *
 * - `true`: ドライバが「書き込める領域」と判定した。
 * - `false`: 「読み取り専用の領域」と判定した。
 * - `undefined`: **判定不能**（アドレスが空・入力途中、解析が `ok: false`、
 *   通信の失敗、グループ未選択）。**既定 ON を当てない**（安全側 -
 *   誤って読み取り専用領域に既定 ON するより、既定が一時的に効かない方を選ぶ。
 *   判定不能を「制限なし」に倒さない）。
 *
 * 本モジュールは**登録時の検証を一切変えない** - サーバー側の8段ゲート
 * （API キーのスコープ完全一致・書き込み受付トグル・レート制限・監査を
 * 含む）はそのまま効く。ここで決めるのはあくまで新規作成フォームを開いた
 * 時点の「チェックボックスの初期状態」という UI 側の既定値だけ。
 */
import type { TagKind } from './tagRegistryAdmin';

/**
 * design §3.3「PLC タグかつ書き込み可能な領域のときのみ」既定 ON を
 * 適用してよいか。
 *
 * - `tagKind !== 'plc'`（computed/internal）は常に `false` - internal
 *   タグは `writable` を禁止されてはいないが（`tag.rs` の
 *   `INTERNAL_TAG_KIND` 分岐に `writable` チェックが無い）、design の
 *   適用条件が明示的に「PLC タグ」に限定しているため、internal タグの
 *   チェックボックスは（禁止されているわけではないが）これまでどおり
 *   既定 OFF のまま - 手動で opt-in する対象という位置づけを変えない。
 * - `writableArea`（モジュール冒頭コメント参照 - 解析の API が返すアドレス領域判定）
 *   が**明示的に `true`** のときだけ既定を適用する。`false`（読み取り専用）も
 *   `undefined`（判定不能）も適用しない（R2b-2d で以前の「`undefined` は適用してよい」
 *   から変えた。判定不能を既定 ON に倒すと、読み取り専用領域のタグが保存で断られる）。
 */
export function canDefaultWritable(
	tagKind: TagKind,
	writableArea: boolean | undefined = undefined
): boolean {
	if (tagKind !== 'plc') return false;
	return writableArea === true;
}

/**
 * `writable` の既定を適用しない場合に利用者へ見せる理由。適用できる
 * （{@link canDefaultWritable} が `true`）場合は `null`。
 *
 * - `computed`: 「値は式が決まる」ため常に不可（既存の書き込み安全設定
 *   セクション自体が非表示になるので、この文言は主にツールチップ／補助
 *   説明用）。
 * - `internal`: design の適用条件（PLC タグ限定）に単に該当しないだけで
 *   「禁止」ではないため、ブロック理由としては返さない（`null`）- 手動で
 *   ON にできることが既存の挙動どおり伝わるよう、否定的な理由表示は
 *   出さない。
 * - `plc` かつ `writableArea === false`: 解析の API が「読み取り専用の領域」と
 *   返したときに到達する。文言はプロトコル非依存の汎用表現に留める -
 *   「どのアドレス領域だから」の具体名（`1xxxx` 等）は API のデータが持つ情報で
 *   あり、本モジュールはそれを知らない。判定不能（`undefined`）は理由を出さない
 *   （既定を当てないだけで、手動で ON にはできる）。
 */
export function writableDefaultBlockedReason(
	tagKind: TagKind,
	writableArea: boolean | undefined = undefined
): string | null {
	if (tagKind === 'computed') {
		return 'computed タグは値が式で決まるため、書き込み可（writable）にできません。';
	}
	// S3（docs/banto-hub-external-db-design.md §6-10「v1 は読み取り専用」）:
	// db タグは常に writable=false へ正規化される（サーバー側はエラーに
	// せず黙って正規化するが、UI 側では computed と同じ「書き込み安全
	// 設定」セクション自体を非表示にする - 呼び出し側 `tags/+page.svelte`
	// の該当 `{#if}` 参照）。
	if (tagKind === 'db') {
		return 'db タグは読み取り専用（v1）のため、書き込み可（writable）にできません。';
	}
	if (tagKind !== 'plc') return null;
	if (writableArea === false) {
		return 'このアドレスの領域は読み取り専用のため、書き込み可（writable）にできません。';
	}
	return null;
}

/**
 * R2b-2d（オーナーレビュー P2、2026-10-22）: 解析の API が**読み取り専用の領域と
 * 確定した**とき、保持している `writable` の値を `false` に揃える。
 *
 * 既定 ON の適用（{@link canDefaultWritable}）は利用者が手動で触っていない間だけ
 * 働く。触った後に読み取り専用のアドレス（例: Modbus `40001` → `30001`）へ変えると、
 * チェックボックスは「無効・未チェック」に見えるのに保持値 `true` が残り、送信で
 * サーバーに断られる。表示・保持値・送信値を一致させるため、この関数が**確定した
 * 断り**のときだけ `false` を返す。
 *
 * - `tagKind === 'plc'` かつ `writableArea === false` のときだけ `false`。
 * - `undefined`（判定不能: 入力途中・解析失敗・通信失敗）は**読み取り専用と見なさない**。
 *   利用者の手動の値をそのまま返す。
 * - 返すのは `false` か元の値だけ（`true` に戻すことはしない）ので、呼び出し側の
 *   `$effect` が自分の書き込みで再実行されてもループしない。
 */
export function writableAfterAreaVerdict(
	tagKind: TagKind,
	writableArea: boolean | undefined,
	writable: boolean
): boolean {
	if (tagKind === 'plc' && writableArea === false) return false;
	return writable;
}

/**
 * R2b-2d（Codex レビュー P2・オーナーレビュー P2、2026-10-22）: 送信の時点の `writable` を
 * 決める純関数。解析の結果（`area`）が届いてから**その時点のフォームの値**
 * （`touched`・`writable`・グループの既定）を渡して呼ぶこと。解析を待つ前に値を
 * 取っておくと、待っている間に利用者が直したチェックボックスを古い値で上書きする。
 *
 * - `touched` が `false` の間だけ既定（{@link canDefaultWritable} かつグループの既定）を当てる。
 *   `true`（手動で触った）なら `writable` をそのまま使う。
 * - どちらの場合も最後に {@link writableAfterAreaVerdict} で、確定した読み取り専用は `false`。
 * - `area` が `undefined`（判定不能: 解析の失敗・`ok: false`）は既定 ON を当てず、手動の値を保つ。
 *   保存は止めない（最終的な判定はサーバー）。
 */
export function applyAreaVerdict(args: {
	tagKind: TagKind;
	area: boolean | undefined;
	touched: boolean;
	writable: boolean;
	groupDefault: boolean;
}): boolean {
	const base = args.touched
		? args.writable
		: canDefaultWritable(args.tagKind, args.area) && args.groupDefault;
	return writableAfterAreaVerdict(args.tagKind, args.area, base);
}
