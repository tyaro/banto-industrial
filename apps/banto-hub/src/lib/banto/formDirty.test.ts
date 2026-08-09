/**
 * `formDirty.ts` に対するユニットテスト（`tagCsv.test.ts`/
 * `tagFormNumeric.test.ts` と同じスタイル、依存ゼロの純関数を直接
 * import）。
 */
import { describe, expect, it } from 'vitest';
import { isFormDirty } from './formDirty';

describe('isFormDirty', () => {
	it('同一内容のオブジェクトは dirty ではない', () => {
		const baseline = { name: 'a', enabled: true, decimals: 0 };
		const current = { name: 'a', enabled: true, decimals: 0 };
		expect(isFormDirty(baseline, current)).toBe(false);
	});

	it('同一オブジェクト参照は dirty ではない', () => {
		const baseline = { name: 'a' };
		expect(isFormDirty(baseline, baseline)).toBe(false);
	});

	it('値が変わっていれば dirty', () => {
		const baseline = { name: 'a', enabled: true };
		const current = { name: 'b', enabled: true };
		expect(isFormDirty(baseline, current)).toBe(true);
	});

	it('ネストしたオブジェクトの変更も検出する', () => {
		const baseline = { name: 'a', nested: { x: 1, y: 2 } };
		const current = { name: 'a', nested: { x: 1, y: 3 } };
		expect(isFormDirty(baseline, current)).toBe(true);
	});

	it('ネストしたオブジェクトが同一内容なら dirty ではない', () => {
		const baseline = { name: 'a', nested: { x: 1, y: 2 } };
		const current = { name: 'a', nested: { x: 1, y: 2 } };
		expect(isFormDirty(baseline, current)).toBe(false);
	});
});
