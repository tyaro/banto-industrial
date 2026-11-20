import { describe, expect, it } from 'vitest';
import {
	NO_BUNDLE_LABEL,
	bundleKeyOf,
	bundleTabs,
	groupsOfBundle,
	tabKeyTarget
} from './bundleLogic.js';

const g = (id: number, bundle: string | null) => ({ id, bundle });

describe('bundleTabs', () => {
	it('束が 1 つも無ければ null（今までどおりの平らなタブ）', () => {
		expect(bundleTabs([])).toBeNull();
		expect(bundleTabs([g(1, null), g(2, null)])).toBeNull();
	});

	it('束は最初のグループの現れる順で、束なしは 1 つにまとめて最初の位置に置く', () => {
		const groups = [g(1, null), g(2, 'B'), g(3, 'A'), g(4, null), g(5, 'B')];
		expect(bundleTabs(groups)).toEqual([
			{ key: null, label: NO_BUNDLE_LABEL },
			{ key: 'B', label: 'B' },
			{ key: 'A', label: 'A' }
		]);
	});

	it('束だけのときは束なしのタブを出さない', () => {
		expect(bundleTabs([g(1, 'A'), g(2, 'A')])).toEqual([{ key: 'A', label: 'A' }]);
	});
});

describe('groupsOfBundle / bundleKeyOf', () => {
	const groups = [g(1, null), g(2, 'B'), g(3, 'A'), g(4, null), g(5, 'B')];
	it('束のグループを元の並びのまま返す', () => {
		expect(groupsOfBundle(groups, 'B').map((x) => x.id)).toEqual([2, 5]);
		expect(groupsOfBundle(groups, null).map((x) => x.id)).toEqual([1, 4]);
		expect(groupsOfBundle(groups, 'X')).toEqual([]);
	});
	it('グループの束のキー', () => {
		expect(bundleKeyOf(g(1, 'A'))).toBe('A');
		expect(bundleKeyOf(g(1, null))).toBeNull();
	});
});

describe('tabKeyTarget', () => {
	it('左右・Home・End で動き、端で回り込む', () => {
		expect(tabKeyTarget('ArrowRight', 2, 3)).toBe(0);
		expect(tabKeyTarget('ArrowLeft', 0, 3)).toBe(2);
		expect(tabKeyTarget('Home', 2, 3)).toBe(0);
		expect(tabKeyTarget('End', 0, 3)).toBe(2);
	});
	it('ほかのキー・空の列は動かない', () => {
		expect(tabKeyTarget('Enter', 0, 3)).toBeNull();
		expect(tabKeyTarget('ArrowRight', 0, 0)).toBeNull();
	});
});
