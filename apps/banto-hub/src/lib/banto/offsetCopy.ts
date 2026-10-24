/**
 * T20 機能②b オフセットコピー（docs/banto-hub-t20-design.md §3.2、
 * 2026-09-04 オーナー決定「テンプレートの永続保存はしないが、既存タグ群
 * （＝構造体1インスタンス）からのオフセットコピーは有用」）。
 *
 * 既に登録済みの複数タグ（画面のタグ一覧で複数選択したもの）を、アドレスに
 * 一定オフセット（ワード数）を加えて複製する。構造体登録②a
 * （`structRegistration.ts`）が「1つのベースアドレスから複数フィールドを
 * 新規に割り付ける」のに対し、こちらは「既存タグ群をまるごと平行移動して
 * 複製する」— 衝突検出は②a の {@link detectStructAddressCollisions} をそのまま
 * 再利用し、本ファイルは「コピー先の名前をどう決めるか」（{@link offsetCopyName}）と
 * 「複数selectionを一度に処理する」（{@link computeOffsetCopy}）だけを持つ。
 *
 * **R2b-6d-2a（docs/banto-hub-driver-ui-design.md §4.3・§7・§7.1、2026-10-24）: アドレスの
 * 計算はドライバに任せる**。コピー先のアドレスは `POST /api/tags/offset-addresses`
 * （項目ごとの接続・`unit: "words"`）が返す。**ワード内ビット（`D100.5`）は、ワードの部分だけ
 * 進めてビットを保つ**（以前はビットの軸で進めて `D100.6` になっていた。物理的なワードの
 * 変位なので、構造体を写してもビットの位置関係が崩れない）。ドライバが解釈できない
 * アドレス・範囲外は、推測せずそのタグの行に API のエラーを出す。
 * 計算は「REST の結果を受け取って行を組み立てる純関数（{@link assembleOffsetCopy}）」と
 * 「REST を呼ぶ薄い層（{@link computeOffsetCopy}）」に分けてある。
 *
 * ## 命名ルール（オーナー決定、2026-09-05）
 *
 * コピー先タグの名前は、元タグ名の性質に応じて**タグごとに適応的に**決める:
 *
 * - **アドレス由来**（{@link isDeviceNameBasedName}）: 名前が元のアドレスの正規形
 *   （保存されている形。例 アドレス `D3000` に対し名前 `D3000`/`d3000`）と、大文字小文字を
 *   無視して同じ場合 → コピー先の**新アドレスの正規形**を名前にする（例 +100 で
 *   `D3100`/`d3100`。元の大文字/小文字の流儀を踏襲）。この場合、コピー先の
 *   名前は「そのアドレスのデバイス名」という意味を保つことが利用者の直感に
 *   合うため、既存タグ名との衝突があっても**名前をずらして回避したりしない**
 *   — 衝突はそのまま {@link assembleOffsetCopy} のエラーとして報告し、
 *   利用者に判断させる（アドレス自体をずらすか、コピー元を見直すか）。
 * - **それ以外**（意味のある構造体名、例 `temp01`/`pressure`）の場合 →
 *   **名前の末尾に数字を付ける**形でコピーする（名前の規則でありアドレスの文法ではないので
 *   TS に残す）。末尾が既に数字なら+1した値を、末尾に数字が無ければ `2` から開始し、
 *   `taken`（既存タグ名＋この一括で既に採った名前）と衝突しない最初の値まで数字を進める
 *   （この branch だけが積極的な衝突回避を行う — 数字を進めるだけで簡単に回避できる上、
 *   構造体を大量複製する運用でコピーごとに手直しさせないため）。
 *
 * ## オフセットの扱い（2026-09-05 オーナー決定「正の整数のみ」）
 *
 * `offset` は **1以上の整数ワード数のみ**を仕様とする。0・負・非整数は
 * いずれも不正な入力として {@link assembleOffsetCopy} が**先頭で**弾き、
 * source を1件も処理せず、全 source を対象にした一律のエラー
 * （「オフセットは1以上の整数で指定してください。」）を返す（`rows` は空）。
 * UI 側（`+page.svelte` のオフセットコピーパネル）も `<input min="1">`
 * に加え、JS 側の disabled 条件・エラー表示でこの仕様を弾く。
 */
import {
	fetchExistingSpans,
	chunked,
	realAddressApi,
	spanOfParsed,
	type AddressApi
} from './addressSpan';
import { MAX_ADDRESS_ITEMS_PER_REQUEST } from './tagRegistryAdmin';
import {
	detectStructAddressCollisions,
	type ExistingTagForCollision,
	type StructAllocatedRow
} from './structRegistration';
import type {
	OffsetAddressItem,
	ParsedAddress,
	StringEncoding,
	Tag,
	TagDataType,
	TagInput
} from './tagRegistryAdmin';

/**
 * タグ名が、そのアドレスの正規形（`address`。保存されている形）と、前後の空白と大文字小文字を
 * 無視して一致するかどうか。どちらかが空なら `false`。
 */
export function isDeviceNameBasedName(name: string, address: string): boolean {
	const trimmedName = name.trim();
	const trimmedAddress = address.trim();
	if (trimmedName === '' || trimmedAddress === '') return false;
	return trimmedName.toUpperCase() === trimmedAddress.toUpperCase();
}

/** 名前の非数字部分がすべて小文字（＝大文字を1つも含まない）かどうか。 */
function isLowerCaseStyle(name: string): boolean {
	const trimmed = name.trim();
	return trimmed === trimmed.toLowerCase();
}

/**
 * 「末尾に数字を付ける」ブランチの本体。末尾が既に10進数字列なら+1した値を
 * （元の桁数を `padStart` で維持する - 例 `temp01` → `temp02`）、末尾に
 * 数字が無ければ `2` から開始し、`taken` と衝突しない最初の値まで数字を
 * 1ずつ進める。`taken` に生成した名前を追加する（呼び出し元が後続の
 * 呼び出しでも衝突を避け続けられるようにするための意図的な副作用）。
 */
function nextAvailableNumberedName(baseName: string, taken: Set<string>): string {
	const trimmed = baseName.trim();
	const match = /^(.*?)(\d+)$/.exec(trimmed);
	const prefix = match ? match[1] : trimmed;
	const digits = match ? match[2] : null;
	const width = digits ? digits.length : 0;
	let n = digits ? Number.parseInt(digits, 10) + 1 : 2;
	let candidate = `${prefix}${String(n).padStart(width, '0')}`;
	while (taken.has(candidate)) {
		n += 1;
		candidate = `${prefix}${String(n).padStart(width, '0')}`;
	}
	taken.add(candidate);
	return candidate;
}

/**
 * コピー先タグの名前を1件ぶん決める（ドキュメント冒頭「命名ルール」参照）。
 * `source.address` は保存されている正規形、`newAddress` はコピー先の正規形。
 *
 * `taken` は「衝突とみなす名前の集合」で、この関数は生成した名前を
 * **`taken` に追加する副作用を持つ**（同一バッチ内の後続呼び出しが、
 * この呼び出しで採った名前を衝突として認識できるようにするため）。
 * アドレス由来ブランチは `taken` を見て書き換えない（衝突回避のための
 * 書き換えを行わない設計）が、生成した名前は後続呼び出しのために `taken` へ追加する。
 */
export function offsetCopyName(
	source: { name: string; address: string },
	newAddress: string,
	taken: Set<string>
): string {
	if (isDeviceNameBasedName(source.name, source.address)) {
		const candidate = isLowerCaseStyle(source.name)
			? newAddress.toLowerCase()
			: newAddress.toUpperCase();
		taken.add(candidate);
		return candidate;
	}
	return nextAvailableNumberedName(source.name, taken);
}

/** {@link assembleOffsetCopy} が生成する1行分。プレビュー表示と
 * {@link offsetCopyRowsToTagInputs} への変換の両方に使う共通の中間表現。 */
export interface OffsetCopyRow {
	/** コピー元タグの id（プレビューでの対応付け・エラーとの突き合わせに使う）。 */
	sourceId: number;
	sourceName: string;
	sourceAddress: string;
	name: string;
	address: string;
	collectionGroupId: number;
	dataType: TagDataType;
	stringLength?: number | null;
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

/** {@link assembleOffsetCopy} が返す1件分のエラー。`sourceId` で対応する
 * {@link OffsetCopyRow}（存在すれば）と突き合わせられる - アドレス算出
 * 自体に失敗した場合は対応する行が無い。 */
export interface OffsetCopyError {
	sourceId: number;
	sourceName: string;
	sourceAddress: string;
	message: string;
}

export interface OffsetCopyResult {
	rows: OffsetCopyRow[];
	errors: OffsetCopyError[];
}

/**
 * 選択済みタグ群 `sources` を、アドレスに `offset` ワード分を加えて複製するための行を組み立てる
 * 純関数。`moved[i]` は `sources[i]` を `offset` ワード動かした結果（`offset-addresses`、
 * `unit: "words"`。`undefined` は結果が無い = 算出できない）。
 *
 * - `ok: false` の source は対応する行を作らずエラー（API のメッセージを添える）。
 * - `newName = offsetCopyName(source, newAddress, taken)` - `taken` は
 *   コピー先と同じ収集グループ（`source.collectionGroupId`）内の既存タグ名
 *   ＋この一括で既に採った名前（`existingNames`。グループ単位でスコープする）。
 * - コピー先グループは source と同じ `collectionGroupId`。型・
 *   `stringLength`・`writable` に加え、単位・スケーリング（raw/eng）も source から
 *   引き継ぐ（複製は名前とアドレスのみ変更、他は引き継ぐ）。
 * - **衝突検出**: 生成した行を収集グループ単位でまとめ、②aの
 *   {@link detectStructAddressCollisions} をそのまま再利用してアドレスの占有の重なり・
 *   名前の重複（グループ内の既存タグとの重複、およびこのバッチ内の行同士の重複）を検出する。
 *   `existing` は既存タグの名前と占有（`span` が `null` のタグは名前だけ見る）。検出された行は
 *   `rows` には残したまま（プレビュー表に「行として」出し、警告を添えられるように）、`errors` に追記する。
 */
export function assembleOffsetCopy(
	sources: Tag[],
	offset: number,
	moved: (ParsedAddress | undefined)[],
	existing: ExistingTagForCollision[]
): OffsetCopyResult {
	const rows: OffsetCopyRow[] = [];
	const errors: OffsetCopyError[] = [];

	if (!Number.isInteger(offset) || offset < 1) {
		for (const source of sources) {
			errors.push({
				sourceId: source.id,
				sourceName: source.name,
				sourceAddress: source.address,
				message: 'オフセットは1以上の整数で指定してください。'
			});
		}
		return { rows, errors };
	}

	const takenByGroup = new Map<number, Set<string>>();
	function takenSetFor(groupId: number): Set<string> {
		let set = takenByGroup.get(groupId);
		if (!set) {
			set = new Set(existing.filter((t) => t.collectionGroupId === groupId).map((t) => t.name));
			takenByGroup.set(groupId, set);
		}
		return set;
	}

	const spans: StructAllocatedRow['span'][] = [];
	sources.forEach((source, i) => {
		const result = moved[i];
		if (!result?.ok) {
			errors.push({
				sourceId: source.id,
				sourceName: source.name,
				sourceAddress: source.address,
				message: `アドレス "${source.address}" から +${offset} ワード先のアドレスを算出できません: ${result && !result.ok ? result.message : '算出結果が返りませんでした'}`
			});
			return;
		}
		const newAddress = result.canonical;
		const taken = takenSetFor(source.collectionGroupId);
		const name = offsetCopyName(source, newAddress, taken);

		spans.push(spanOfParsed(result)!);
		rows.push({
			sourceId: source.id,
			sourceName: source.name,
			sourceAddress: source.address,
			name,
			address: newAddress,
			collectionGroupId: source.collectionGroupId,
			dataType: source.dataType,
			stringLength: source.dataType === 'string' ? source.stringLength : undefined,
			stringEncoding: source.dataType === 'string' ? source.stringEncoding : undefined,
			unit: source.unit,
			decimals: source.decimals,
			rawLo: source.rawLo,
			rawHi: source.rawHi,
			engLo: source.engLo,
			engHi: source.engHi,
			enabled: source.enabled,
			writable: source.writable
		});
	});

	// 衝突検出は収集グループ単位（detectStructAddressCollisions の契約）。
	const rowIndicesByGroup = new Map<number, number[]>();
	rows.forEach((row, index) => {
		const list = rowIndicesByGroup.get(row.collectionGroupId);
		if (list) {
			list.push(index);
		} else {
			rowIndicesByGroup.set(row.collectionGroupId, [index]);
		}
	});

	for (const [groupId, indices] of rowIndicesByGroup) {
		const groupRows: StructAllocatedRow[] = indices.map((i) => {
			const row = rows[i];
			return {
				name: row.name,
				address: row.address,
				dataType: row.dataType,
				stringLength: row.stringLength,
				span: spans[i]
			};
		});
		const collisions = detectStructAddressCollisions(groupRows, existing, groupId);
		for (const collision of collisions) {
			const row = rows[indices[collision.index]];
			errors.push({
				sourceId: row.sourceId,
				sourceName: row.sourceName,
				sourceAddress: row.sourceAddress,
				message: collision.message
			});
		}
	}

	return { rows, errors };
}

function failedAll(sources: Tag[], what: string, error: unknown): OffsetCopyResult {
	const message = error instanceof Error ? error.message : String(error);
	return {
		rows: [],
		errors: sources.map((source) => ({
			sourceId: source.id,
			sourceName: source.name,
			sourceAddress: source.address,
			message: `${what}に失敗しました（${message}）。`
		}))
	};
}

/**
 * {@link assembleOffsetCopy} の前に REST を呼ぶ薄い層。コピー元の全件を 1 回（上限を超えるときは
 * 分けて）`offset-addresses` で動かし、衝突検出のために既存タグ（対象のグループのもの）の占有を
 * `parse-addresses` で引く。`connectionIdForGroup` は収集グループから接続の id を引く関数
 * （無ければそのタグだけエラー）。通信の失敗は、衝突なしに倒さず全件のエラーにする。
 */
export async function computeOffsetCopy(
	sources: Tag[],
	offset: number,
	existingTags: Tag[],
	connectionIdForGroup: (collectionGroupId: number) => number | undefined,
	api: AddressApi = realAddressApi
): Promise<OffsetCopyResult> {
	if (!Number.isInteger(offset) || offset < 1) {
		return assembleOffsetCopy(sources, offset, [], []);
	}

	const moved: (ParsedAddress | undefined)[] = sources.map(() => undefined);
	const requestIndices: number[] = [];
	const items: OffsetAddressItem[] = [];
	sources.forEach((source, i) => {
		const connectionId = connectionIdForGroup(source.collectionGroupId);
		if (connectionId === undefined) {
			moved[i] = { ok: false, message: '接続が見つかりません' };
			return;
		}
		requestIndices.push(i);
		items.push({
			connectionId,
			tagKind: source.tagKind,
			address: source.address,
			dataType: source.dataType,
			stringLength: source.stringLength,
			stringEncoding: source.stringEncoding,
			n: offset,
			unit: 'words'
		});
	});

	try {
		let cursor = 0;
		for (const part of chunked(items, MAX_ADDRESS_ITEMS_PER_REQUEST)) {
			const results = await api.offset(part);
			results.forEach((result, k) => {
				moved[requestIndices[cursor + k]] = result;
			});
			cursor += part.length;
		}
	} catch (error) {
		return failedAll(sources, 'アドレスの算出', error);
	}

	const groupIds = new Set(sources.map((s) => s.collectionGroupId));
	const targetExisting = existingTags.filter((t) => groupIds.has(t.collectionGroupId));
	let existing: ExistingTagForCollision[];
	try {
		const spans = await fetchExistingSpans(targetExisting, connectionIdForGroup, api);
		existing = targetExisting.map((t) => ({
			name: t.name,
			collectionGroupId: t.collectionGroupId,
			span: spans.get(t.id) ?? null
		}));
	} catch (error) {
		return failedAll(sources, '既存タグのアドレスの確認', error);
	}
	return assembleOffsetCopy(sources, offset, moved, existing);
}

/**
 * {@link assembleOffsetCopy} の行を `POST /api/tags/batch`
 * （{@link createTagsBatch}）に渡せる {@link TagInput} 配列へ変換する。
 * 構造体登録②aの `structRowsToTagInputs` とは異なり、単位・スケーリング・
 * しきい値も引き継ぐ（上の doc コメント参照）。
 */
export function offsetCopyRowsToTagInputs(rows: OffsetCopyRow[]): TagInput[] {
	return rows.map((row) => ({
		name: row.name,
		collectionGroupId: row.collectionGroupId,
		address: row.address,
		dataType: row.dataType,
		stringLength: row.stringLength,
		stringEncoding: row.stringEncoding ?? undefined,
		unit: row.unit,
		decimals: row.decimals,
		rawLo: row.rawLo,
		rawHi: row.rawHi,
		engLo: row.engLo,
		engHi: row.engHi,
		enabled: row.enabled,
		writable: row.writable
	}));
}
