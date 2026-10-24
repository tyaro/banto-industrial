/**
 * T20 機能②a 構造体タグ登録（docs/banto-hub-t20-design.md §3.2、2026-09-04
 * オーナー決定「テンプレートの永続保存は不要」「自動割付・手動割付の両方を
 * 実装」）。
 *
 * 連続登録（`continuousRegistration.ts`）が「1タグをN連番」で複製するのに
 * 対し、構造体登録は「複数の異なるフィールド」を1つのベースアドレスから
 * 連続したワード領域へ割り付ける。
 *
 * **R2b-6d-2a（docs/banto-hub-driver-ui-design.md §3.6・§4.3・§7.1、2026-10-24）: アドレスの
 * 計算はドライバに任せる**。以前はここ（と `slmpDeviceTable.ts`）が SLMP のデバイス表記を
 * 読んで番号を進め、占有ワードの範囲で重なりを判定していた。いまは
 * - ベースアドレスの検証と各フィールドのワード数は `POST /api/tags/parse-addresses` の結果
 *   （`bit`・`wordCount`）、
 * - フィールドごとのアドレスは `POST /api/tags/offset-addresses`（`unit: "words"`、累積の
 *   ワード数ぶん動かす。ワード内ビットのベースは断る）、
 * - 重なりの判定は `area`・`index`・`wordCount`（`addressSpan.ts`）
 * を使う。ドライバが解釈できないアドレスは、推測せずその行に API のエラーを出す。
 *
 * 計算を「REST の結果を受け取って行を組み立てる純関数」と「REST を呼ぶ薄い層
 * （{@link allocateStructFields}・{@link manualStructRows}）」に分けてある。純関数のテストは
 * `structRegistration.test.ts`（vitest。REST の結果を偽で与える）。
 */
import {
	spanOfParsed,
	spansOverlap,
	realAddressApi,
	type AddressApi,
	type AddressSpan
} from './addressSpan';
import type { ParsedAddress, StringEncoding, TagDataType, TagInput } from './tagRegistryAdmin';

/** 構造体の1フィールド定義。`address` は手動割付モードでのみ使う。 */
export interface StructField {
	name: string;
	dataType: TagDataType;
	stringLength?: number | null;
	/** `dataType === 'string'` のときのみ意味を持つ。既定 `'utf8'`。 */
	stringEncoding?: StringEncoding | null;
	/** 手動割付モードでのみ参照する。自動割付モードでは無視される。 */
	address?: string;
}

/**
 * 割付結果の1行。プレビュー表示にも {@link createTagsBatch} 相当への変換
 * （{@link structRowsToTagInputs}）にも使う共通の中間表現。`address` はドライバが返した
 * 正規形（保存される形）、`span` は衝突検出（{@link detectStructAddressCollisions}）が
 * 使う占有（ドライバが返した `area`・`index`・`wordCount`）。
 */
export interface StructAllocatedRow {
	name: string;
	address: string;
	dataType: TagDataType;
	stringLength?: number | null;
	stringEncoding?: StringEncoding | null;
	span: AddressSpan;
}

export type StructAllocationResult =
	{ ok: true; rows: StructAllocatedRow[] } | { ok: false; error: string };

/** ワード内ビットのベースアドレスを断る文言（以前の `allocateStructFields` と同じ）。 */
export const STRUCT_BASE_BIT_ERROR =
	'ベースアドレスにビット指定（例: D100.5）は使えません。ワードアドレス（例: D100）を指定してください。';

/** 1 つのフィールドを引く要求の素（アドレスだけ差し替える）。 */
function fieldItemBase(field: StructField) {
	return {
		tagKind: 'plc' as const,
		dataType: field.dataType,
		stringLength: field.dataType === 'string' ? (field.stringLength ?? null) : undefined,
		stringEncoding: field.dataType === 'string' ? (field.stringEncoding ?? 'utf8') : undefined
	};
}

/** 名前・文字列長の形の検証（API を呼ぶ前）。誤りがあれば最初の 1 件の文言。 */
export function validateStructFields(fields: StructField[]): string | null {
	if (fields.length === 0) return 'フィールドを1つ以上追加してください。';
	for (let index = 0; index < fields.length; index++) {
		const field = fields[index];
		if (field.name.trim() === '') {
			return `${index + 1}行目のフィールド名を入力してください。`;
		}
		if (
			field.dataType === 'string' &&
			(!field.stringLength || field.stringLength < 1 || !Number.isInteger(field.stringLength))
		) {
			return `${index + 1}行目（${field.name}）: string 型では文字列長を1以上の整数で指定してください。`;
		}
	}
	return null;
}

/**
 * ベースアドレスの検証。`probes` は同じアドレスを別のデータ型（`u16`・`bit`）で解析した結果
 * （型によって通る形が違うため、どちらかが通ればよい）。どちらも通らなければ最初のエラー、
 * 通ったものの `bit` が null でなければ（ワード内ビット `D100.5`）断る。
 */
export function checkStructBase(
	probes: (ParsedAddress | undefined)[]
): { ok: true } | { ok: false; error: string } {
	const passed = probes.find((p) => p?.ok);
	if (!passed?.ok) {
		const failed = probes.find((p) => p && !p.ok);
		return {
			ok: false,
			error: `ベースアドレスを解釈できません: ${failed && !failed.ok ? failed.message : '解析結果が返りませんでした'}`
		};
	}
	if (passed.bit !== null) return { ok: false, error: STRUCT_BASE_BIT_ERROR };
	return { ok: true };
}

/**
 * 各フィールドのワード数。`bit` 型は他の型と同じく 1 ワードを占める（ビット位置の
 * パッキングはしない）。それ以外は `parsed`（ベースアドレスをそのフィールドの型で解析した
 * 結果。`bit` 型の位置には `undefined`）の `wordCount`。解析に失敗したフィールドは行のエラー。
 */
export function structFieldWordCounts(
	fields: StructField[],
	parsed: (ParsedAddress | undefined)[]
): { ok: true; counts: number[] } | { ok: false; error: string } {
	const counts: number[] = [];
	for (let i = 0; i < fields.length; i++) {
		const field = fields[i];
		if (field.dataType === 'bit') {
			counts.push(1);
			continue;
		}
		const result = parsed[i];
		if (!result?.ok || result.wordCount === null) {
			return {
				ok: false,
				error: `${i + 1}行目（${field.name}）: ${result && !result.ok ? result.message : 'ワード数を確認できませんでした'}`
			};
		}
		counts.push(result.wordCount);
	}
	return { ok: true, counts };
}

/** 累積のワード数: 先頭は 0、以降は前のフィールドまでのワード数の和。 */
export function cumulativeWordOffsets(counts: number[]): number[] {
	const offsets: number[] = [];
	let sum = 0;
	for (const count of counts) {
		offsets.push(sum);
		sum += count;
	}
	return offsets;
}

function rowOf(field: StructField, parsed: ParsedAddress & { ok: true }): StructAllocatedRow {
	return {
		name: field.name,
		address: parsed.canonical,
		dataType: field.dataType,
		stringLength: field.dataType === 'string' ? field.stringLength : undefined,
		stringEncoding: field.dataType === 'string' ? field.stringEncoding : undefined,
		span: spanOfParsed(parsed)!
	};
}

/**
 * 自動割付の行を組み立てる。`moved[i]` はフィールド i のアドレスを累積ワード数ぶん動かした
 * 結果（`offset-addresses`、`unit: "words"`）。失敗した行は割付エラー（最初の 1 件）。
 */
export function assembleAutoStructRows(
	fields: StructField[],
	moved: (ParsedAddress | undefined)[]
): StructAllocationResult {
	const rows: StructAllocatedRow[] = [];
	for (let i = 0; i < fields.length; i++) {
		const result = moved[i];
		if (!result?.ok) {
			return {
				ok: false,
				error: `${i + 1}行目（${fields[i].name}）のアドレスを算出できません: ${result && !result.ok ? result.message : '算出結果が返りませんでした'}`
			};
		}
		rows.push(rowOf(fields[i], result));
	}
	return { ok: true, rows };
}

/** 手動割付の行を組み立てる。`parsed[i]` はフィールド i の入力アドレスの解析結果。 */
export function assembleManualStructRows(
	fields: StructField[],
	parsed: (ParsedAddress | undefined)[]
): StructAllocationResult {
	const rows: StructAllocatedRow[] = [];
	for (let i = 0; i < fields.length; i++) {
		const result = parsed[i];
		if (!result?.ok) {
			return {
				ok: false,
				error: `${i + 1}行目（${fields[i].name}）のアドレスを解釈できません: ${result && !result.ok ? result.message : '解析結果が返りませんでした'}`
			};
		}
		rows.push(rowOf(fields[i], result));
	}
	return { ok: true, rows };
}

function failedCall(what: string, error: unknown): StructAllocationResult {
	const message = error instanceof Error ? error.message : String(error);
	return { ok: false, error: `${what}に失敗しました（${message}）。` };
}

/**
 * 自動割付: `baseAddress` から、フィールドを順にワード数を考慮した連続アドレスへ割り付ける
 * （REST を呼ぶ薄い層）。フィールド i のアドレスは、ベースを累積ワード数
 * （前のフィールドまでのワード数の和）だけ `unit: "words"` で動かしたもの。
 * ベースがワード内ビット（`D100.5`）なら断る。`connectionId` は対象グループの接続。
 */
export async function allocateStructFields(
	connectionId: number,
	baseAddress: string,
	fields: StructField[],
	api: AddressApi = realAddressApi
): Promise<StructAllocationResult> {
	const base = baseAddress.trim();
	if (base === '') return { ok: false, error: 'ベースアドレスを入力してください。' };
	const shapeError = validateStructFields(fields);
	if (shapeError) return { ok: false, error: shapeError };

	let parsed: ParsedAddress[];
	try {
		// 先頭 2 件がベースの検証、続く fields.length 件が各フィールドのワード数（bit 型は不要）。
		parsed = await api.parse(connectionId, [
			{ tagKind: 'plc', address: base, dataType: 'u16' },
			{ tagKind: 'plc', address: base, dataType: 'bit' },
			...fields.map((f) =>
				f.dataType === 'bit'
					? { tagKind: 'plc' as const, address: base, dataType: 'u16' as const }
					: { ...fieldItemBase(f), address: base }
			)
		]);
	} catch (error) {
		return failedCall('ベースアドレスの確認', error);
	}
	const baseCheck = checkStructBase(parsed.slice(0, 2));
	if (!baseCheck.ok) return baseCheck;
	const counts = structFieldWordCounts(
		fields,
		fields.map((f, i) => (f.dataType === 'bit' ? undefined : parsed[2 + i]))
	);
	if (!counts.ok) return counts;

	const offsets = cumulativeWordOffsets(counts.counts);
	let moved: ParsedAddress[];
	try {
		moved = await api.offset(
			fields.map((f, i) => ({
				...fieldItemBase(f),
				connectionId,
				address: base,
				n: offsets[i],
				unit: 'words' as const
			}))
		);
	} catch (error) {
		return failedCall('アドレスの算出', error);
	}
	return assembleAutoStructRows(fields, moved);
}

/** 手動割付: 各フィールドが明示的に持つ `address` をドライバで解析して正規形にする（薄い層）。 */
export async function manualStructRows(
	connectionId: number,
	fields: StructField[],
	api: AddressApi = realAddressApi
): Promise<StructAllocationResult> {
	const shapeError = validateStructFields(fields);
	if (shapeError) return { ok: false, error: shapeError };
	for (let i = 0; i < fields.length; i++) {
		if (!fields[i].address?.trim()) {
			return {
				ok: false,
				error: `${i + 1}行目（${fields[i].name}）のアドレスを入力してください。`
			};
		}
	}
	try {
		const parsed = await api.parse(
			connectionId,
			fields.map((f) => ({ ...fieldItemBase(f), address: f.address!.trim() }))
		);
		return assembleManualStructRows(fields, parsed);
	} catch (error) {
		return failedCall('アドレスの確認', error);
	}
}

/** {@link detectStructAddressCollisions} が既存タグ側に要求する最小限の形。 */
export interface ExistingTagForCollision {
	name: string;
	collectionGroupId: number;
	/**
	 * 保存済みのアドレスの占有（`addressSpan.ts::fetchExistingSpans`）。`plc` 以外のタグ・
	 * 占有が分からないタグは `null`（名前の衝突だけ見る）。
	 */
	span: AddressSpan | null;
}

export interface StructCollision {
	/** 衝突が起きた {@link StructAllocatedRow} の配列内インデックス（0始まり）。 */
	index: number;
	kind:
		| 'field-address-overlap'
		| 'existing-address-overlap'
		| 'field-name-duplicate'
		| 'existing-name-duplicate';
	message: string;
}

/**
 * 生成済みの割付行に対する衝突検出。
 *
 * - **フィールド間のアドレス重なり**（`field-address-overlap`）: 同一構造体
 *   内で2フィールドの占有が重なる場合（手動割付での入力ミス、または自動割付
 *   ロジック自体のバグを検出する安全網）。
 * - **既存タグとの重なり**（`existing-address-overlap`）: **同じ収集グループ**
 *   内の既存タグと占有が重なる場合。グループを跨いだ重なり（同じ PLC でも別の
 *   グループ）は検出しない（タグ名の一意性がグループ内で閉じているのと同じ設計、
 *   `tagDuplicate.ts` の既存判断を踏襲）。
 * - **名前の衝突**: 構造体内でのフィールド名重複（`field-name-duplicate`）
 *   と、同じ収集グループ内の既存タグ名との重複（`existing-name-duplicate`）
 *   の両方を検出する（タグ名はグループ内一意 — サーバー側検証と同じ前提）。
 *
 * 占有の重なりの規則は `addressSpan.ts::spansOverlap`（同じ `area` の
 * `[index, index + wordCount - 1]`。ワード内ビットはワードを占める。`index` が無ければ
 * `canonical` の一致だけ）。戻り値は行ごとの衝突一覧（空配列 = 衝突なし）。1行に複数種類の
 * 衝突があれば複数エントリを返す。
 */
export function detectStructAddressCollisions(
	rows: StructAllocatedRow[],
	existingTags: ExistingTagForCollision[],
	collectionGroupId: number
): StructCollision[] {
	const collisions: StructCollision[] = [];

	const groupExisting = existingTags.filter((t) => t.collectionGroupId === collectionGroupId);
	const existingByName = new Map(groupExisting.map((t) => [t.name, t]));

	const firstSeenAt = new Map<string, number>();

	rows.forEach((row, index) => {
		if (firstSeenAt.has(row.name)) {
			collisions.push({
				index,
				kind: 'field-name-duplicate',
				message: `フィールド名 "${row.name}" が構造体内で重複しています（${firstSeenAt.get(row.name)! + 1}行目と同じ名前）。`
			});
		} else {
			firstSeenAt.set(row.name, index);
		}

		if (existingByName.has(row.name)) {
			collisions.push({
				index,
				kind: 'existing-name-duplicate',
				message: `タグ名 "${row.name}" は同じ収集グループに既に登録されています。`
			});
		}

		for (let other = 0; other < rows.length; other++) {
			if (other === index) continue;
			const otherRow = rows[other];
			if (spansOverlap(row.span, otherRow.span)) {
				collisions.push({
					index,
					kind: 'field-address-overlap',
					message: `アドレス ${row.address}（${row.span.wordCount}word占有）が ${other + 1}行目「${otherRow.name}」（${otherRow.address}、${otherRow.span.wordCount}word占有）と重なっています。`
				});
				break;
			}
		}

		for (const existing of groupExisting) {
			if (existing.span && spansOverlap(row.span, existing.span)) {
				collisions.push({
					index,
					kind: 'existing-address-overlap',
					message: `アドレス ${row.address}（${row.span.wordCount}word占有）が既存タグ「${existing.name}」と重なっています。`
				});
				break;
			}
		}
	});

	return collisions;
}

/** {@link structRowsToTagInputs} が受け取る、構造体全体で共通の設定値。 */
export interface StructCommonSettings {
	collectionGroupId: number;
	enabled: boolean;
	writable: boolean;
}

/**
 * 割付結果を `POST /api/tags/batch`（{@link createTagsBatch}）に渡せる
 * {@link TagInput} 配列へ変換する。連続登録と同じく、スケーリング・
 * しきい値はここでは扱わず既定（未設定）のままにする — 構造体登録は
 * 「デバイス自動割付・手動割付」に絞ったスライスのため（設計 §3.2 の
 * フィールド定義自体が名前・型・stringLength・address のみを持つ）。
 */
export function structRowsToTagInputs(
	rows: StructAllocatedRow[],
	common: StructCommonSettings
): TagInput[] {
	return rows.map((row) => ({
		name: row.name,
		collectionGroupId: common.collectionGroupId,
		address: row.address,
		dataType: row.dataType,
		stringLength: row.dataType === 'string' ? row.stringLength : undefined,
		stringEncoding: row.dataType === 'string' ? (row.stringEncoding ?? undefined) : undefined,
		decimals: 0,
		enabled: common.enabled,
		writable: common.writable
	}));
}
