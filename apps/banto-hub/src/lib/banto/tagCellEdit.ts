/**
 * T18-3e（docs/banto-hub-t18-design.md「T18-3e BantoGrid セル編集/TSV貼付の
 * 接続」、docs/banto-hub-desktop-plan.md §9.4 TAG-UX-D 後半）: BantoGrid の
 * `onCellEdit`/`onRangePaste`（`@banto/grid-svelte`、`CellEdit<Tag>`）が返す
 * 生のセル編集を、`updateTagsBatch`（`POST /api/tags/batch-update`）に渡せる
 * {@link BatchTagUpdateRow}[] へ変換する依存ゼロの純関数群。
 *
 * `tagBulkOps.ts`/`tagCsvDiff.ts` と同じ方針 - `+page.svelte` の保留編集
 * バッファ（`CellEdit` を集めた配列）と現行 `tags` から、実際に値が変わる
 * 行だけを組み立てる。テストは同ディレクトリの `tagCellEdit.test.ts`。
 *
 * **即保存しない設計**（実装指示 T18-3e）: `+page.svelte` は `onCellEdit`/
 * `onRangePaste` で受け取った生の編集をそのまま `TagCellEditInput[]` として
 * 保留バッファに溜めるだけで、ここではネットワーク呼び出しを一切行わない。
 * 「保存」操作時に {@link buildTagCellEditBatch} で行を組み立て、
 * `updateTagsBatch(rows, true)`（全構成 preflight）→ 差分確認 →
 * `updateTagsBatch(rows, false)`（all-or-nothing 適用）という2段フローに
 * 渡すのは呼び出し側の責務（`tagCsvDiff.ts::classifyCsvUpdate` と同じ役割
 * 分担）。
 *
 * **編集対象は4フィールドのみ**（`enabled`/`writable`/`unit`/`decimals`） -
 * アドレス・型・収集グループ・種別・名前・式は編集不可列のため、BantoGrid
 * 自体がそれらのセルを never editable にする（`editable` を付与しない）。
 */
import { diffFormRecords, type ConflictFieldDiff } from './tagConflictDiff';
import type { BatchTagUpdateRow, Tag, TagInput } from './tagRegistryAdmin';

/** グリッド上で編集可能にする4フィールド（実装指示「安全・軽量フィールドのみ」）。 */
export type EditableTagField = 'enabled' | 'writable' | 'unit' | 'decimals';

/**
 * BantoGrid の `CellEdit<Tag>`（`row`/`oldValue` は不要なので落とした最小形）
 * を、このページの保留バッファがそのまま溜める形。`id` は
 * `getRowId={(t) => t.id}` で渡している `Tag.id`（`CellEdit.rowId`）。
 */
export interface TagCellEditInput {
	id: number;
	field: EditableTagField;
	/** BantoGrid の editor 型で既にパース済みの値（checkbox→boolean, number→number, text→string）。 */
	value: unknown;
}

/** {@link mergeTagCellEdits}/{@link applyTagCellOverrides} が扱う、Tag 側の型に正規化済みの上書き値。 */
export type TagCellFieldValues = Partial<Pick<Tag, 'enabled' | 'writable' | 'unit' | 'decimals'>>;

/** 保存確認パネルの1行（対象タグ1件・変更フィールドの束）。 */
export interface TagCellEditRowDiff {
	id: number;
	name: string;
	/** `diffFormRecords` そのまま - `local`/`server` は既に日本語表示用に整形済み（「変更前」「変更後」に使える）。 */
	diffs: ConflictFieldDiff[];
}

export interface TagCellEditBatch {
	/** `updateTagsBatch` にそのまま渡せる行（実際に値が変わる行のみ）。 */
	rows: BatchTagUpdateRow[];
	/** `rows` と同じ順序・同じ対象タグ集合の、確認パネル表示用の差分。 */
	diffRows: TagCellEditRowDiff[];
}

const EDITABLE_FIELD_LABELS: Record<EditableTagField, string> = {
	enabled: '有効',
	writable: '書き込み可',
	unit: '単位',
	decimals: '小数桁数'
};

/**
 * グリッドの生の編集値を {@link Tag} 側の型へ正規化する。
 *
 * - `unit`: BantoGrid の text editor は「未入力」を `''` として返す
 *   （`Tag.unit` は `null`）。空文字・`null`・`undefined` はすべて
 *   `null`（未設定）に揃える - `toInput()`（`+page.svelte`）の
 *   `unit === '' ? undefined : unit` と同じ「空文字=未設定」の扱い。
 *   これを揃えないと、単位が未設定のタグをダブルクリックしただけで
 *   （何も入力せず blur しただけで）`''` への「変更」が誤検出される
 *   （BantoGrid は commit 前に `draft !== oldValue` だけを見るため、
 *   `null` と `''` が別値と判定される - 詳細は edit.ts の
 *   `prepareCommit`）。
 * - `decimals`: number editor は既に数値を返すが、念のため `Number()`
 *   で再変換する（`NaN` は 0 にフォールバック - 実際には
 *   `+page.svelte` の列 `validate` が非整数/範囲外を弾くため、ここに
 *   `NaN` が来るのは想定外パスのみ）。
 * - `enabled`/`writable`: checkbox editor は既に `boolean` を返すが、
 *   念のため `Boolean()` で揃える。
 */
export function normalizeTagCellValue(field: EditableTagField, value: unknown): unknown {
	if (field === 'unit') {
		if (value === '' || value === null || value === undefined) return null;
		return String(value);
	}
	if (field === 'decimals') {
		const n = typeof value === 'number' ? value : Number(value);
		return Number.isFinite(n) ? n : 0;
	}
	return Boolean(value);
}

/**
 * 保留バッファ（配列、入力順）を「行 id → フィールドごとの正規化済み上書き値」
 * のマップへ集約する。**同一 id・同一フィールドへの複数編集は最後の値が勝つ**
 * （実装指示「マージ（最後の値優先）」）- `Map` は挿入順を保持するため、
 * 配列を先頭から辿って上書きしていくだけで自然にこの規則になる。
 */
export function mergeTagCellEdits(
	edits: readonly TagCellEditInput[]
): Map<number, TagCellFieldValues> {
	const merged = new Map<number, TagCellFieldValues>();
	for (const edit of edits) {
		const current = merged.get(edit.id) ?? {};
		const normalized = normalizeTagCellValue(edit.field, edit.value);
		merged.set(edit.id, { ...current, [edit.field]: normalized } as TagCellFieldValues);
	}
	return merged;
}

/**
 * 表示用: `overrides` にある分だけ `tag` のフィールドを上書きした新しい
 * {@link Tag} を返す（`tag` 自体は変更しない）。保存前の保留編集をグリッドに
 * 反映するための、`+page.svelte` 側のローカル表示コピー生成に使う。
 * `overrides` が無ければ `tag` をそのまま返す。
 */
export function applyTagCellOverrides(tag: Tag, overrides: TagCellFieldValues | undefined): Tag {
	if (!overrides) return tag;
	return { ...tag, ...overrides };
}

/** {@link Tag} の全フィールドを {@link TagInput} へそのまま写す（`tagBulkOps.ts::tagToInput` と同じ変換）。 */
function tagToInput(tag: Tag): TagInput {
	return {
		name: tag.name,
		collectionGroupId: tag.collectionGroupId,
		address: tag.address,
		dataType: tag.dataType,
		stringLength: tag.stringLength,
		rawLo: tag.rawLo,
		rawHi: tag.rawHi,
		engLo: tag.engLo,
		engHi: tag.engHi,
		unit: tag.unit,
		decimals: tag.decimals,
		thresholdH: tag.thresholdH,
		thresholdHh: tag.thresholdHh,
		thresholdL: tag.thresholdL,
		thresholdLl: tag.thresholdLl,
		enabled: tag.enabled,
		writable: tag.writable,
		tagKind: tag.tagKind,
		expression: tag.expression,
		retain: tag.retain
	};
}

function applyDiffToInput(input: TagInput, field: EditableTagField, value: unknown): void {
	switch (field) {
		case 'enabled':
			input.enabled = value as boolean;
			break;
		case 'writable':
			input.writable = value as boolean;
			break;
		case 'unit':
			input.unit = value as string | null;
			break;
		case 'decimals':
			input.decimals = value as number;
			break;
	}
}

/**
 * 保留バッファ（`CellEdit` を集めた配列）と現行 `tags` から、実際に
 * `updateTagsBatch` へ送る行と、確認パネル用の差分を組み立てる。
 *
 * - 同一 id への複数セル編集は {@link mergeTagCellEdits} でマージ済み
 *   （最後の値優先）。
 * - 各行は `tag` の全フィールドを {@link tagToInput} で写した上で、
 *   実際に変わったフィールドだけを上書きする（`id`/`expectedRevision`
 *   は `tag.id`/`tag.revision`）。
 * - **元の値と正規化後の値が同じフィールドは上書きしない**、かつ
 *   **1フィールドも変わらない行は `rows`/`diffRows` に含めない**
 *   （実装指示「実際に値が変わった行のみ含める＝無変更無送信」）。
 * - バッファに載っている id が既に `tags` に存在しない（別経路で削除
 *   された等）場合は、その id を静かにスキップする - `+page.svelte`
 *   側は `reload()` のたびに保留バッファをそのまま持ち越すが、削除済み
 *   行に対して保存を試みても意味が無いため、エラー扱いにはしない。
 */
export function buildTagCellEditBatch(
	edits: readonly TagCellEditInput[],
	tags: readonly Tag[]
): TagCellEditBatch {
	const merged = mergeTagCellEdits(edits);
	const tagsById = new Map(tags.map((t) => [t.id, t] as const));

	const rows: BatchTagUpdateRow[] = [];
	const diffRows: TagCellEditRowDiff[] = [];

	for (const [id, overrides] of merged) {
		const tag = tagsById.get(id);
		if (!tag) continue;

		const before: Record<string, unknown> = {
			enabled: tag.enabled,
			writable: tag.writable,
			unit: tag.unit,
			decimals: tag.decimals
		};
		const after: Record<string, unknown> = { ...before, ...overrides };

		const diffs = diffFormRecords(before, after, EDITABLE_FIELD_LABELS);
		if (diffs.length === 0) continue;

		diffRows.push({ id, name: tag.name, diffs });

		const input = tagToInput(tag);
		for (const diff of diffs) {
			applyDiffToInput(input, diff.key as EditableTagField, after[diff.key]);
		}
		rows.push({ id, expectedRevision: tag.revision, ...input });
	}

	return { rows, diffRows };
}
