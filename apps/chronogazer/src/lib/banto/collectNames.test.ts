import { describe, expect, it } from 'vitest';
import { buildKeyNames, resolveKeyLabel } from './collectNames';

describe('buildKeyNames / resolveKeyLabel（キー → 名前、#383 P5）', () => {
	const names = buildKeyNames({
		connections: [
			{ id: 1, name: 'ライン1 PLC' },
			{ id: 2, name: 'Hub' }
		],
		groups: [
			{ id: 10, name: '温度' },
			{ id: 11, name: '圧力' }
		],
		tags: [
			{ id: 100, name: '炉温', collectionGroupId: 10 },
			{ id: 101, name: '共通', collectionGroupId: 10 },
			{ id: 102, name: '共通', collectionGroupId: 11 },
			{ id: 103, name: '孤児', collectionGroupId: 99 }
		]
	});

	it('接続・タグのキーを名前に直す', () => {
		expect(resolveKeyLabel('conn:1', names)).toBe('ライン1 PLC');
		expect(resolveKeyLabel('conn:2', names)).toBe('Hub');
		expect(resolveKeyLabel('tag:100', names)).toBe('炉温');
	});

	it('別のグループに同名のタグがあるときだけ、グループ名で区別する', () => {
		expect(resolveKeyLabel('tag:101', names)).toBe('共通（温度）');
		expect(resolveKeyLabel('tag:102', names)).toBe('共通（圧力）');
	});

	it('グループが見つからない同名タグは名前だけ（括弧を空にしない）', () => {
		const orphan = buildKeyNames({
			connections: [],
			groups: [],
			tags: [
				{ id: 1, name: 'A', collectionGroupId: 5 },
				{ id: 2, name: 'A', collectionGroupId: 6 }
			]
		});
		expect(resolveKeyLabel('tag:1', orphan)).toBe('A');
	});

	it('見つからないキー（削除済みなど）はキーのまま、無い値は -', () => {
		expect(resolveKeyLabel('conn:99', names)).toBe('conn:99');
		expect(resolveKeyLabel('tag:999', names)).toBe('tag:999');
		expect(resolveKeyLabel(null, names)).toBe('-');
		expect(resolveKeyLabel(undefined, names)).toBe('-');
	});

	it('名前が読めていない（空の対応表）ときも全部キーのまま', () => {
		expect(resolveKeyLabel('conn:1', new Map())).toBe('conn:1');
	});
});
