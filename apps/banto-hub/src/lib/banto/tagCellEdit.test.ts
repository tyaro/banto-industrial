/**
 * `tagCellEdit.ts`（T18-3e、docs/banto-hub-t18-design.md「T18-3e BantoGrid
 * セル編集/TSV貼付の接続」）に対するユニットテスト。`tagBulkOps.test.ts` と
 * 同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	applyTagCellOverrides,
	buildTagCellEditBatch,
	mergeTagCellEdits,
	normalizeTagCellValue,
	type TagCellEditInput
} from './tagCellEdit';
import type { Tag } from './tagRegistryAdmin';

function makeTag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: 'temp1',
		collectionGroupId: 10,
		address: 'D100',
		dataType: 'f32',
		stringLength: null,
		rawLo: 0,
		rawHi: 100,
		engLo: 0,
		engHi: 100,
		unit: '℃',
		decimals: 1,
		thresholdH: 90,
		thresholdHh: 95,
		thresholdL: 10,
		thresholdLl: 5,
		enabled: true,
		writable: true,
		tagKind: 'plc',
		expression: null,
		retain: false,
		revision: 3,
		...overrides
	};
}

describe('normalizeTagCellValue', () => {
	it('unit: 空文字/null/undefined はすべて null に揃える', () => {
		expect(normalizeTagCellValue('unit', '')).toBeNull();
		expect(normalizeTagCellValue('unit', null)).toBeNull();
		expect(normalizeTagCellValue('unit', undefined)).toBeNull();
	});

	it('unit: 非空文字はそのまま文字列として保持する', () => {
		expect(normalizeTagCellValue('unit', '℃')).toBe('℃');
	});

	it('decimals: 数値はそのまま、非数値は 0 にフォールバックする', () => {
		expect(normalizeTagCellValue('decimals', 2)).toBe(2);
		expect(normalizeTagCellValue('decimals', '3')).toBe(3);
		expect(normalizeTagCellValue('decimals', NaN)).toBe(0);
	});

	it('enabled/writable: 真偽値へ揃える', () => {
		expect(normalizeTagCellValue('enabled', true)).toBe(true);
		expect(normalizeTagCellValue('writable', false)).toBe(false);
	});
});

describe('mergeTagCellEdits', () => {
	it('空配列なら空マップ', () => {
		expect(mergeTagCellEdits([]).size).toBe(0);
	});

	it('同一 id・同一フィールドへの複数編集は最後の値が勝つ', () => {
		const edits: TagCellEditInput[] = [
			{ id: 1, field: 'enabled', value: true },
			{ id: 1, field: 'enabled', value: false }
		];
		const merged = mergeTagCellEdits(edits);
		expect(merged.get(1)).toEqual({ enabled: false });
	});

	it('同一 id の異なるフィールドはそれぞれ保持する', () => {
		const edits: TagCellEditInput[] = [
			{ id: 1, field: 'enabled', value: false },
			{ id: 1, field: 'unit', value: 'kPa' },
			{ id: 1, field: 'decimals', value: 2 }
		];
		const merged = mergeTagCellEdits(edits);
		expect(merged.get(1)).toEqual({ enabled: false, unit: 'kPa', decimals: 2 });
	});

	it('複数行はそれぞれ独立したエントリになる', () => {
		const edits: TagCellEditInput[] = [
			{ id: 1, field: 'enabled', value: false },
			{ id: 2, field: 'writable', value: true }
		];
		const merged = mergeTagCellEdits(edits);
		expect(merged.get(1)).toEqual({ enabled: false });
		expect(merged.get(2)).toEqual({ writable: true });
	});

	it('unit の空文字編集は null へ正規化してマージする', () => {
		const merged = mergeTagCellEdits([{ id: 1, field: 'unit', value: '' }]);
		expect(merged.get(1)).toEqual({ unit: null });
	});
});

describe('applyTagCellOverrides', () => {
	it('overrides が無ければ同じ tag を返す', () => {
		const tag = makeTag();
		expect(applyTagCellOverrides(tag, undefined)).toBe(tag);
	});

	it('overrides のフィールドだけを上書きした新しいオブジェクトを返す', () => {
		const tag = makeTag({ enabled: true, unit: '℃' });
		const next = applyTagCellOverrides(tag, { enabled: false });
		expect(next).not.toBe(tag);
		expect(next.enabled).toBe(false);
		expect(next.unit).toBe('℃');
		expect(tag.enabled).toBe(true); // 元の tag は変更しない
	});
});

describe('buildTagCellEditBatch', () => {
	it('編集が無ければ空の rows/diffRows', () => {
		const result = buildTagCellEditBatch([], [makeTag()]);
		expect(result.rows).toEqual([]);
		expect(result.diffRows).toEqual([]);
	});

	it('単一セル編集: 1フィールドだけ変更した行を作る', () => {
		const tag = makeTag({ id: 5, enabled: true, revision: 7 });
		const edits: TagCellEditInput[] = [{ id: 5, field: 'enabled', value: false }];
		const result = buildTagCellEditBatch(edits, [tag]);

		expect(result.rows).toHaveLength(1);
		const [row] = result.rows;
		expect(row.id).toBe(5);
		expect(row.expectedRevision).toBe(7);
		expect(row.enabled).toBe(false);
		// 変更していないフィールドはそのまま引き継ぐ。
		expect(row.writable).toBe(tag.writable);
		expect(row.unit).toBe(tag.unit);
		expect(row.decimals).toBe(tag.decimals);
		expect(row.name).toBe(tag.name);
		expect(row.address).toBe(tag.address);

		expect(result.diffRows).toEqual([
			{
				id: 5,
				name: tag.name,
				diffs: [{ key: 'enabled', label: '有効', local: 'オン', server: 'オフ' }]
			}
		]);
	});

	it('複数セル編集（同一行）: 変更フィールドをまとめて1行にする', () => {
		const tag = makeTag({ id: 5, enabled: true, writable: false, unit: '℃', decimals: 1 });
		const edits: TagCellEditInput[] = [
			{ id: 5, field: 'enabled', value: false },
			{ id: 5, field: 'writable', value: true },
			{ id: 5, field: 'unit', value: 'kPa' },
			{ id: 5, field: 'decimals', value: 3 }
		];
		const result = buildTagCellEditBatch(edits, [tag]);

		expect(result.rows).toHaveLength(1);
		const [row] = result.rows;
		expect(row.enabled).toBe(false);
		expect(row.writable).toBe(true);
		expect(row.unit).toBe('kPa');
		expect(row.decimals).toBe(3);

		expect(result.diffRows[0].diffs.map((d) => d.key).sort()).toEqual([
			'decimals',
			'enabled',
			'unit',
			'writable'
		]);
	});

	it('同一セルへの複数回編集は最後の値が勝つ', () => {
		const tag = makeTag({ id: 5, decimals: 0 });
		const edits: TagCellEditInput[] = [
			{ id: 5, field: 'decimals', value: 1 },
			{ id: 5, field: 'decimals', value: 4 }
		];
		const result = buildTagCellEditBatch(edits, [tag]);
		expect(result.rows[0].decimals).toBe(4);
	});

	it('元の値と同じ値へ戻す編集は無変更として除外する', () => {
		const tag = makeTag({ id: 5, enabled: true });
		const edits: TagCellEditInput[] = [
			{ id: 5, field: 'enabled', value: false },
			{ id: 5, field: 'enabled', value: true } // 元に戻した
		];
		const result = buildTagCellEditBatch(edits, [tag]);
		expect(result.rows).toEqual([]);
		expect(result.diffRows).toEqual([]);
	});

	it('unit: 未設定タグへの空文字編集（誤検出防止）は無変更として除外する', () => {
		const tag = makeTag({ id: 5, unit: null });
		const edits: TagCellEditInput[] = [{ id: 5, field: 'unit', value: '' }];
		const result = buildTagCellEditBatch(edits, [tag]);
		expect(result.rows).toEqual([]);
		expect(result.diffRows).toEqual([]);
	});

	it('unit: 実際に空へ変更した場合は null への変更として送る', () => {
		const tag = makeTag({ id: 5, unit: '℃' });
		const edits: TagCellEditInput[] = [{ id: 5, field: 'unit', value: '' }];
		const result = buildTagCellEditBatch(edits, [tag]);
		expect(result.rows).toHaveLength(1);
		expect(result.rows[0].unit).toBeNull();
	});

	it('複数行の編集をそれぞれ独立した行として組み立てる', () => {
		const tags = [
			makeTag({ id: 1, revision: 1, enabled: true }),
			makeTag({ id: 2, revision: 9, writable: false })
		];
		const edits: TagCellEditInput[] = [
			{ id: 1, field: 'enabled', value: false },
			{ id: 2, field: 'writable', value: true }
		];
		const result = buildTagCellEditBatch(edits, tags);
		expect(result.rows.map((r) => [r.id, r.expectedRevision])).toEqual([
			[1, 1],
			[2, 9]
		]);
		expect(result.rows[0].enabled).toBe(false);
		expect(result.rows[1].writable).toBe(true);
	});

	it('削除済み等で対応する tag が見つからない id は静かにスキップする', () => {
		const tag = makeTag({ id: 1 });
		const edits: TagCellEditInput[] = [
			{ id: 1, field: 'enabled', value: false },
			{ id: 999, field: 'enabled', value: false }
		];
		const result = buildTagCellEditBatch(edits, [tag]);
		expect(result.rows).toHaveLength(1);
		expect(result.rows[0].id).toBe(1);
	});
});
