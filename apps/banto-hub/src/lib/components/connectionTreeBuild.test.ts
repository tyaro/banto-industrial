/**
 * T18-5a: connectionTreeBuild.ts のユニットテスト。ConnectionTree.svelte が
 * `$derived` で1回だけ構築する集計 Map（`buildTagCountsByGroup` /
 * `buildGroupsByConnection`）について、件数集計・順序保持・0件の扱いを
 * 固定する。
 */
import { describe, expect, it } from 'vitest';
import { buildTagCountsByGroup, buildGroupsByConnection } from './connectionTreeBuild';

describe('buildTagCountsByGroup', () => {
	it('空配列なら空の Map を返す', () => {
		expect(buildTagCountsByGroup([]).size).toBe(0);
	});

	it('複数グループへタグが分散している件数を数える', () => {
		const counts = buildTagCountsByGroup([
			{ collectionGroupId: 1 },
			{ collectionGroupId: 2 },
			{ collectionGroupId: 1 },
			{ collectionGroupId: 1 },
			{ collectionGroupId: 3 }
		]);
		expect(counts.get(1)).toBe(3);
		expect(counts.get(2)).toBe(1);
		expect(counts.get(3)).toBe(1);
	});

	it('0件のグループは Map に存在しない（get は undefined、呼び出し側で ?? 0 する）', () => {
		const counts = buildTagCountsByGroup([{ collectionGroupId: 1 }]);
		expect(counts.has(2)).toBe(false);
		expect(counts.get(2)).toBeUndefined();
	});
});

describe('buildGroupsByConnection', () => {
	it('空配列なら空の Map を返す', () => {
		expect(buildGroupsByConnection([]).size).toBe(0);
	});

	it('接続ごとにグループをまとめ、入力配列の順序を保持する', () => {
		const groupA1 = { id: 10, plcConnectionId: 1, name: 'A1' };
		const groupA2 = { id: 11, plcConnectionId: 1, name: 'A2' };
		const groupB1 = { id: 20, plcConnectionId: 2, name: 'B1' };

		const byConnection = buildGroupsByConnection([groupA1, groupB1, groupA2]);

		expect(byConnection.get(1)).toEqual([groupA1, groupA2]);
		expect(byConnection.get(2)).toEqual([groupB1]);
	});

	it('グループが0件の接続は Map に存在しない（呼び出し側で ?? [] する）', () => {
		const byConnection = buildGroupsByConnection([{ id: 10, plcConnectionId: 1, name: 'A1' }]);
		expect(byConnection.has(99)).toBe(false);
		expect(byConnection.get(99)).toBeUndefined();
	});
});
