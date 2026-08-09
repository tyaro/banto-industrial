/**
 * タグ登録フォーム（`+page.svelte` の単票 create/update・連続登録）が使う
 * 数値系フィールドの共通パース処理。
 *
 * Svelte 5 の `<input type="number">` は `bind:value` の対象を空にすると、
 * 他の `<input>` と違って空文字列 `''` ではなく **`null`** を代入する。
 * `+page.svelte` の `FormState`/`ContinuousFormState` は数値系フィールド
 * （`stringLength`/`rawLo`/`rawHi`/`engLo`/`engHi`/`thresholdH`/`thresholdHh`/
 * `thresholdL`/`thresholdLl` 等）を TypeScript 上は `string` と宣言している
 * ため、この `null` 混入は型チェックでは検出できない
 * （docs/banto-hub-desktop-plan.md §16.4「`optNum` の null 取りこぼし」）。
 * 旧 `optNum` は `s === '' ? undefined : Number(s)` しか見ておらず、
 * `Number(null) === 0` により「入力欄をクリアしたつもりが 0 として
 * サイレントに保存される」バグを生んでいた。
 *
 * `tagCsv.ts`/`continuousRegistration.ts` と同じ設計方針 — 依存なしの純関数
 * を lib へ切り出し、`tagFormNumeric.test.ts`（vitest）で単体テストする。
 */

/**
 * フォーム入力値を「省略可能な数値」として解釈する。
 *
 * - `null` / `undefined` / `''` / 空白のみの文字列 → `undefined`（未設定）
 * - 有限の `number`、または有限な数値へ変換できる文字列 → その数値
 * - それ以外（`NaN`、変換できない文字列、真偽値等）→ `undefined`
 *
 * 「未設定」と「入力エラー」を区別しない（= 変換できない値も静かに
 * `undefined` として扱う）のは旧 `optNum` の既存挙動を踏襲したもの — 入力欄の
 * 検証・インラインエラー表示は呼び出し側（`+page.svelte` の `errors`）の
 * 責務であり、本関数の役割は「送信ペイロードへ意図しない `0` を紛れ込ませ
 * ない」ことに限定する。
 */
export function parseOptionalNumber(value: unknown): number | undefined {
	if (value === null || value === undefined) return undefined;
	if (typeof value === 'number') return Number.isFinite(value) ? value : undefined;
	if (typeof value !== 'string') return undefined;
	if (value.trim() === '') return undefined;
	const n = Number(value);
	return Number.isFinite(n) ? n : undefined;
}

/**
 * {@link parseOptionalNumber} の結果を `null` に正規化するラッパー。
 *
 * 連続登録（`ContinuousRegistrationParams`）のように、フィールドが
 * `number | undefined` ではなく `number | null` を要求する呼び出し元向け
 * （`optNum(...) ?? null` を毎回書く代わりにこちらを使う）。
 */
export function toOptionalNumberOrNull(value: unknown): number | null {
	return parseOptionalNumber(value) ?? null;
}
