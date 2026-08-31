/**
 * `monitorValues.ts` に対するユニットテスト。`monitorFilter.test.ts` と
 * 同じスタイル（describe/it、依存ゼロの純関数を直接 import）。
 *
 * 2026-08-31 実機診断（`monitorValues.ts` 冒頭 doc comment 参照）で
 * 特定した「WS の初期スナップショットが catalog 取得より先に届くと、
 * 届いた値が rows へ一度も反映されないまま失われる」不具合の再発防止が
 * 主目的 - 特に「catalog が後から出来ても、先に受け取っていた値が
 * 反映される」ケース（`applyTagValues` を空 rows → catalog 到着後の rows
 * の順で適用するテスト）を固定する。
 */
import { describe, expect, it } from 'vitest';
import {
	applyTagValues,
	mergeTagValues,
	type RowValue,
	type ValueBearingRow
} from './monitorValues';

interface TestRow extends ValueBearingRow {
	name: string;
}

function row(externalName: string, overrides: Partial<RowValue> = {}): TestRow {
	return {
		external_name: externalName,
		name: externalName,
		v: overrides.v ?? null,
		q: overrides.q ?? 'stale',
		t: overrides.t ?? 0
	};
}

describe('mergeTagValues', () => {
	it('空マップへ values を畳み込むと外部名をキーにしたマップになる', () => {
		const merged = mergeTagValues(new Map(), [
			{ tag: 'plc1.groupA.D1', v: 1.5, q: 'good', t: 100 },
			{ tag: 'plc1.groupA.D2', v: 2.5, q: 'good', t: 100 }
		]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 1.5, q: 'good', t: 100 });
		expect(merged.get('plc1.groupA.D2')).toEqual({ v: 2.5, q: 'good', t: 100 });
		expect(merged.size).toBe(2);
	});

	it('既存マップを書き換えずに新しいマップを返す（純関数）', () => {
		const current = new Map([['plc1.groupA.D1', { v: 0, q: 'good', t: 1 }]]);
		const merged = mergeTagValues(current, [{ tag: 'plc1.groupA.D1', v: 9, q: 'good', t: 2 }]);
		expect(current.get('plc1.groupA.D1')).toEqual({ v: 0, q: 'good', t: 1 });
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 9, q: 'good', t: 2 });
		expect(merged).not.toBe(current);
	});

	it('同じタグが更新されると後の値で上書きする', () => {
		const merged = mergeTagValues(new Map(), [
			{ tag: 'plc1.groupA.D1', v: 1, q: 'good', t: 1 },
			{ tag: 'plc1.groupA.D1', v: 2, q: 'good', t: 2 }
		]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 2, q: 'good', t: 2 });
	});

	it('既存マップに無いタグは維持したまま追加する', () => {
		const current = new Map([['plc1.groupA.D1', { v: 0, q: 'good', t: 1 }]]);
		const merged = mergeTagValues(current, [{ tag: 'plc1.groupA.D2', v: 5, q: 'good', t: 5 }]);
		expect(merged.get('plc1.groupA.D1')).toEqual({ v: 0, q: 'good', t: 1 });
		expect(merged.get('plc1.groupA.D2')).toEqual({ v: 5, q: 'good', t: 5 });
	});
});

describe('applyTagValues', () => {
	it('マップに対応するエントリがある行は値/品質/時刻が更新される', () => {
		const rows = [row('plc1.groupA.D1'), row('plc1.groupA.D2')];
		const values = new Map<string, RowValue>([['plc1.groupA.D1', { v: 42, q: 'good', t: 1000 }]]);
		const result = applyTagValues(rows, values);
		expect(result[0]).toMatchObject({ v: 42, q: 'good', t: 1000 });
		expect(result[1]).toMatchObject({ v: null, q: 'stale', t: 0 });
	});

	it('マップに無い行は同一参照のまま返す（不要な再描画を避ける）', () => {
		const rows = [row('plc1.groupA.D1'), row('plc1.groupA.D2')];
		const result = applyTagValues(rows, new Map([['plc1.groupA.D1', { v: 1, q: 'good', t: 1 }]]));
		expect(result[1]).toBe(rows[1]);
	});

	it('値が既存行と完全一致する場合は同一参照のまま返す', () => {
		const rows = [row('plc1.groupA.D1', { v: 1, q: 'good', t: 1 })];
		const result = applyTagValues(rows, new Map([['plc1.groupA.D1', { v: 1, q: 'good', t: 1 }]]));
		expect(result[0]).toBe(rows[0]);
	});

	it('回帰確認: 空の rows へ適用しても例外にならず、後から来た catalog 行には反映される（初期スナップショットの取りこぼし再発防止）', () => {
		// 2026-08-31 実機診断の再現: WS の初期スナップショットが catalog の
		// HTTP 応答より先に届くと、その時点の rows はまだ空。旧実装は
		// `rows.map(...)` でその場の空配列に突き合わせていたため、値は
		// 一致する行が無いまま消えていた。
		const values = mergeTagValues(new Map(), [{ tag: 'plc1.groupA.D1', v: 7, q: 'good', t: 500 }]);

		// (1) スナップショット到着時点では rows がまだ空 - 何も起きないが
		//     例外にもならない。
		expect(applyTagValues([], values)).toEqual([]);

		// (2) その後 catalog が到着して rows が組み立てられる。新しい行を
		//     構築する側（`+page.svelte` の `toRow` 相当）は、この時点で
		//     `values` マップを見て初期値を埋めるべき - それを担うのが
		//     `applyTagValues` の再利用箇所であることを固定する。
		const rowsAfterCatalog = [row('plc1.groupA.D1'), row('plc1.groupA.D2')];
		const result = applyTagValues(rowsAfterCatalog, values);
		expect(result[0]).toMatchObject({ v: 7, q: 'good', t: 500 });
		expect(result[1]).toMatchObject({ v: null, q: 'stale', t: 0 });
	});
});
