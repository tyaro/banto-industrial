/**
 * `writableDefault.ts`（T19 S1-b UX-34）に対するユニットテスト。
 * `tagFormLayout.test.ts` と同じスタイル
 * （describe/it、依存ゼロの純関数を直接 import）。
 *
 * R2b-2d（2026-10-22）: `writableArea` は解析の API（`POST /api/tags/parse-addresses`）
 * の結果で、**`undefined`（判定不能）は既定 ON を当てない**契約に変えた
 * （以前は「未配線の間は当ててよい」だった）。`true` だけが当てる。
 */
import { describe, expect, it } from 'vitest';
import {
	applyAreaVerdict,
	canDefaultWritable,
	writableAfterAreaVerdict,
	writableDefaultBlockedReason
} from './writableDefault';

describe('canDefaultWritable', () => {
	it('plc タグ + writableArea 未指定（判定不能）は false（既定 ON に倒さない）', () => {
		expect(canDefaultWritable('plc')).toBe(false);
		expect(canDefaultWritable('plc', undefined)).toBe(false);
	});

	it('computed タグは常に false', () => {
		expect(canDefaultWritable('computed')).toBe(false);
		expect(canDefaultWritable('computed', true)).toBe(false);
	});

	it('internal タグは常に false（design の適用条件が plc タグ限定のため）', () => {
		expect(canDefaultWritable('internal')).toBe(false);
	});

	it('S3: db タグは常に false（v1 は読み取り専用）', () => {
		expect(canDefaultWritable('db')).toBe(false);
	});

	it('plc タグ + writableArea=false（読み取り専用の領域）は false', () => {
		expect(canDefaultWritable('plc', false)).toBe(false);
	});

	it('plc タグ + writableArea=true（書き込める領域）だけが true', () => {
		expect(canDefaultWritable('plc', true)).toBe(true);
	});
});

describe('writableDefaultBlockedReason', () => {
	it('computed タグは理由を返す', () => {
		expect(writableDefaultBlockedReason('computed')).toMatch(/computed タグ/);
	});

	it('internal タグは理由を返さない（禁止ではなく単に適用対象外なため）', () => {
		expect(writableDefaultBlockedReason('internal')).toBeNull();
	});

	it('S3: db タグは理由を返す（v1 は読み取り専用）', () => {
		expect(writableDefaultBlockedReason('db')).toMatch(/db タグ/);
	});

	it('plc タグ + writableArea 未指定（判定不能）は理由なし（null。既定を当てないだけ）', () => {
		expect(writableDefaultBlockedReason('plc')).toBeNull();
		expect(writableDefaultBlockedReason('plc', undefined)).toBeNull();
	});

	it('plc タグ + writableArea=false は理由を返す', () => {
		expect(writableDefaultBlockedReason('plc', false)).toMatch(/読み取り専用/);
	});

	it('plc タグ + writableArea=true は理由なし', () => {
		expect(writableDefaultBlockedReason('plc', true)).toBeNull();
	});
});

describe('writableAfterAreaVerdict', () => {
	it('plc + writableArea=false（確定した断り）は writable を false に揃える', () => {
		expect(writableAfterAreaVerdict('plc', false, true)).toBe(false);
		expect(writableAfterAreaVerdict('plc', false, false)).toBe(false);
	});

	it('plc + writableArea=undefined（判定不能）は手動の値を変えない', () => {
		expect(writableAfterAreaVerdict('plc', undefined, true)).toBe(true);
		expect(writableAfterAreaVerdict('plc', undefined, false)).toBe(false);
	});

	it('plc + writableArea=true は値を変えない（true に戻さない）', () => {
		expect(writableAfterAreaVerdict('plc', true, true)).toBe(true);
		expect(writableAfterAreaVerdict('plc', true, false)).toBe(false);
	});

	it('plc 以外は writableArea に関わらず値を変えない', () => {
		expect(writableAfterAreaVerdict('internal', false, true)).toBe(true);
		expect(writableAfterAreaVerdict('computed', false, true)).toBe(true);
	});
});

describe('applyAreaVerdict（解析の後の、その時点のフォームの値で決める: オーナー P2）', () => {
	const base = {
		tagKind: 'plc' as const,
		area: true as boolean | undefined,
		touched: false,
		writable: false,
		groupDefault: true
	};

	it('未操作 + 書き込める領域 + グループ既定 ON: true', () => {
		expect(applyAreaVerdict(base)).toBe(true);
	});

	it('未操作: 既定が当たる（保持値 false でも true）', () => {
		expect(applyAreaVerdict({ ...base, writable: false })).toBe(true);
	});

	it('手動で触った + 読み取り専用の領域: false（手動 ON でも揃える）', () => {
		expect(applyAreaVerdict({ ...base, area: false, touched: true, writable: true })).toBe(false);
	});

	it('手動 ON → 解析の待ち中に OFF へ直した: 領域が書き込めても false（古い true に戻さない）', () => {
		expect(applyAreaVerdict({ ...base, touched: true, writable: false })).toBe(false);
	});

	it('手動 OFF → 待ち中に ON へ直した: 書き込める領域なら true', () => {
		expect(applyAreaVerdict({ ...base, touched: true, writable: true })).toBe(true);
	});

	it('グループ既定 OFF なら書き込める領域でも（未操作は）false', () => {
		expect(applyAreaVerdict({ ...base, groupDefault: false })).toBe(false);
	});

	it('判定不能（undefined）: 未操作は既定を当てず false、手動の値は保つ', () => {
		expect(applyAreaVerdict({ ...base, area: undefined })).toBe(false);
		expect(applyAreaVerdict({ ...base, area: undefined, touched: true, writable: true })).toBe(
			true
		);
	});
});
