/**
 * T11-1 連続登録（docs/ux-plan.md §3「T11: タグの一括登録」、連続登録は
 * オーナー発案・2026-08-06 採用）: 名前パターン + 開始アドレス + 点数 +
 * 共通設定から連続タグ（{@link TagInput} の配列）を生成する。
 *
 * **設計判断（2026-08-07）: パターン展開はクライアント側（ここ）が担う**
 * - 生成結果（プレビュー表示）がそのまま `POST /api/tags/batch` への送信
 *   内容になるので、「プレビューに出た通りに登録される」を保証できる。
 * - サーバー側の一括 API（`banto_hub_core::rest::tags_batch` /
 *   `banto_tags::TagService::create_batch`）は展開済み `TagInput[]` を
 *   受け取るだけの汎用エンドポイントのまま保てる（T11-2 の CSV インポート
 *   も同じ API を、別の展開ロジック — CSV パース — から呼ぶだけで済む）。
 *
 * **R2b-6d-2a（docs/banto-hub-driver-ui-design.md §4.3・§7・§7.1、2026-10-24）: アドレスの
 * 計算はドライバに任せる**。以前はここ（と `slmpDeviceTable.ts`）が SLMP のデバイス表記・
 * 基数・ビットの軸を読んで番号を進めていた。いまは `POST /api/tags/offset-addresses`
 * （`unit: "points"`。1 点ごとに型のワード数ぶん動く）が返したアドレスをそのまま使い、
 * 名前パターン・開始番号の既定も `parse-addresses` の結果（`canonical`・`area`・`index`）から
 * 作る。ドライバが解釈できないアドレス（以前の「末尾の数字を進める」フォールバック）は
 * 推測せず、API のエラーをその行（件目）に出す。
 *
 * 計算を「REST の結果を受け取って行を組み立てる純関数（{@link generateContinuousTags}）」と
 * 「REST を呼ぶ薄い層（{@link computeContinuousTags}）」に分けてある。単体テストは
 * `continuousRegistration.test.ts`（vitest、REST の結果を偽で与える）。
 */
import type {
	OffsetAddressItem,
	ParsedAddress,
	StringEncoding,
	TagDataType,
	TagInput
} from './tagRegistryAdmin';
import { parseOptionalNumber, toOptionalNumberOrNull } from './tagFormNumeric';
import { realAddressApi, type AddressApi } from './addressSpan';

/** 1回のプレビュー/バッチで生成できる点数の上限（暴走防止の安全弁）。 */
export const MAX_CONTINUOUS_COUNT = 1000;

/**
 * 名前パターンを展開する。`{n}` を `n`（開始番号からの連番）で置換する。
 *
 * **設計判断（2026-08-07）: `{n}` が無いパターンはエラーではなく末尾に
 * 連番を付与する**。連続登録という機能の性質上「パターンに連番を含める」
 * ことが前提だが、`{n}` の書き忘れを入力エラーとして拒否するより、
 * 「多分こう意図しただろう」という自然な結果（`temp` → `temp1`,
 * `temp2`, ...）を返す方が UI として親切、かつ生成結果はどのみち適用前
 * プレビューで確認できるので誤りがあれば気付ける。
 */
export function expandNamePattern(pattern: string, n: number): string {
	if (pattern.includes('{n}')) return pattern.split('{n}').join(String(n));
	return `${pattern}${n}`;
}

export interface ContinuousRegistrationCommon {
	collectionGroupId: number;
	dataType: TagDataType;
	stringLength?: number | null;
	/** T20 ①a-UI: `dataType === 'string'` のときのみ意味を持つ。既定 `'utf8'`。 */
	stringEncoding?: StringEncoding | null;
	unit?: string | null;
	decimals: number;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	enabled: boolean;
	writable: boolean;
}

export interface ContinuousRegistrationParams extends ContinuousRegistrationCommon {
	/** `{n}` を含む名前パターン（例 `temp{n}`）。含まなければ末尾に連番付与。 */
	namePattern: string;
	/** 連番の開始値（`{n}` に入る最初の数）。 */
	startNumber: number;
	/** 1件目のアドレス。2件目以降はドライバが 1 点ずつ（型のワード数ぶん）動かす。 */
	startAddress: string;
	/** 生成する点数（1以上、{@link MAX_CONTINUOUS_COUNT} 以下）。 */
	count: number;
}

/**
 * 連続登録フォーム（`+page.svelte` の `continuousForm`）の生の入力状態。
 *
 * **TAG-P0-1（2026-08-09、docs/banto-hub-desktop-plan.md §9 TAG-P0-1）:
 * フィールドの型宣言は `string` だが実際の代入値は必ずしも文字列ではない**。
 * Svelte 5 の `<input type="number" bind:value>` は空欄で `null` を、
 * 入力時は `number` を代入する（`<input type="text">`/`<select>` のみが
 * 実際に `string` を代入する）。この型宣言と実代入値のズレが、旧
 * `continuousParams()` の `form.count.trim()` 呼び出しで
 * `TypeError: count.trim is not a function` を起こしていた（`count` は
 * `<input type="number">` 由来で number|null）。
 *
 * 本 interface はこの「型は string だが実体は string|number|null」という
 * 実態を明示するため、number input 由来のフィールド（`startNumber`/
 * `count`/`decimals`/`stringLength`/`rawLo`〜`engHi`）は
 * `string | number | null` とし、text input・select 由来のフィールド
 * （`collectionGroupId`/`namePattern`/`startAddress`）のみ `string` に
 * 保つ。パースは {@link buildContinuousParams} が
 * {@link parseOptionalNumber}/{@link toOptionalNumberOrNull} を通して行う
 * ため、`.trim()` は string 保証フィールドにのみ現れる。
 */
export interface ContinuousFormState {
	collectionGroupId: string;
	namePattern: string;
	startNumber: string | number | null;
	startAddress: string;
	count: string | number | null;
	dataType: TagDataType;
	stringLength: string | number | null;
	/** select 由来のため常に `string`（{@link StringEncoding}）を保つ。 */
	stringEncoding: StringEncoding;
	unit: string;
	decimals: string | number | null;
	rawLo: string | number | null;
	rawHi: string | number | null;
	engLo: string | number | null;
	engHi: string | number | null;
	enabled: boolean;
	writable: boolean;
}

/**
 * T19 S1-b（UX-35、docs/banto-hub-t19-design.md §2「連続登録の名前パターン
 * — デバイス名（M/D/W/B 等）を既定とし、開始番号はアドレスから導出（入力
 * 不要）」、2026-09-02 オーナー決定）: 開始アドレスの解析結果から連続登録フォームの
 * 名前パターンの既定値を導出する。
 *
 * R2b-6d-2a（§7.1）: `canonical` が `area` で始まる（大小文字を無視）なら `{area}{n}`
 * （`D3000` → `D{n}`）、そうでなければ `tag{n}`（Modbus の `400001` は領域名が
 * アドレスに現れない）。解析に失敗したとき（`ok: false`・`undefined`）は `null`
 * （導出できない。呼び出し側は欄を空に戻す）。
 */
export function namePatternFromParsed(parsed: ParsedAddress | undefined): string | null {
	if (!parsed?.ok) return null;
	return startsWithArea(parsed) ? `${parsed.area}{n}` : 'tag{n}';
}

/** `canonical` が `area` で始まる（大小文字を無視）か。 */
function startsWithArea(parsed: ParsedAddress & { ok: true }): boolean {
	return (
		parsed.area !== null &&
		parsed.area !== '' &&
		parsed.canonical.toLowerCase().startsWith(parsed.area.toLowerCase())
	);
}

/**
 * T19 S1-b（UX-35、同design「開始番号はアドレスから導出（入力不要）」）: 開始アドレスの
 * 解析結果から連続登録フォームの開始番号の既定値を導出する。
 *
 * R2b-6d-2a（§7.1）: `canonical` が `area` で始まるなら `index`（ワード内ビット `D100.5` は
 * ワードの番号 100。`index` が無ければ `null`）、そうでなければ `canonical` の末尾の数字を
 * 10 進で読む（Modbus は正規形が 6 桁なので `40001` は 400001 になる。決めるのは名前の番号で
 * アドレスではない）。導出できなければ `null`（`buildContinuousParams` が空欄を 0 として扱う）。
 */
export function startNumberFromParsed(parsed: ParsedAddress | undefined): number | null {
	if (!parsed?.ok) return null;
	if (startsWithArea(parsed)) return parsed.index;
	const match = /^\D*(\d+)$/.exec(parsed.canonical);
	return match ? Number.parseInt(match[1], 10) : null;
}

/**
 * T19 S1-b（UX-35）: 開始アドレスの解析結果が届いたときの、名前パターン欄の
 * プリフィル判定。`#lib/banto/tagNamePrefill.ts::nextTagNameOnAddressChange`
 * と同じ「touched 追跡」方式 - ユーザーが名前パターン欄を一度でも直接編集
 * したら `namePatternTouched` を立て、以後はアドレス入力に追従させない。
 * `null` を返せば呼び出し側は名前パターン欄に触れない。アドレスが空・解析できない間は
 * 空文字列（欄を空に戻す）。
 */
export function nextNamePatternOnParsed(
	address: string,
	parsed: ParsedAddress | undefined,
	namePatternTouched: boolean
): string | null {
	if (namePatternTouched) return null;
	if (address.trim() === '') return '';
	return namePatternFromParsed(parsed) ?? '';
}

/**
 * T19 S1-b（UX-35）: 開始番号欄のプリフィル判定。{@link nextNamePatternOnParsed} と同じ
 * 「touched 追跡」方式。導出できない間は空文字列を返す - 開始番号欄を空のまま保つ
 * （`buildContinuousParams` が空欄を `startNumber: 0` 既定として扱う、既存の
 * `parseOptionalNumber` の挙動どおり）。
 */
export function nextStartNumberOnParsed(
	address: string,
	parsed: ParsedAddress | undefined,
	startNumberTouched: boolean
): string | null {
	if (startNumberTouched) return null;
	if (address.trim() === '') return '';
	const derived = startNumberFromParsed(parsed);
	return derived === null ? '' : String(derived);
}

export interface ContinuousRegistrationRow {
	name: string;
	address: string;
}

export type ContinuousRegistrationResult =
	{ ok: true; rows: ContinuousRegistrationRow[]; tags: TagInput[] } | { ok: false; error: string };

/**
 * API を呼ぶ前の入力の検証。誤りがあれば最初の 1 件の文言、なければ `null`。
 * アドレスの文法の検証はしない（ドライバが決める）。
 */
export function validateContinuousParams(params: ContinuousRegistrationParams): string | null {
	if (!Number.isInteger(params.count) || params.count < 1) {
		return '点数は1以上の整数で指定してください。';
	}
	if (params.count > MAX_CONTINUOUS_COUNT) {
		return `点数は${MAX_CONTINUOUS_COUNT}以下で指定してください。`;
	}
	if (params.startAddress.trim() === '') return '開始アドレスを入力してください。';
	if (params.dataType === 'string' && (!params.stringLength || params.stringLength < 1)) {
		return 'string 型では文字列長の指定が必要です。';
	}
	if (params.namePattern.trim() === '') return '名前パターンを入力してください。';
	return null;
}

/** `offset-addresses` に渡す項目（`i` 番目 = 開始アドレスから `i` 点ぶん）。 */
export function buildContinuousOffsetItems(
	params: ContinuousRegistrationParams,
	connectionId: number
): OffsetAddressItem[] {
	const address = params.startAddress.trim();
	return Array.from({ length: params.count }, (_, i) => ({
		connectionId,
		tagKind: 'plc' as const,
		address,
		dataType: params.dataType,
		stringLength: params.dataType === 'string' ? params.stringLength : undefined,
		stringEncoding: params.dataType === 'string' ? (params.stringEncoding ?? 'utf8') : undefined,
		n: i,
		unit: 'points' as const
	}));
}

/**
 * 連続登録のパターン展開本体（純関数）。`addresses[i]` は `i` 件目のアドレス
 * （{@link buildContinuousOffsetItems} を `offset-addresses` に渡した結果）。エラーは例外を
 * 投げず判別共用体で返す — 呼び出し元（連続登録フォーム）はプレビュー欄にそのまま表示するだけで
 * よい。算出できなかった件は、その件目のエラー（API のメッセージつき）。
 */
export function generateContinuousTags(
	params: ContinuousRegistrationParams,
	addresses: (ParsedAddress | undefined)[]
): ContinuousRegistrationResult {
	const invalid = validateContinuousParams(params);
	if (invalid) return { ok: false, error: invalid };

	const rows: ContinuousRegistrationRow[] = [];
	const seenNames = new Set<string>();

	for (let i = 0; i < params.count; i++) {
		const n = params.startNumber + i;
		const name = expandNamePattern(params.namePattern, n);
		if (name.trim() === '') {
			return {
				ok: false,
				error: `${i + 1}件目（連番${n}）の名前が空になります。名前パターンを見直してください。`
			};
		}
		if (seenNames.has(name)) {
			return {
				ok: false,
				error: `名前 "${name}" が連番の結果重複します。名前パターンまたは開始番号を見直してください。`
			};
		}
		seenNames.add(name);

		const result = addresses[i];
		if (!result?.ok) {
			return {
				ok: false,
				error: `${i + 1}件目（連番${n}）のアドレスを算出できません: ${result && !result.ok ? result.message : '算出結果が返りませんでした'}`
			};
		}
		rows.push({ name, address: result.canonical });
	}

	const tags: TagInput[] = rows.map((row) => ({
		name: row.name,
		collectionGroupId: params.collectionGroupId,
		address: row.address,
		dataType: params.dataType,
		stringLength: params.dataType === 'string' ? params.stringLength : undefined,
		stringEncoding: params.dataType === 'string' ? (params.stringEncoding ?? undefined) : undefined,
		rawLo: params.rawLo,
		rawHi: params.rawHi,
		engLo: params.engLo,
		engHi: params.engHi,
		unit: params.unit,
		decimals: params.decimals,
		enabled: params.enabled,
		writable: params.writable
	}));

	return { ok: true, rows, tags };
}

/**
 * {@link generateContinuousTags} の前に REST を呼ぶ薄い層。入力を検証してから、全件を 1 回の
 * `offset-addresses`（`unit: "points"`）で求める。通信の失敗は、推測で埋めずエラーにする。
 */
export async function computeContinuousTags(
	params: ContinuousRegistrationParams,
	connectionId: number,
	api: Pick<AddressApi, 'offset'> = realAddressApi
): Promise<ContinuousRegistrationResult> {
	const invalid = validateContinuousParams(params);
	if (invalid) return { ok: false, error: invalid };
	try {
		const addresses = await api.offset(buildContinuousOffsetItems(params, connectionId));
		return generateContinuousTags(params, addresses);
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error);
		return { ok: false, error: `アドレスの算出に失敗しました（${message}）。` };
	}
}

/**
 * {@link ContinuousFormState}（フォームの生の入力値）から
 * {@link generateContinuousTags} への入力 {@link ContinuousRegistrationParams}
 * を組み立てる。まだプレビューを生成するには情報不足（対象グループ未選択・
 * 名前パターン/開始アドレス未入力・点数が未入力または数値化できない）
 * であれば `null` を返す（エラー表示を急がず、フォーム入力途中は
 * プレビュー欄を静かに空にするだけにする方針、旧 `continuousParams()`
 * を踏襲）。
 *
 * **TAG-P0-1（2026-08-09）**: `count`/`startNumber`/`decimals` は number
 * input 由来で `null` が入り得るため {@link parseOptionalNumber} でパースし、
 * `.trim()` は string 保証フィールド（`collectionGroupId`/`namePattern`/
 * `startAddress`）にのみ使う。`count` が `undefined`（未入力・非数）の場合は
 * この関数自体が `null` を返す（`generateContinuousTags` 側の
 * 「1以上の整数か」の検証はここでは行わない — それは後段の責務）。
 */
export function buildContinuousParams(
	form: ContinuousFormState
): ContinuousRegistrationParams | null {
	if (
		form.collectionGroupId === '' ||
		form.namePattern.trim() === '' ||
		form.startAddress.trim() === ''
	) {
		return null;
	}
	const count = parseOptionalNumber(form.count);
	if (count === undefined) {
		return null;
	}
	return {
		collectionGroupId: Number(form.collectionGroupId),
		namePattern: form.namePattern,
		startNumber: parseOptionalNumber(form.startNumber) ?? 0,
		startAddress: form.startAddress,
		count,
		dataType: form.dataType,
		stringLength: form.dataType === 'string' ? toOptionalNumberOrNull(form.stringLength) : null,
		stringEncoding: form.dataType === 'string' ? form.stringEncoding : null,
		unit: form.unit === '' ? undefined : form.unit,
		decimals: parseOptionalNumber(form.decimals) ?? 0,
		rawLo: toOptionalNumberOrNull(form.rawLo),
		rawHi: toOptionalNumberOrNull(form.rawHi),
		engLo: toOptionalNumberOrNull(form.engLo),
		engHi: toOptionalNumberOrNull(form.engHi),
		enabled: form.enabled,
		writable: form.writable
	};
}
