/**
 * `tagBulkOps.ts`（T18-3b、docs/banto-hub-t18-design.md「T18-3b 一括操作」）
 * に対するユニットテスト。`tagDuplicate.test.ts`/`tagCsv.test.ts` と同じ
 * スタイル（describe/it、依存ゼロの純関数を直接 import）。
 */
import { describe, expect, it } from 'vitest';
import {
	buildBulkEnableRows,
	buildBulkMoveRows,
	formatTagsBulkDeleteConfirmMessage,
	hasMixedTagKinds,
	summarizeBulkChange
} from './tagBulkOps';
import type { Tag } from './tagRegistryAdmin';

function makeTag(overrides: Partial<Tag> = {}): Tag {
	return {
		id: 1,
		name: 'temp1',
		collectionGroupId: 10,
		address: 'D100',
		dataType: 'f32',
		stringLength: null,
		stringEncoding: 'utf8',
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

describe('buildBulkEnableRows', () => {
	it('空選択なら空配列を返す', () => {
		expect(buildBulkEnableRows([], true)).toEqual([]);
	});

	it('enabled だけを差し替え、他の全フィールドを引き継ぐ', () => {
		const tag = makeTag({ id: 5, enabled: false, revision: 7 });
		const [row] = buildBulkEnableRows([tag], true);
		expect(row).toEqual({
			id: 5,
			expectedRevision: 7,
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
			enabled: true,
			writable: tag.writable,
			tagKind: tag.tagKind,
			expression: tag.expression,
			retain: tag.retain
		});
	});

	it('複数選択それぞれに id/expectedRevision を個別に付ける', () => {
		const tags = [
			makeTag({ id: 1, revision: 1 }),
			makeTag({ id: 2, revision: 9 }),
			makeTag({ id: 3, revision: 0 })
		];
		const rows = buildBulkEnableRows(tags, false);
		expect(rows.map((r) => [r.id, r.expectedRevision])).toEqual([
			[1, 1],
			[2, 9],
			[3, 0]
		]);
		expect(rows.every((r) => r.enabled === false)).toBe(true);
	});

	it('computed/internal タグでも writable/expression/retain をそのまま引き継ぐ（強制しない）', () => {
		const computed = makeTag({
			id: 8,
			tagKind: 'computed',
			writable: false,
			expression: '1+1',
			address: ''
		});
		const internal = makeTag({ id: 9, tagKind: 'internal', address: '', retain: true });
		const rows = buildBulkEnableRows([computed, internal], true);
		expect(rows[0].expression).toBe('1+1');
		expect(rows[0].address).toBe('');
		expect(rows[1].retain).toBe(true);
	});
});

describe('buildBulkMoveRows', () => {
	it('空選択なら空配列を返す', () => {
		expect(buildBulkMoveRows([], 99)).toEqual([]);
	});

	it('collectionGroupId だけを差し替え、他の全フィールドを引き継ぐ', () => {
		const tag = makeTag({ id: 5, collectionGroupId: 10, revision: 2, enabled: false });
		const [row] = buildBulkMoveRows([tag], 42);
		expect(row.collectionGroupId).toBe(42);
		expect(row.id).toBe(5);
		expect(row.expectedRevision).toBe(2);
		// enabled 等、移動対象外のフィールドは変化しない。
		expect(row.enabled).toBe(false);
		expect(row.name).toBe(tag.name);
		expect(row.address).toBe(tag.address);
	});

	it('複数選択それぞれに同じ移動先グループIDを設定する', () => {
		const tags = [
			makeTag({ id: 1, collectionGroupId: 10 }),
			makeTag({ id: 2, collectionGroupId: 20 })
		];
		const rows = buildBulkMoveRows(tags, 30);
		expect(rows.map((r) => r.collectionGroupId)).toEqual([30, 30]);
	});
});

describe('hasMixedTagKinds', () => {
	it('空選択は混在ではない', () => {
		expect(hasMixedTagKinds([])).toBe(false);
	});

	it('全件同じ種別なら混在ではない', () => {
		expect(hasMixedTagKinds([makeTag({ tagKind: 'plc' }), makeTag({ tagKind: 'plc' })])).toBe(
			false
		);
	});

	it('種別が異なる行が混ざっていれば混在と判定する', () => {
		expect(hasMixedTagKinds([makeTag({ tagKind: 'plc' }), makeTag({ tagKind: 'computed' })])).toBe(
			true
		);
		expect(
			hasMixedTagKinds([
				makeTag({ tagKind: 'plc' }),
				makeTag({ tagKind: 'computed' }),
				makeTag({ tagKind: 'internal' })
			])
		).toBe(true);
	});
});

describe('summarizeBulkChange', () => {
	it('空選択なら対象0件・変更0件', () => {
		const summary = summarizeBulkChange([], 'enabled', true);
		expect(summary).toEqual({ targetCount: 0, changedCount: 0, rows: [] });
	});

	it('enabled: 既に目的値と一致する行は changed=false になる', () => {
		const tags = [makeTag({ id: 1, enabled: true }), makeTag({ id: 2, enabled: false })];
		const summary = summarizeBulkChange(tags, 'enabled', true);
		expect(summary.targetCount).toBe(2);
		expect(summary.changedCount).toBe(1);
		expect(summary.rows).toEqual([
			{ id: 1, name: 'temp1', from: true, to: true, changed: false },
			{ id: 2, name: 'temp1', from: false, to: true, changed: true }
		]);
	});

	it('collectionGroupId: 移動先と現在のグループが同じ行は変更なし扱い', () => {
		const tags = [
			makeTag({ id: 1, collectionGroupId: 10 }),
			makeTag({ id: 2, collectionGroupId: 20 })
		];
		const summary = summarizeBulkChange(tags, 'collectionGroupId', 10);
		expect(summary.targetCount).toBe(2);
		expect(summary.changedCount).toBe(1);
		expect(summary.rows[0]).toEqual({ id: 1, name: 'temp1', from: 10, to: 10, changed: false });
		expect(summary.rows[1]).toEqual({ id: 2, name: 'temp1', from: 20, to: 10, changed: true });
	});

	it('全件変更ありなら changedCount === targetCount', () => {
		const tags = [makeTag({ id: 1, enabled: false }), makeTag({ id: 2, enabled: false })];
		const summary = summarizeBulkChange(tags, 'enabled', true);
		expect(summary.changedCount).toBe(summary.targetCount);
	});
});

describe('formatTagsBulkDeleteConfirmMessage', () => {
	it('states the exact selected count and that history is kept', () => {
		const message = formatTagsBulkDeleteConfirmMessage(3);
		expect(message).toContain('選択した 3 件のタグを削除します。');
		expect(message).toContain('記録済みの履歴（収集データ）は削除されません。');
	});

	it('states the count even when it is zero', () => {
		const message = formatTagsBulkDeleteConfirmMessage(0);
		expect(message).toContain('選択した 0 件のタグを削除します。');
		expect(message).toContain('記録済みの履歴（収集データ）は削除されません。');
	});
});
