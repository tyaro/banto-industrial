import { describe, expect, it } from 'vitest';
import type { DisplayGroup } from '#lib/banto/displayGroupsAdmin.js';
import {
	MAX_PENS,
	addPen,
	canAddPen,
	colorSlotVar,
	defaultColorSlot,
	draftFromGroup,
	draftProblems,
	draftToInput,
	effectiveColorSlot,
	emptyDraft,
	isDraftDirty,
	movePen,
	movedGroupIds,
	reloadLatestGroup,
	removePen,
	sortFieldErrors,
	tagsUsedByOtherPens,
	thresholdSummary,
	type GroupDraft
} from './groupsPageLogic';

const group: DisplayGroup = {
	id: 7,
	name: 'ライン1',
	sortOrder: 0,
	kind: 'trend',
	attributes: { timeWindowSec: 1800 },
	pens: [
		{ tagId: 1, colorSlot: null },
		{ tagId: 2, colorSlot: 5 }
	],
	revision: 3
};

function withPens(...tagIds: (number | null)[]): GroupDraft {
	return { ...emptyDraft(), name: 'g', pens: tagIds.map((tagId) => ({ tagId, colorSlot: null })) };
}

describe('下書きと保存の形（#393）', () => {
	it('読み込んだグループの下書きは、そのまま保存の形に戻る（往復）', () => {
		const draft = draftFromGroup(group);
		expect(draftToInput(draft, group.revision)).toEqual({
			name: 'ライン1',
			kind: 'trend',
			attributes: { timeWindowSec: 1800 },
			pens: [
				{ tagId: 1, colorSlot: null },
				{ tagId: 2, colorSlot: 5 }
			],
			expectedRevision: 3
		});
	});

	it('トレンド以外では時間窓を送らない（閉じた集合の外を送らない）', () => {
		for (const kind of ['digital', 'bar', 'gauge'] as const) {
			const input = draftToInput({ ...draftFromGroup(group), kind });
			expect(input.attributes).toEqual({});
			expect('expectedRevision' in input).toBe(false);
		}
	});

	it('時間窓が無いグループは既定の 10 分で開く', () => {
		const draft = draftFromGroup({ ...group, kind: 'gauge', attributes: {} });
		expect(draft.timeWindowSec).toBe(600);
	});

	it('未保存の判定: 名前・種別・ペン・色・順番の変更は未保存、トレンド以外の時間窓は数えない', () => {
		const baseline = draftFromGroup(group);
		expect(isDraftDirty(draftFromGroup(group), baseline)).toBe(false);
		expect(isDraftDirty({ ...baseline, name: 'x' }, baseline)).toBe(true);
		expect(isDraftDirty({ ...baseline, kind: 'bar' }, baseline)).toBe(true);
		expect(isDraftDirty({ ...baseline, timeWindowSec: 60 }, baseline)).toBe(true);
		expect(isDraftDirty(movePen(baseline, 0, 1), baseline)).toBe(true);
		expect(isDraftDirty(removePen(baseline, 1), baseline)).toBe(true);
		expect(
			isDraftDirty(
				{ ...baseline, pens: [baseline.pens[0], { ...baseline.pens[1], colorSlot: 6 }] },
				baseline
			)
		).toBe(true);

		const gauge = { ...baseline, kind: 'gauge' as const };
		expect(isDraftDirty({ ...gauge, timeWindowSec: 60 }, gauge)).toBe(false);
	});

	it('タグ未選択のペンは保存の前に止める（行の番号つき）', () => {
		expect(draftProblems(withPens(1, 2))).toEqual([]);
		expect(draftProblems(withPens(1, null, null))).toEqual([
			'ペン 2 のタグを選んでください',
			'ペン 3 のタグを選んでください'
		]);
	});
});

describe('ペンの操作', () => {
	it(`ペンは ${MAX_PENS} 本まで足せる`, () => {
		let draft = emptyDraft();
		for (let i = 0; i < MAX_PENS; i += 1) {
			expect(canAddPen(draft)).toBe(true);
			draft = addPen(draft);
		}
		expect(draft.pens).toHaveLength(MAX_PENS);
		expect(canAddPen(draft)).toBe(false);
		expect(addPen(draft)).toBe(draft);
	});

	it('順番の入れ替えは端で何もしない', () => {
		const draft = withPens(1, 2, 3);
		expect(movePen(draft, 0, -1)).toBe(draft);
		expect(movePen(draft, 2, 1)).toBe(draft);
		expect(movePen(draft, 1, -1).pens.map((p) => p.tagId)).toEqual([2, 1, 3]);
		expect(movePen(draft, 1, 1).pens.map((p) => p.tagId)).toEqual([1, 3, 2]);
	});

	it('同じグループの他のペンが使っているタグは選べない', () => {
		const draft = withPens(1, null, 3);
		expect([...tagsUsedByOtherPens(draft, 0)]).toEqual([3]);
		expect([...tagsUsedByOtherPens(draft, 1)].sort()).toEqual([1, 3]);
	});

	it('色の既定はペンの位置 + 1 の枠（8 で頭打ち）、指定があればそれ', () => {
		expect([0, 1, 7, 8, 20].map(defaultColorSlot)).toEqual([1, 2, 8, 8, 8]);
		expect(effectiveColorSlot({ colorSlot: null }, 2)).toBe(3);
		expect(effectiveColorSlot({ colorSlot: 6 }, 2)).toBe(6);
		expect(colorSlotVar(4)).toBe('var(--banto-chart-4)');
	});
});

describe('しきい値の表示（タグ定義の値を読むだけ）', () => {
	it('LL / L / H / HH の順に、無いものは —', () => {
		expect(thresholdSummary({ thresholdL: 10, thresholdH: 90 })).toBe('LL — / L 10 / H 90 / HH —');
		expect(
			thresholdSummary({ thresholdLl: 0, thresholdL: 5, thresholdH: 95, thresholdHh: 100 })
		).toBe('LL 0 / L 5 / H 95 / HH 100');
	});

	it('1 つも無ければ「しきい値なし」、タグが見つからなければ —', () => {
		expect(thresholdSummary({})).toBe('しきい値なし');
		expect(thresholdSummary({ thresholdH: null })).toBe('しきい値なし');
		expect(thresholdSummary(undefined)).toBe('—');
	});
});

describe('グループの並べ替え', () => {
	const groups = [{ id: 1 }, { id: 2 }, { id: 3 }];
	it('1 つ動かした ID の並びを返し、端・不明なら null', () => {
		expect(movedGroupIds(groups, 2, -1)).toEqual([2, 1, 3]);
		expect(movedGroupIds(groups, 2, 1)).toEqual([1, 3, 2]);
		expect(movedGroupIds(groups, 1, -1)).toBeNull();
		expect(movedGroupIds(groups, 3, 1)).toBeNull();
		expect(movedGroupIds(groups, 99, 1)).toBeNull();
	});
});

describe('サーバーの検証エラーの振り分け', () => {
	it('欄・ペンの行・版の食い違い・その他に分け、どれも捨てない', () => {
		const sorted = sortFieldErrors([
			{ field: 'name', message: '既に使用されています' },
			{ field: 'kind', message: '種別' },
			{ field: 'attributes.timeWindowSec', message: '時間窓' },
			{ field: 'attributes.color', message: '使えない項目' },
			{ field: 'pens', message: '8 本まで' },
			{ field: 'pens[2].tagId', message: 'タグが見つかりません' },
			{ field: 'pens[2].colorSlot', message: '色' },
			{ field: 'pens[0].tagId', message: '重複' },
			{ field: 'expectedRevision', message: '先に更新されました' },
			{ field: 'displayGroups', message: '16 個まで' }
		]);
		expect(sorted).toEqual({
			name: ['既に使用されています'],
			kind: ['種別'],
			timeWindowSec: ['時間窓'],
			pens: ['8 本まで'],
			penRows: { 0: ['重複'], 2: ['タグが見つかりません', '色'] },
			revision: ['先に更新されました'],
			other: ['使えない項目', '16 個まで']
		});
	});
});

describe('「最新の内容を読み込む」（オーナーレビュー P2-2・P2-5）', () => {
	/** 後から解決・失敗させられる取得（遅れて届く応答の再現）。 */
	function deferred<T>() {
		let resolve!: (value: T) => void;
		let reject!: (err: unknown) => void;
		const promise = new Promise<T>((res, rej) => {
			resolve = res;
			reject = rej;
		});
		return { promise, resolve, reject };
	}
	const latest: DisplayGroup = { ...group, name: 'ライン1（他の人が更新）', revision: 4 };
	const describeError = (err: unknown) => String(err);

	it('読めたら最新の行で開き直す', async () => {
		const load = deferred<DisplayGroup[]>();
		const pending = reloadLatestGroup({
			id: group.id,
			load: () => load.promise,
			isCurrent: () => true,
			describeError
		});
		load.resolve([latest]);
		expect(await pending).toEqual({ kind: 'open', group: latest, groups: [latest] });
	});

	it('待っている間に選択・編集が変わったら、遅れて届いた応答で開き直さない（P2-2）', async () => {
		// 反証: `reloadLatestGroup` の `isCurrent()` の確認を消すと、この結果が `open` になって落ちる。
		let generation = 1;
		const load = deferred<DisplayGroup[]>();
		const pending = reloadLatestGroup({
			id: group.id,
			load: () => load.promise,
			isCurrent: () => generation === 1,
			describeError
		});
		generation = 2; // 別のグループを開いた・新規作成を始めた
		load.resolve([latest]);
		expect(await pending).toEqual({ kind: 'stale' });
	});

	it('読めなかったら古い行で開き直さず、理由を返す（P2-5）', async () => {
		// 反証: 失敗を握って一覧の古い内容で続ける形（修正前の `reloadGroups()` の振る舞い）にすると落ちる。
		const load = deferred<DisplayGroup[]>();
		const pending = reloadLatestGroup({
			id: group.id,
			load: () => load.promise,
			isCurrent: () => true,
			describeError
		});
		load.reject(new Error('サーバーに接続できません'));
		expect(await pending).toEqual({ kind: 'failed', message: 'Error: サーバーに接続できません' });
	});

	it('読めなかったが、その間に選択が変わっていたら何もしない', async () => {
		let current = true;
		const load = deferred<DisplayGroup[]>();
		const pending = reloadLatestGroup({
			id: group.id,
			load: () => load.promise,
			isCurrent: () => current,
			describeError
		});
		current = false;
		load.reject(new Error('x'));
		expect(await pending).toEqual({ kind: 'stale' });
	});

	it('読めたがグループが無ければ deleted', async () => {
		const other = { ...group, id: 99 };
		expect(
			await reloadLatestGroup({
				id: group.id,
				load: async () => [other],
				isCurrent: () => true,
				describeError
			})
		).toEqual({ kind: 'deleted', groups: [other] });
	});
});
