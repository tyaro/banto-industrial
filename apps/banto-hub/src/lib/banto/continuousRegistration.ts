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
 * 依存なしの純関数群 — フォーム（`+page.svelte`）から状態を持ち込まず
 * テスト可能にしてある。単体テストは `continuousRegistration.test.ts`
 * （vitest、`tagFormNumeric.test.ts` と同じ describe/it スタイル。
 * TAG-P0-1、2026-08-09 に追加）。
 */
import type { TagDataType, TagInput } from './tagRegistryAdmin';
import { parseOptionalNumber, toOptionalNumberOrNull } from './tagFormNumeric';
import { formatSlmpAddress, MAX_DEVICE_NUMBER, parseSlmpAddress } from './slmpDeviceTable';

/** 1回のプレビュー/バッチで生成できる点数の上限（暴走防止の安全弁）。 */
export const MAX_CONTINUOUS_COUNT = 1000;

const TWO_WORD_DATA_TYPES: ReadonlySet<TagDataType> = new Set(['i32', 'u32', 'f32']);

/**
 * データ型からアドレス増分を決める（docs/ux-plan.md §3）: ワード系
 * （bit/i16/u16）は+1、2ワード型（i32/u32/f32）は+2、string は
 * +string_length。`crates/banto-tags/src/tag.rs::ALLOWED_DATA_TYPES` に
 * 存在するデータ型はこの3分岐で尽くされる（bit タグがビットデバイス
 * （M100 等）に置かれるかワードのビット位置（D100.5）に置かれるかは
 * アドレス書式の話であって data_type の話ではないので、増分自体は
 * bit/i16/u16 のどれでも同じ「+1」でよい — 実際にどちらの形かは
 * {@link incrementAddress} がアドレス自体の形（`.N` 付きかどうか）から
 * 判別する）。
 */
export function addressIncrement(dataType: TagDataType, stringLength?: number | null): number {
	if (dataType === 'string') return Math.max(1, stringLength ?? 1);
	if (TWO_WORD_DATA_TYPES.has(dataType)) return 2;
	return 1;
}

/**
 * アドレスを1件分進める。`step` は {@link addressIncrement} が返すワード
 * 増分、`index` は連番中の何件目か（0始まり）。
 *
 * **T18-3c（docs/banto-hub-t18-design.md「T18-3c 連続登録の基数/bit
 * 連番」、2026-08-13）: SLMP デバイス記法（`slmpDeviceTable.ts` 経由で
 * `crates/banto-plc/src/slmp/address.rs` の規則を参照）を認識できるときは
 * その軸に沿って増分する**。ワード内 bit 連番（`.N` 付き）とデバイス番号の
 * 連番（16進デバイスの桁上がりを含む）を自動判別する — 以前はどちらも
 * 「非対応」としてエラーにしていたが、軸をアドレス自体の形から一意に
 * 決められる（`.N` があれば bit 軸、無ければデバイス番号軸）ため、判断を
 * 保留する理由がなくなった。
 *
 * - **bit 軸**（{@link parseSlmpAddress} が `bit` を返す場合）: `number` と
 *   `bit` を「1ワード=16bit」の1本の数直線上の値
 *   （`number * 16 + bit`）とみなし、そこへ `index` を足してから
 *   16で割った商・余りに戻す。`step` は使わない（bit 連番は常に1行=1bit
 *   進む — ワード型の `step` を混ぜると「2bit飛ばし」のような意味の
 *   薄い増分になってしまうため）。例: `D100.14` → (i=1) `D100.15` →
 *   (i=2) `D101.0`（bit15 の次はワード+1・bit0）。
 * - **デバイス番号軸**（bit サフィックスが無い場合）: `number + step *
 *   index` をそのデバイスの基数で整形し直す。16進デバイス
 *   （X/Y/B/W/SB/SW/DX/DY）は16進の桁上がりが自然に起こる
 *   （`Number.parseInt`/`toString(16)` が基数変換そのものを担うため、
 *   10進の下2桁だけを見て繰り上げるような特別扱いは不要）。例: `X1E` →
 *   (i=1) `X1F` → (i=2) `X20`、`W1FF` → (i=1) `W200`。
 *
 * どちらの軸でも、結果のデバイス番号が {@link MAX_DEVICE_NUMBER} を
 * 超える場合は `null`（呼び出し元 `generateContinuousTags` がその行を
 * エラーにする）。
 *
 * **SLMP デバイス記法として解釈できないアドレスへのフォールバック**:
 * Modbus 参照番号（`"40001"` 等、デバイスニーモニックを持たない）や
 * その他 {@link parseSlmpAddress} が `null` を返す形式は、T18-3c 以前と
 * 同じ「先頭の非数字列 + 末尾の10進数字列」を `base + step * index` で
 * 増分する素朴なロジックにフォールバックする（既存の10進連番の挙動は
 * 不変にする、という受け入れ条件のための後方互換パス）。
 */
export function incrementAddress(address: string, step: number, index: number): string | null {
	const trimmed = address.trim();

	const parsed = parseSlmpAddress(trimmed);
	if (parsed) {
		if (parsed.bit !== undefined) {
			const total = parsed.number * 16 + parsed.bit + index;
			const number = Math.floor(total / 16);
			const bit = total % 16;
			if (number > MAX_DEVICE_NUMBER) return null;
			return formatSlmpAddress(parsed.mnemonic, number, bit);
		}
		const number = parsed.number + step * index;
		if (!Number.isFinite(number) || number < 0 || number > MAX_DEVICE_NUMBER) return null;
		return formatSlmpAddress(parsed.mnemonic, number);
	}

	// フォールバック: SLMP デバイス記法として解釈できないアドレス
	// （Modbus 参照番号など）向けの、T18-3c 以前と同じ素朴な10進増分。
	const match = /^(\D*)(\d+)$/.exec(trimmed);
	if (!match) return null;
	const [, prefix, digits] = match;
	const base = Number.parseInt(digits, 10);
	const next = base + step * index;
	if (!Number.isFinite(next) || next < 0) return null;
	const nextDigits = String(next).padStart(digits.length, '0');
	return `${prefix}${nextDigits}`;
}

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
	unit?: string | null;
	decimals: number;
	rawLo?: number | null;
	rawHi?: number | null;
	engLo?: number | null;
	engHi?: number | null;
	thresholdH?: number | null;
	thresholdHh?: number | null;
	thresholdL?: number | null;
	thresholdLl?: number | null;
	enabled: boolean;
	writable: boolean;
}

export interface ContinuousRegistrationParams extends ContinuousRegistrationCommon {
	/** `{n}` を含む名前パターン（例 `temp{n}`）。含まなければ末尾に連番付与。 */
	namePattern: string;
	/** 連番の開始値（`{n}` に入る最初の数）。 */
	startNumber: number;
	/** 1件目のアドレス。2件目以降は {@link addressIncrement} 分ずつ増える。 */
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
 * `count`/`decimals`/`stringLength`/`rawLo`〜`thresholdLl`）は
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
	unit: string;
	decimals: string | number | null;
	rawLo: string | number | null;
	rawHi: string | number | null;
	engLo: string | number | null;
	engHi: string | number | null;
	thresholdH: string | number | null;
	thresholdHh: string | number | null;
	thresholdL: string | number | null;
	thresholdLl: string | number | null;
	enabled: boolean;
	writable: boolean;
}

export interface ContinuousRegistrationRow {
	name: string;
	address: string;
}

export type ContinuousRegistrationResult =
	{ ok: true; rows: ContinuousRegistrationRow[]; tags: TagInput[] } | { ok: false; error: string };

/**
 * 連続登録のパターン展開本体。エラーは例外を投げず判別共用体で返す —
 * 呼び出し元（連続登録フォーム）はプレビュー欄にそのまま表示するだけで
 * よい。
 */
export function generateContinuousTags(
	params: ContinuousRegistrationParams
): ContinuousRegistrationResult {
	if (!Number.isInteger(params.count) || params.count < 1) {
		return { ok: false, error: '点数は1以上の整数で指定してください。' };
	}
	if (params.count > MAX_CONTINUOUS_COUNT) {
		return { ok: false, error: `点数は${MAX_CONTINUOUS_COUNT}以下で指定してください。` };
	}

	const trimmedAddress = params.startAddress.trim();
	if (trimmedAddress === '') {
		return { ok: false, error: '開始アドレスを入力してください。' };
	}
	// T18-3c: bit 軸（`.N` 付きアドレス）は data_type が bit のタグでしか
	// 意味を持たない（16bit 値の1ワードをビット単位でずらして書き込む、
	// という操作が成立しないため）。ここで弾かないと
	// `generateContinuousTags` はワード連番のつもりで `.N` を生成してしまう。
	const parsedStartAddress = parseSlmpAddress(trimmedAddress);
	if (parsedStartAddress?.bit !== undefined && params.dataType !== 'bit') {
		return {
			ok: false,
			error:
				'ビット指定アドレス（例: D100.5）のワード内 bit 連番は、データ型が bit のタグでのみ使用できます。'
		};
	}
	if (params.dataType === 'string' && (!params.stringLength || params.stringLength < 1)) {
		return { ok: false, error: 'string 型では文字列長の指定が必要です。' };
	}
	if (params.namePattern.trim() === '') {
		return { ok: false, error: '名前パターンを入力してください。' };
	}

	const step = addressIncrement(params.dataType, params.stringLength);
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

		const address = incrementAddress(trimmedAddress, step, i);
		if (address === null) {
			return {
				ok: false,
				error: `${i + 1}件目（連番${n}）のアドレスを算出できません（開始アドレス "${trimmedAddress}" の形式が未対応か、デバイス番号の上限（0x00FFFFFF）を超えています）。`
			};
		}
		rows.push({ name, address });
	}

	const tags: TagInput[] = rows.map((row) => ({
		name: row.name,
		collectionGroupId: params.collectionGroupId,
		address: row.address,
		dataType: params.dataType,
		stringLength: params.dataType === 'string' ? params.stringLength : undefined,
		rawLo: params.rawLo,
		rawHi: params.rawHi,
		engLo: params.engLo,
		engHi: params.engHi,
		unit: params.unit,
		decimals: params.decimals,
		thresholdH: params.thresholdH,
		thresholdHh: params.thresholdHh,
		thresholdL: params.thresholdL,
		thresholdLl: params.thresholdLl,
		enabled: params.enabled,
		writable: params.writable
	}));

	return { ok: true, rows, tags };
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
		unit: form.unit === '' ? undefined : form.unit,
		decimals: parseOptionalNumber(form.decimals) ?? 0,
		rawLo: toOptionalNumberOrNull(form.rawLo),
		rawHi: toOptionalNumberOrNull(form.rawHi),
		engLo: toOptionalNumberOrNull(form.engLo),
		engHi: toOptionalNumberOrNull(form.engHi),
		thresholdH: toOptionalNumberOrNull(form.thresholdH),
		thresholdHh: toOptionalNumberOrNull(form.thresholdHh),
		thresholdL: toOptionalNumberOrNull(form.thresholdL),
		thresholdLl: toOptionalNumberOrNull(form.thresholdLl),
		enabled: form.enabled,
		writable: form.writable
	};
}
