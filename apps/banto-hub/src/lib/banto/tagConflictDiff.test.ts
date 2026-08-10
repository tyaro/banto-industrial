/**
 * `tagConflictDiff.ts` に対するユニットテスト（`formDirty.test.ts`/
 * `tagDeleteImpact.test.ts` と同じスタイル、依存ゼロの純関数を直接
 * import）。
 */
import { describe, expect, it } from 'vitest';
import { diffFormRecords, type ConflictFieldDiff } from './tagConflictDiff';

const LABELS: Record<string, string> = {
	name: '名前',
	unit: '単位',
	enabled: '有効'
};

describe('diffFormRecords', () => {
	it('全フィールドが同じなら差分は空配列', () => {
		const local = { name: 'a', unit: '℃', enabled: true };
		const server = { name: 'a', unit: '℃', enabled: true };
		expect(diffFormRecords(local, server, LABELS)).toEqual([]);
	});

	it('値が異なるフィールドだけを返す', () => {
		const local = { name: 'a', unit: 'kPa', enabled: true };
		const server = { name: 'a', unit: 'MPa', enabled: true };
		expect(diffFormRecords(local, server, LABELS)).toEqual<ConflictFieldDiff[]>([
			{ key: 'unit', label: '単位', local: 'kPa', server: 'MPa' }
		]);
	});

	it('複数フィールドの差分を local のキー順で返す', () => {
		const local = { name: 'a', unit: 'kPa', enabled: true };
		const server = { name: 'b', unit: 'MPa', enabled: false };
		expect(diffFormRecords(local, server, LABELS)).toEqual<ConflictFieldDiff[]>([
			{ key: 'name', label: '名前', local: 'a', server: 'b' },
			{ key: 'unit', label: '単位', local: 'kPa', server: 'MPa' },
			{ key: 'enabled', label: '有効', local: 'オン', server: 'オフ' }
		]);
	});

	it('boolean は「オン」/「オフ」に正規化する', () => {
		const result = diffFormRecords({ enabled: true }, { enabled: false }, LABELS);
		expect(result).toEqual<ConflictFieldDiff[]>([
			{ key: 'enabled', label: '有効', local: 'オン', server: 'オフ' }
		]);
	});

	it('空文字・null・undefined は「（空）」に正規化する', () => {
		expect(diffFormRecords({ unit: '' }, { unit: 'kPa' }, LABELS)).toEqual<ConflictFieldDiff[]>([
			{ key: 'unit', label: '単位', local: '（空）', server: 'kPa' }
		]);
		expect(
			diffFormRecords({ unit: null }, { unit: 'kPa' }, LABELS as Record<string, string>)
		).toEqual<ConflictFieldDiff[]>([
			{ key: 'unit', label: '単位', local: '（空）', server: 'kPa' }
		]);
		expect(diffFormRecords({ unit: undefined }, { unit: 'kPa' }, LABELS)).toEqual<
			ConflictFieldDiff[]
		>([{ key: 'unit', label: '単位', local: '（空）', server: 'kPa' }]);
	});

	it('labels に無いキーはそのままキー名をラベルとして使う', () => {
		const result = diffFormRecords({ address: 'D100' }, { address: 'D200' }, LABELS);
		expect(result).toEqual<ConflictFieldDiff[]>([
			{ key: 'address', label: 'address', local: 'D100', server: 'D200' }
		]);
	});

	it('local にしかないキー・server にしかないキーの差分も検出する', () => {
		const local = { name: 'a', extraLocal: 'x' };
		const server = { name: 'a', extraServer: 'y' };
		expect(diffFormRecords(local, server, LABELS)).toEqual<ConflictFieldDiff[]>([
			{ key: 'extraLocal', label: 'extraLocal', local: 'x', server: '（空）' },
			{ key: 'extraServer', label: 'extraServer', local: '（空）', server: 'y' }
		]);
	});
});
