/**
 * T20 機能②a 構造体タグ登録（docs/banto-hub-t20-design.md §3.2、2026-09-04
 * オーナー決定「テンプレートの永続保存は不要」「自動割付・手動割付の両方を
 * 実装」）。
 *
 * 連続登録（`continuousRegistration.ts`）が「1タグをN連番」で複製するのに
 * 対し、構造体登録は「複数の異なるフィールド」を1つのベースアドレスから
 * 連続したワード領域へ割り付ける。アドレス算術そのもの（{@link
 * addressIncrement}/{@link incrementAddress}）は連続登録の実装を再利用し、
 * このファイルは「フィールドごとの累積ワードオフセット」「手動割付」
 * 「衝突検出（フィールド間・既存タグ・名前）」だけを新規に足す。
 *
 * 依存なしの純関数群 — `continuousRegistration.ts`/`tagRegistryAdmin.ts` と
 * 同じ流儀で、フォーム（`+page.svelte`）から状態を持ち込まずテスト可能に
 * してある。単体テストは `structRegistration.test.ts`
 * （vitest、`continuousRegistration.test.ts` と同じ describe/it スタイル）。
 */
import type { TagDataType, TagInput } from './tagRegistryAdmin';
import { addressIncrement, incrementAddress } from './continuousRegistration';
import { parseSlmpAddress } from './slmpDeviceTable';

/** 構造体の1フィールド定義。`address` は手動割付モードでのみ使う。 */
export interface StructField {
	name: string;
	dataType: TagDataType;
	stringLength?: number | null;
	/** 手動割付モードでのみ参照する。自動割付モードでは無視される。 */
	address?: string;
}

/**
 * 割付結果の1行。プレビュー表示にも {@link createTagsBatch} 相当への変換
 * （{@link structRowsToTagInputs}）にも使う共通の中間表現。`words` は
 * このフィールドが占有するワード数（{@link addressIncrement} の値そのもの）
 * で、衝突検出（{@link detectStructAddressCollisions}）が占有範囲の算出に
 * 使う。
 */
export interface StructAllocatedRow {
	name: string;
	address: string;
	dataType: TagDataType;
	stringLength?: number | null;
	words: number;
}

export type StructAllocationResult =
	{ ok: true; rows: StructAllocatedRow[] } | { ok: false; error: string };

function validateCommonFieldShape(fields: StructField[], index: number): string | null {
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
	return null;
}

/**
 * 自動割付: `baseAddress` から、フィールドを順にワードサイズ考慮の連続
 * アドレスへ割り付ける。フィールド i のアドレスは `incrementAddress(base,
 * 1, cumulativeWords_i)` — `cumulativeWords_0 = 0`、`cumulativeWords_i =
 * cumulativeWords_{i-1} + addressIncrement(field_{i-1}.dataType,
 * field_{i-1}.stringLength)`。つまり各フィールドが占有するワード数だけ
 * 次をずらす（docs/banto-hub-t20-design.md §3.2 実装指示のとおり）。
 *
 * - `baseAddress` はビットサフィックスを持たないワードアドレス
 *   （例 `D3000`）が前提 — `.N` 付きが渡されたらエラーを返す（bit
 *   フィールドの自動割付を本スライスでは扱わないため）。
 * - bit フィールドは `addressIncrement('bit')` が返す1ワードをそのまま
 *   占有する（他の型と同じ「+1ワード」の分岐 — ビット位置のパッキングは
 *   行わない。設計「bit フィールドは1ワード占有として素直に割り付ける」）。
 * - `incrementAddress` が `null`（{@link MAX_DEVICE_NUMBER} 超過、または
 *   `baseAddress` の形式が未対応）を返したらエラー行として打ち切る。
 */
export function allocateStructFields(
	baseAddress: string,
	fields: StructField[]
): StructAllocationResult {
	const trimmedBase = baseAddress.trim();
	if (trimmedBase === '') {
		return { ok: false, error: 'ベースアドレスを入力してください。' };
	}
	const parsedBase = parseSlmpAddress(trimmedBase);
	if (parsedBase?.bit !== undefined) {
		return {
			ok: false,
			error:
				'ベースアドレスにビット指定（例: D100.5）は使えません。ワードアドレス（例: D100）を指定してください。'
		};
	}
	if (fields.length === 0) {
		return { ok: false, error: 'フィールドを1つ以上追加してください。' };
	}

	const rows: StructAllocatedRow[] = [];
	let cumulativeWords = 0;
	for (let i = 0; i < fields.length; i++) {
		const field = fields[i];
		const shapeError = validateCommonFieldShape(fields, i);
		if (shapeError) return { ok: false, error: shapeError };

		const words = addressIncrement(field.dataType, field.stringLength);
		const address = incrementAddress(trimmedBase, 1, cumulativeWords);
		if (address === null) {
			return {
				ok: false,
				error: `${i + 1}行目（${field.name}）のアドレスを算出できません（ベースアドレス "${trimmedBase}" の形式が未対応か、デバイス番号の上限（0x00FFFFFF）を超えています）。`
			};
		}
		rows.push({
			name: field.name,
			address,
			dataType: field.dataType,
			stringLength: field.dataType === 'string' ? field.stringLength : undefined,
			words
		});
		cumulativeWords += words;
	}

	return { ok: true, rows };
}

/**
 * 手動割付: 各フィールドが明示的に持つ `address` をそのまま使う
 * （自動割付のようなアドレス算術は行わない）。
 */
export function manualStructRows(fields: StructField[]): StructAllocationResult {
	if (fields.length === 0) {
		return { ok: false, error: 'フィールドを1つ以上追加してください。' };
	}

	const rows: StructAllocatedRow[] = [];
	for (let i = 0; i < fields.length; i++) {
		const field = fields[i];
		const shapeError = validateCommonFieldShape(fields, i);
		if (shapeError) return { ok: false, error: shapeError };

		const address = field.address?.trim();
		if (!address) {
			return { ok: false, error: `${i + 1}行目（${field.name}）のアドレスを入力してください。` };
		}
		rows.push({
			name: field.name,
			address,
			dataType: field.dataType,
			stringLength: field.dataType === 'string' ? field.stringLength : undefined,
			words: addressIncrement(field.dataType, field.stringLength)
		});
	}

	return { ok: true, rows };
}

/** アドレスが占有するワード範囲。衝突検出のためだけの内部表現。 */
interface AddressWordRange {
	/** SLMP デバイスニーモニック、または非SLMP形式の先頭非数字プレフィックス（大文字化）。 */
	readonly axis: string;
	readonly start: number;
	/** 含む（inclusive）。 */
	readonly end: number;
}

/**
 * アドレス文字列と占有ワード数から比較用の範囲を組み立てる。
 *
 * - SLMP デバイス記法として解釈できれば、デバイスニーモニックを軸に
 *   `[number, number + words - 1]`。bit サフィックス（`.N`）が付いていても
 *   ビット位置は無視し、そのワード自体を1ワード占有として扱う（構造体の
 *   衝突検出はワード単位まで — 同一ワード内の異なるビット位置同士は
 *   本実装では「重なる」と判定する。設計が要求する粒度を超える誤検出を
 *   避けるより、見落とし（誤って重複を許す）を避ける方を優先した）。
 * - 解釈できない形式（Modbus 参照番号等）は `incrementAddress` と同じ
 *   フォールバック正規表現（先頭の非数字列 + 末尾の10進数字列）で軸と
 *   開始番号を取り出す。
 * - どちらの形にも合わなければ `null`（衝突判定の対象外 — 呼び出し側は
 *   その行を「範囲比較はできないが単純一致は見る」等の追加処理をしない。
 *   本実装では単に衝突なし扱いにする）。
 */
function addressWordRange(address: string, words: number): AddressWordRange | null {
	const trimmed = address.trim();
	const span = Math.max(1, words);
	const parsed = parseSlmpAddress(trimmed);
	if (parsed) {
		return { axis: parsed.mnemonic, start: parsed.number, end: parsed.number + span - 1 };
	}
	const match = /^(\D*)(\d+)$/.exec(trimmed);
	if (!match) return null;
	const [, prefix, digits] = match;
	const start = Number.parseInt(digits, 10);
	if (!Number.isFinite(start)) return null;
	return { axis: prefix.toUpperCase(), start, end: start + span - 1 };
}

function rangesOverlap(a: AddressWordRange, b: AddressWordRange): boolean {
	return a.axis === b.axis && a.start <= b.end && b.start <= a.end;
}

/** {@link detectStructAddressCollisions} が既存タグ側に要求する最小限の形。 */
export interface ExistingTagForCollision {
	name: string;
	collectionGroupId: number;
	address: string;
	dataType: TagDataType;
	stringLength?: number | null;
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
 *   内で2フィールドの占有ワード範囲が重なる場合（手動割付での入力ミス、
 *   または自動割付ロジック自体のバグを検出する安全網）。
 * - **既存タグとの重なり**（`existing-address-overlap`）: 同じ収集グループ
 *   内の既存タグと占有ワード範囲が重なる場合。グループを跨いだ重なりは
 *   検出しない（タグ名の一意性がグループ内で閉じているのと同じ設計、
 *   `tagDuplicate.ts` の既存判断を踏襲）。
 * - **名前の衝突**: 構造体内でのフィールド名重複（`field-name-duplicate`）
 *   と、同じ収集グループ内の既存タグ名との重複（`existing-name-duplicate`）
 *   の両方を検出する（タグ名はグループ内一意 — サーバー側検証と同じ前提）。
 *
 * 戻り値は行ごとの衝突一覧（空配列 = 衝突なし）。1行に複数種類の衝突が
 * あれば複数エントリを返す。
 */
export function detectStructAddressCollisions(
	rows: StructAllocatedRow[],
	existingTags: ExistingTagForCollision[],
	collectionGroupId: number
): StructCollision[] {
	const collisions: StructCollision[] = [];

	const groupExisting = existingTags.filter((t) => t.collectionGroupId === collectionGroupId);
	const existingByName = new Map(groupExisting.map((t) => [t.name, t]));
	const existingRanges = groupExisting.map((t) => ({
		name: t.name,
		range: addressWordRange(t.address, addressIncrement(t.dataType, t.stringLength))
	}));

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

		const range = addressWordRange(row.address, row.words);
		if (!range) return;

		for (let other = 0; other < rows.length; other++) {
			if (other === index) continue;
			const otherRow = rows[other];
			const otherRange = addressWordRange(otherRow.address, otherRow.words);
			if (otherRange && rangesOverlap(range, otherRange)) {
				collisions.push({
					index,
					kind: 'field-address-overlap',
					message: `アドレス ${row.address}（${row.words}word占有）が ${other + 1}行目「${otherRow.name}」（${otherRow.address}、${otherRow.words}word占有）と重なっています。`
				});
				break;
			}
		}

		for (const existing of existingRanges) {
			if (existing.range && rangesOverlap(range, existing.range)) {
				collisions.push({
					index,
					kind: 'existing-address-overlap',
					message: `アドレス ${row.address}（${row.words}word占有）が既存タグ「${existing.name}」と重なっています。`
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
		decimals: 0,
		enabled: common.enabled,
		writable: common.writable
	}));
}
