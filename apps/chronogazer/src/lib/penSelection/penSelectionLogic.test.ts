import { describe, expect, it } from 'vitest';
import {
	defaultPenSelection,
	effectivePenSelection,
	isPenCheckDisabled,
	needsPenSelection,
	penSelectionCountLabel,
	togglePenSelection
} from './penSelectionLogic';

const ids = (n: number) => Array.from({ length: n }, (_, i) => i + 1);

describe('needsPenSelection', () => {
	it('8 本ちょうどまでは一覧を出さず、9 本から出す', () => {
		expect(needsPenSelection(ids(0), 8)).toBe(false);
		expect(needsPenSelection(ids(8), 8)).toBe(false);
		expect(needsPenSelection(ids(9), 8)).toBe(true);
		expect(needsPenSelection(ids(16), 8)).toBe(true);
	});
});

describe('defaultPenSelection / effectivePenSelection', () => {
	it('既定は先頭から 8 本', () => {
		expect(defaultPenSelection(ids(16), 8)).toEqual(ids(8));
		expect(effectivePenSelection(ids(16), null, 8)).toEqual(ids(8));
	});

	it('8 本以下のグループは、選択に関係なく全部（今と同じ）', () => {
		expect(effectivePenSelection(ids(5), null, 8)).toEqual(ids(5));
		expect(effectivePenSelection(ids(8), [2], 8)).toEqual(ids(8));
	});

	it('選んだものだけを、グループの順で返す', () => {
		expect(effectivePenSelection(ids(12), [12, 3, 9], 8)).toEqual([3, 9, 12]);
	});

	it('グループに無いタグは落とし、上限を超える分は切る', () => {
		expect(effectivePenSelection(ids(10), [99, 2], 8)).toEqual([2]);
		expect(effectivePenSelection(ids(16), ids(16), 8)).toEqual(ids(8));
	});

	it('何も選ばなければ空（出力は始められない）', () => {
		expect(effectivePenSelection(ids(12), [], 8)).toEqual([]);
	});
});

describe('togglePenSelection', () => {
	it('チェックを外すと減り、入れると足される（グループの順）', () => {
		const all = ids(12);
		expect(togglePenSelection(all, ids(8), 3, 8)).toEqual([1, 2, 4, 5, 6, 7, 8]);
		expect(togglePenSelection(all, [1, 5], 3, 8)).toEqual([1, 3, 5]);
	});

	it('8 本を超えては選べない（超えるチェックは無視）', () => {
		const all = ids(12);
		expect(togglePenSelection(all, ids(8), 9, 8)).toEqual(ids(8));
		// 1 本外せば別のペンを選べる。
		const fewer = togglePenSelection(all, ids(8), 1, 8);
		expect(togglePenSelection(all, fewer, 9, 8)).toEqual([2, 3, 4, 5, 6, 7, 8, 9]);
	});

	it('グループに無いタグは足せない', () => {
		expect(togglePenSelection(ids(12), [1], 99, 8)).toEqual([1]);
	});
});

describe('isPenCheckDisabled / ラベル', () => {
	it('上限に達したら、選んでいないペンだけ無効になる', () => {
		expect(isPenCheckDisabled(ids(8), 9, 8)).toBe(true);
		expect(isPenCheckDisabled(ids(8), 3, 8)).toBe(false);
		expect(isPenCheckDisabled(ids(7), 9, 8)).toBe(false);
	});

	it('選択数の表示', () => {
		expect(penSelectionCountLabel(3, 8)).toBe('選択中 3 / 8 本');
	});
});

describe('タグ ID の重複は 1 本に数える', () => {
	it('9 ペンで 8 タグなら選択は要らず、8 タグを返す', () => {
		const tagIds = [...ids(8), 3];
		expect(needsPenSelection(tagIds, 8)).toBe(false);
		expect(defaultPenSelection(tagIds, 8)).toEqual(ids(8));
		expect(effectivePenSelection(tagIds, null, 8)).toEqual(ids(8));
		expect(effectivePenSelection(tagIds, [1], 8)).toEqual(ids(8));
	});
	it('10 ペンで 9 タグなら選択が要り、既定は重複を除いた先頭 8 タグ', () => {
		const tagIds = [1, 2, 2, 3, 4, 5, 6, 7, 8, 9];
		expect(needsPenSelection(tagIds, 8)).toBe(true);
		expect(defaultPenSelection(tagIds, 8)).toEqual(ids(8));
		expect(effectivePenSelection(tagIds, null, 8)).toEqual(ids(8));
	});
	it('切り替えも重複を除いた列で数える', () => {
		const tagIds = [1, 2, 2, 3, 4, 5, 6, 7, 8, 9];
		expect(togglePenSelection(tagIds, ids(8), 9, 8)).toEqual(ids(8));
		expect(togglePenSelection(tagIds, ids(8), 8, 8)).toEqual(ids(7));
		expect(togglePenSelection(tagIds, ids(7), 9, 8)).toEqual([...ids(7), 9]);
	});
});
