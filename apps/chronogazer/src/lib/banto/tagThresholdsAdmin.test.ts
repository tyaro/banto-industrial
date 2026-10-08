/**
 * `withThresholds`（#532）: タグの一覧に記録計の側のしきい値を添える純関数。
 * 監視画面（D-1 / D-2）の判定の入力になる。
 */
import { describe, expect, it } from 'vitest';
import type { Tag } from './tagRegistryAdmin';
import { NO_THRESHOLDS, withThresholds, type TagThresholds } from './tagThresholdsAdmin';

function tag(id: number): Tag {
	return {
		id,
		name: `t${id}`,
		collectionGroupId: 1,
		address: '40001',
		dataType: 'u16',
		rawLo: null,
		rawHi: null,
		engLo: null,
		engHi: null,
		unit: null,
		decimals: 0,
		enabled: true,
		revision: 1
	};
}

describe('withThresholds', () => {
	const thresholds: TagThresholds[] = [
		{ tagId: 2, thresholdLl: null, thresholdL: 0, thresholdH: 80, thresholdHh: null, revision: 3 },
		// タグの一覧に無い（消えた）タグのしきい値は使われない。
		{ tagId: 9, thresholdLl: 1, thresholdL: 2, thresholdH: 3, thresholdHh: 4, revision: 1 }
	];

	it('一覧にあるタグにはその値、無いタグは設定なし（既定）。0 は 0 のまま', () => {
		const merged = withThresholds([tag(1), tag(2)], thresholds);
		expect(merged).toEqual([
			{ ...tag(1), ...NO_THRESHOLDS },
			{ ...tag(2), thresholdLl: null, thresholdL: 0, thresholdH: 80, thresholdHh: null }
		]);
	});

	it('タグの並びと数は変えず、元の要素は変えない', () => {
		const tags = [tag(2), tag(1)];
		const merged = withThresholds(tags, thresholds);
		expect(merged.map((t) => t.id)).toEqual([2, 1]);
		expect('thresholdH' in tags[0]).toBe(false);
	});

	it('しきい値の一覧が空ならすべて設定なし', () => {
		expect(withThresholds([tag(1)], [])).toEqual([{ ...tag(1), ...NO_THRESHOLDS }]);
	});
});
