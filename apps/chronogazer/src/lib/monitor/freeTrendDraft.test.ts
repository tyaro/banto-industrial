import { describe, expect, it } from 'vitest';
import {
	addPens,
	afterFreeTrendSave,
	afterFreeTrendSaveFailure,
	shouldDiscardSavedDraft,
	applyPenNumberText,
	canCreateFreeTrend,
	classifySaveFailure,
	clearDraft,
	createBlocker,
	DRAFT_STORAGE_PREFIX,
	draftFromDisplayGroup,
	draftFromFreeTrend,
	draftKeysToDiscard,
	draftOwner,
	draftStorageKey,
	draftToInput,
	draftToKeep,
	emptyDraft,
	loadDraft,
	movePenAt,
	parseDraft,
	penStyleRowProblems,
	penToInput,
	pruneDrafts,
	removePenAt,
	remainingPens,
	saveDraft,
	setPenColor,
	setPenStyle,
	emptyFreeTrendFieldErrors,
	sortFreeTrendFieldErrors,
	penRowErrorsByTag,
	resolvePenRowErrors,
	withoutPenErrors,
	usedTagKeys,
	withoutPenTexts,
	type DraftStorage,
	type FreeTrendDraft
} from './freeTrendDraft';
import type { FreeTrend } from '../banto/freeTrendsAdmin';
import { EMPTY_PEN_STYLE, type Pen } from '../banto/displayGroupsAdmin';

function memoryStorage(initial: Record<string, string> = {}): DraftStorage & {
	data: Map<string, string>;
} {
	const data = new Map(Object.entries(initial));
	return {
		data,
		get length() {
			return data.size;
		},
		key: (i) => [...data.keys()][i] ?? null,
		getItem: (k) => data.get(k) ?? null,
		setItem: (k, v) => void data.set(k, v),
		removeItem: (k) => void data.delete(k)
	};
}

const styled = {
	tagId: 5,
	colorSlot: 3,
	label: '温度',
	unit: '℃',
	decimals: 1,
	lineStyle: 'dashed' as const,
	lineWidth: 2.5,
	marker: 'circle' as const,
	markerSize: 6,
	axis: 'right' as const,
	yMin: 0,
	yMax: 100,
	limitLines: true,
	limitHh: 90,
	limitH: 80,
	limitL: 10,
	limitLl: 5,
	bitStyle: 'band' as const,
	bitBandHeight: 20
};

const saved: FreeTrend = {
	id: 7,
	name: 'ライン A',
	timeWindowSec: 300,
	pens: [styled, { ...penToInput({ tagId: 9, colorSlot: null } as never) }],
	revision: 4,
	createdBy: 'admin',
	createdAt: '2026-11-20 00:00:00',
	updatedBy: 'admin',
	updatedAt: '2026-11-20 00:00:00',
	canEdit: true
};

describe('draftFromFreeTrend / draftToInput', () => {
	it('属性を落とさずに下書きへ、保存の入力へ往復する', () => {
		const draft = draftFromFreeTrend(saved);
		expect(draft.baseRevision).toBe(4);
		expect(draft.pens[0]).toEqual(styled);
		const input = draftToInput(draft, true);
		expect(input.pens[0]).toEqual(styled);
		expect(input.expectedRevision).toBe(4);
	});
	it('新規は版を付けない', () => {
		expect(draftToInput(emptyDraft(), true).expectedRevision).toBeUndefined();
		expect(draftToInput(draftFromFreeTrend(saved), false).expectedRevision).toBeUndefined();
	});
	it('FreeTrend の余分なキーを持ち込まない', () => {
		const pen = penToInput({ ...styled, extra: 1 } as never);
		expect(Object.keys(pen)).not.toContain('extra');
	});
	it('保存の入力では凡例の名前・単位の前後の空白を落とし、空は null（8b2b）', () => {
		const draft = { ...emptyDraft(), pens: [{ ...styled, label: ' 温度 ', unit: '  ' }] };
		const input = draftToInput(draft, false);
		expect(input.pens[0].label).toBe('温度');
		expect(input.pens[0].unit).toBeNull();
		// 下書きの中の空白はそのまま（入力の途中を消さない）。
		expect(draft.pens[0].label).toBe(' 温度 ');
	});
});

describe('draftToKeep', () => {
	it('元と同じなら持たない。違えば持つ（版が違うだけなら持たない）', () => {
		const base = draftFromFreeTrend(saved);
		expect(draftToKeep({ ...base }, base)).toBeNull();
		expect(draftToKeep({ ...base, baseRevision: 1 }, base)).toBeNull();
		expect(draftToKeep({ ...base, name: 'x' }, base)).not.toBeNull();
		expect(draftToKeep(removePenAt(base, 0), base)).not.toBeNull();
		expect(draftToKeep(emptyDraft(), emptyDraft())).toBeNull();
	});
});

describe('下書きの読み書き', () => {
	const OWNER = 'account:alice';
	const K = (owner: string, t: string | number) =>
		`${DRAFT_STORAGE_PREFIX}${encodeURIComponent(owner)}:${t}`;
	it('持ち主と対象ごとのキーで往復し、属性を保つ', () => {
		const storage = memoryStorage();
		const draft = draftFromFreeTrend(saved);
		saveDraft(storage, OWNER, 7, draft);
		saveDraft(storage, OWNER, 'new', emptyDraft());
		expect(storage.data.has(K(OWNER, 7))).toBe(true);
		expect(draftStorageKey(OWNER, 'new')).toBe(K(OWNER, 'new'));
		expect(loadDraft(storage, OWNER, 7)).toEqual(draft);
		clearDraft(storage, OWNER, 7);
		expect(loadDraft(storage, OWNER, 7)).toBeNull();
		expect(loadDraft(storage, OWNER, 'new')).toEqual(emptyDraft());
	});
	it('別の持ち主の下書きは読めない。持ち主なしは読まない・書かない・消さない', () => {
		const storage = memoryStorage();
		saveDraft(storage, OWNER, 7, draftFromFreeTrend(saved));
		expect(loadDraft(storage, 'account:bob', 7)).toBeNull();
		expect(loadDraft(storage, 'localDesktop', 7)).toBeNull();
		expect(loadDraft(storage, null, 7)).toBeNull();
		saveDraft(storage, null, 7, emptyDraft());
		expect([...storage.data.keys()]).toEqual([K(OWNER, 7)]);
		clearDraft(storage, null, 7);
		clearDraft(storage, 'account:bob', 7);
		expect(loadDraft(storage, OWNER, 7)).not.toBeNull();
	});
	it('持ち主の ID にコロンがあっても取り違えない', () => {
		const storage = memoryStorage();
		saveDraft(storage, 'account:a:7', 'new', emptyDraft());
		expect(loadDraft(storage, 'account:a', 7)).toBeNull();
		expect(loadDraft(storage, 'account:a:7', 'new')).toEqual(emptyDraft());
	});
	it('キー無しの属性は null と読む', () => {
		const text = JSON.stringify({
			baseRevision: null,
			name: 'a',
			timeWindowSec: 600,
			pens: [{ tagId: 1, colorSlot: null }]
		});
		const draft = parseDraft(JSON.parse(text));
		expect(draft?.pens[0].lineStyle).toBeNull();
		expect(draft?.pens[0].bitBandHeight).toBeNull();
	});
	it('壊れた物は黙って捨てる', () => {
		const ok = { baseRevision: null, name: 'a', timeWindowSec: 600, pens: [] };
		expect(parseDraft(ok)).not.toBeNull();
		expect(parseDraft(null)).toBeNull();
		expect(parseDraft([])).toBeNull();
		expect(parseDraft({ ...ok, name: 1 })).toBeNull();
		expect(parseDraft({ ...ok, timeWindowSec: 61 })).toBeNull();
		expect(parseDraft({ ...ok, baseRevision: 'x' })).toBeNull();
		expect(parseDraft({ ...ok, baseRevision: undefined })).toBeNull();
		expect(parseDraft({ ...ok, pens: 'x' })).toBeNull();
		expect(parseDraft({ ...ok, pens: [{ tagId: 'a', colorSlot: null }] })).toBeNull();
		expect(parseDraft({ ...ok, pens: [{ tagId: 1, colorSlot: 17 }] })).toBeNull();
		expect(
			parseDraft({ ...ok, pens: [{ tagId: 1, colorSlot: null, lineStyle: 'wavy' }] })
		).toBeNull();
		expect(parseDraft({ ...ok, pens: [{ tagId: 1, colorSlot: null, yMin: 'x' }] })).toBeNull();
		expect(
			parseDraft({
				...ok,
				pens: [
					{ tagId: 1, colorSlot: null },
					{ tagId: 1, colorSlot: null }
				]
			})
		).toBeNull();
		const many = Array.from({ length: 17 }, (_, i) => ({ tagId: i + 1, colorSlot: null }));
		expect(parseDraft({ ...ok, pens: many })).toBeNull();
	});
	it('壊れた JSON・投げるストレージでも例外にしない', () => {
		const storage = memoryStorage({ [`${DRAFT_STORAGE_PREFIX}account%3Aa:1`]: '{oops' });
		expect(loadDraft(storage, 'account:a', 1)).toBeNull();
		const throwing = {
			getItem: () => {
				throw new Error('denied');
			},
			setItem: () => {
				throw new Error('denied');
			},
			removeItem: () => {
				throw new Error('denied');
			}
		};
		expect(loadDraft(throwing, 'o', 1)).toBeNull();
		expect(() => saveDraft(throwing, 'o', 1, emptyDraft())).not.toThrow();
		expect(() => clearDraft(throwing, 'o', 1)).not.toThrow();
		expect(loadDraft(undefined, 'o', 1)).toBeNull();
	});
});

describe('不要な下書きを捨てる', () => {
	const mine = 'account:alice';
	const K = (owner: string, t: string | number) =>
		`${DRAFT_STORAGE_PREFIX}${encodeURIComponent(owner)}:${t}`;
	it('判定: 一覧に無い ID・形の違うキー・他の持ち主・古い形を捨てる', () => {
		const keys = [
			K(mine, 1),
			K(mine, 2),
			K(mine, 'new'),
			K(mine, 'abc'),
			K('localDesktop', 1),
			K('account:bob', 'new'),
			`${DRAFT_STORAGE_PREFIX}1`,
			`${DRAFT_STORAGE_PREFIX}new`,
			'other'
		];
		expect(draftKeysToDiscard(keys, new Set([1]), mine)).toEqual([
			K(mine, 2),
			K(mine, 'abc'),
			K('localDesktop', 1),
			K('account:bob', 'new'),
			`${DRAFT_STORAGE_PREFIX}1`,
			`${DRAFT_STORAGE_PREFIX}new`
		]);
	});
	it('持ち主なしなら、下書きのキーをすべて捨てる（無関係なキーは残す）', () => {
		const keys = [K(mine, 1), K(mine, 'new'), `${DRAFT_STORAGE_PREFIX}new`, 'other'];
		expect(draftKeysToDiscard(keys, new Set([1]), null)).toEqual(keys.slice(0, 3));
	});
	it('ストレージから消す。自分の new と無関係なキーは残す', () => {
		const storage = memoryStorage({
			[K(mine, 1)]: '{}',
			[K(mine, 2)]: '{}',
			[K(mine, 'new')]: '{}',
			[K('account:bob', 'new')]: '{}',
			[`${DRAFT_STORAGE_PREFIX}new`]: '{}',
			other: 'x'
		});
		pruneDrafts(storage, mine, [1]);
		expect([...storage.data.keys()].sort()).toEqual([K(mine, 1), K(mine, 'new'), 'other'].sort());
		expect(() => pruneDrafts(undefined, mine, [])).not.toThrow();
	});
});

describe('draftOwner', () => {
	const base = { authDisabled: false, publicViewer: false, localDesktop: false, accountId: null };
	it.each([
		['アカウント', { ...base, accountId: 'alice' }, 'account:alice'],
		['この PC からの接続', { ...base, localDesktop: true }, 'localDesktop'],
		['ID が分からない', base, null],
		['空の ID', { ...base, accountId: '' }, null],
		['ログイン不要モード', { ...base, authDisabled: true, accountId: 'alice' }, null],
		['閲覧公開', { ...base, publicViewer: true, accountId: 'alice' }, null],
		['閲覧公開（localDesktop も立つ）', { ...base, publicViewer: true, localDesktop: true }, null]
	])('%s', (_name, session, expected) => {
		expect(draftOwner(session)).toBe(expected);
	});
});

describe('ペンの編集', () => {
	const base = (): FreeTrendDraft => ({
		...emptyDraft(),
		pens: [1, 2, 3].map((tagId) => penToInput({ tagId, colorSlot: null } as never))
	});
	it('足す。重複・上限超えは何も足さない', () => {
		const next = addPens(base(), [4, 5]);
		expect(next.pens.map((p) => p.tagId)).toEqual([1, 2, 3, 4, 5]);
		expect(addPens(base(), [2, 3])).toEqual(base());
		const full = addPens(
			emptyDraft(),
			Array.from({ length: 16 }, (_, i) => i + 1)
		);
		expect(full.pens).toHaveLength(16);
		expect(remainingPens(full)).toBe(0);
		expect(addPens(full, [99])).toBe(full);
		const almost = addPens(
			emptyDraft(),
			Array.from({ length: 15 }, (_, i) => i + 1)
		);
		expect(addPens(almost, [98, 99])).toBe(almost);
	});
	it('足したペンは属性が既定', () => {
		const pen = addPens(emptyDraft(), [4]).pens[0];
		expect(pen.lineStyle).toBeNull();
		expect(pen.colorSlot).toBeNull();
	});
	it('外す・動かす・色を決める。既存の属性は保つ', () => {
		const d = { ...base(), pens: [styled, ...base().pens] };
		expect(removePenAt(d, 0).pens.map((p) => p.tagId)).toEqual([1, 2, 3]);
		expect(removePenAt(d, 9)).toBe(d);
		expect(movePenAt(d, 0, 1).pens.map((p) => p.tagId)).toEqual([1, 5, 2, 3]);
		expect(movePenAt(d, 0, -1)).toBe(d);
		expect(movePenAt(d, 3, 1)).toBe(d);
		const colored = setPenColor(d, 0, 9);
		expect(colored.pens[0]).toEqual({ ...styled, colorSlot: 9 });
		expect(setPenColor(colored, 0, null).pens[0].colorSlot).toBeNull();
	});
	it('使用中のタグのキー（自分の行は除く）', () => {
		expect([...usedTagKeys(base(), null)].sort()).toEqual(['tag:1', 'tag:2', 'tag:3']);
		expect([...usedTagKeys(base(), 1)].sort()).toEqual(['tag:1', 'tag:3']);
	});
});

describe('canCreateFreeTrend', () => {
	const base = { authDisabled: false, publicViewer: false, localDesktop: false, signedIn: true };
	it('アカウントのログインと localDesktop は作れる', () => {
		expect(canCreateFreeTrend(base)).toBe(true);
		expect(canCreateFreeTrend({ ...base, signedIn: false, localDesktop: true })).toBe(true);
	});
	it('ログイン不要モード・閲覧公開・未確認は作れない', () => {
		expect(canCreateFreeTrend({ ...base, authDisabled: true })).toBe(false);
		expect(canCreateFreeTrend({ ...base, authDisabled: true, localDesktop: true })).toBe(false);
		expect(canCreateFreeTrend({ ...base, publicViewer: true })).toBe(false);
		expect(canCreateFreeTrend({ ...base, signedIn: false })).toBe(false);
	});
});

describe('createBlocker', () => {
	it('上限なら理由を返す。作れない人には出さない', () => {
		expect(createBlocker({ canCreate: true, count: 199, max: 200 })).toBeNull();
		expect(createBlocker({ canCreate: true, count: 200, max: 200 })).toContain('200');
		expect(createBlocker({ canCreate: false, count: 200, max: 200 })).toBeNull();
	});
});

describe('shouldDiscardSavedDraft', () => {
	const sent = { baseRevision: 3, name: 'a', timeWindowSec: 600, pens: [] };
	it('送った中身と同じなら捨てる', () => {
		expect(shouldDiscardSavedDraft({ ...sent }, sent)).toBe(true);
	});
	it('保存の後に中身が変わっていれば捨てない', () => {
		expect(shouldDiscardSavedDraft({ ...sent, name: 'b' }, sent)).toBe(false);
		expect(shouldDiscardSavedDraft({ ...sent, timeWindowSec: 60 }, sent)).toBe(false);
	});
	it('版が違えば捨てない', () => {
		expect(shouldDiscardSavedDraft({ ...sent, baseRevision: 4 }, sent)).toBe(false);
		expect(shouldDiscardSavedDraft({ ...sent, baseRevision: null }, sent)).toBe(false);
	});
	it('無い・壊れている（読めず null）なら捨てるものが無い', () => {
		expect(shouldDiscardSavedDraft(null, sent)).toBe(false);
		const storage = { getItem: () => '{broken' };
		expect(shouldDiscardSavedDraft(loadDraft(storage, 'o', 7), sent)).toBe(false);
	});
});

describe('保存の完了後の判断（持ち主・世代・セッション）', () => {
	const ctx = {
		destroyed: false,
		startedFor: 7 as number | 'new',
		currentTarget: 7 as number | 'new' | null,
		startedGen: 3,
		currentGen: 3,
		startedOwner: 'account:alice' as string | null,
		currentOwner: 'account:alice' as string | null,
		startedSessionGen: 5,
		currentSessionGen: 5
	};
	const nothing = {
		clearDraft: false,
		closeEditor: false,
		navigate: false,
		reload: false,
		notify: false,
		applyToPage: false
	};
	it('同じ編集なら編集欄に出し・閉じて移る', () => {
		expect(afterFreeTrendSaveFailure(ctx)).toEqual({
			applyToEditor: true,
			reload: true,
			notify: true
		});
		expect(afterFreeTrendSave({ ...ctx, navigating: false })).toEqual({
			clearDraft: true,
			closeEditor: true,
			navigate: true,
			reload: true,
			notify: true,
			applyToPage: true
		});
		expect(
			afterFreeTrendSave({ ...ctx, startedFor: 'new', currentTarget: 'new', navigating: false })
				.navigate
		).toBe(true);
	});
	it.each([
		['別の対象', { currentTarget: 8 }],
		['編集欄を閉じた', { currentTarget: null }],
		['新規へ移った', { currentTarget: 'new' as const }],
		['同じ対象を閉じて開き直した（世代が進んだ）', { currentGen: 4 }]
	])('%s: 編集欄に反映しない。下書きは送った持ち主のものを捨て、一覧は読み直す', (_n, patch) => {
		const c = { ...ctx, ...patch };
		expect(afterFreeTrendSaveFailure(c)).toEqual({
			applyToEditor: false,
			reload: true,
			notify: true
		});
		expect(afterFreeTrendSave({ ...c, navigating: false })).toEqual({
			clearDraft: true,
			closeEditor: false,
			navigate: false,
			reload: true,
			notify: true,
			applyToPage: true
		});
	});
	it.each([
		['セッションの世代が進んだ', { currentSessionGen: 6 }],
		['持ち主が変わった', { currentOwner: 'account:bob' }],
		['持ち主がなくなった', { currentOwner: null }],
		['セッションが替わり、画面も破棄された', { currentSessionGen: 6, destroyed: true }]
	])('%s: 前のセッションの完了なので、通知も画面も下書きも触らない', (_n, patch) => {
		const c = { ...ctx, ...patch };
		expect(afterFreeTrendSaveFailure(c)).toEqual({
			applyToEditor: false,
			reload: false,
			notify: false
		});
		expect(afterFreeTrendSave({ ...c, navigating: false })).toEqual(nothing);
	});
	it('別の移動の最中なら、編集欄は閉じるが移らない', () => {
		expect(afterFreeTrendSave({ ...ctx, navigating: true })).toEqual({
			clearDraft: true,
			closeEditor: true,
			navigate: false,
			reload: true,
			notify: true,
			applyToPage: true
		});
	});
	it('同じセッションで画面を離れたあとは、下書きを捨てて成功だけ通知する（失敗は出さない）', () => {
		expect(afterFreeTrendSaveFailure({ ...ctx, destroyed: true })).toEqual({
			applyToEditor: false,
			reload: false,
			notify: false
		});
		expect(afterFreeTrendSave({ ...ctx, destroyed: true, navigating: false })).toEqual({
			clearDraft: true,
			closeEditor: false,
			navigate: false,
			reload: false,
			notify: true,
			applyToPage: false
		});
	});
});

describe('保存の失敗', () => {
	it('field_errors を欄に振り分ける', () => {
		const sorted = sortFreeTrendFieldErrors([
			{ field: 'name', message: 'n' },
			{ field: 'timeWindowSec', message: 't' },
			{ field: 'pens', message: 'p' },
			{ field: 'pens[2].tagId', message: 'r2' },
			{ field: 'pens[2].colorSlot', message: 'r2b' },
			{ field: 'zzz', message: 'o' }
		]);
		expect(sorted.name).toEqual(['n']);
		expect(sorted.timeWindowSec).toEqual(['t']);
		expect(sorted.pens).toEqual(['p']);
		expect(sorted.penRows[2]).toEqual(['r2', 'r2b']);
		expect(sorted.other).toEqual(['o']);
	});
	it('行の誤りはタグ ID に写り、並べ替えの後も同じペンに出る', () => {
		const rows = { 0: ['a の誤り'], 2: ['c の誤り'] };
		const before = [{ tagId: 10 }, { tagId: 20 }, { tagId: 30 }];
		const byTag = penRowErrorsByTag(rows, before).byTag;
		expect(byTag).toEqual({ 10: ['a の誤り'], 30: ['c の誤り'] });
		// 10 を下へ動かしても、誤りは 10 に付いたまま（20 には出ない）。
		const moved = [{ tagId: 20 }, { tagId: 10 }, { tagId: 30 }];
		const errors = resolvePenRowErrors({ ...emptyFreeTrendFieldErrors(), penRows: rows }, before);
		expect(errors.penRows).toEqual({});
		expect(errors.penErrorsByTag[10]).toEqual(['a の誤り']);
		expect(moved.map((p) => errors.penErrorsByTag[p.tagId] ?? [])).toEqual([
			[],
			['a の誤り'],
			['c の誤り']
		]);
	});
	it('範囲外の行番号の誤りは捨てずに全体の誤りへ回す', () => {
		const errors = resolvePenRowErrors(
			{ ...emptyFreeTrendFieldErrors(), pens: ['既存'], penRows: { 5: ['はみ出し'] } },
			[{ tagId: 1 }]
		);
		expect(errors.pens).toEqual(['既存', 'はみ出し']);
		expect(errors.penErrorsByTag).toEqual({});
	});
	it('ペンを外すとそのタグ ID の誤りも消える', () => {
		const errors = resolvePenRowErrors(
			{ ...emptyFreeTrendFieldErrors(), penRows: { 0: ['x'], 1: ['y'] } },
			[{ tagId: 1 }, { tagId: 2 }]
		);
		expect(withoutPenErrors(errors, 1).penErrorsByTag).toEqual({ 2: ['y'] });
		expect(withoutPenErrors(errors, 9)).toBe(errors);
	});
	it('版の食い違い・検証・その他を分類する', () => {
		const conflict = {
			body: { kind: 'validation', field_errors: [{ field: 'expectedRevision', message: 'x' }] }
		};
		const isConflict = (e: unknown) => e === conflict;
		const fallback = () => 'boom';
		expect(classifySaveFailure(conflict, isConflict, fallback)).toEqual({ kind: 'conflict' });
		const validation = {
			body: { kind: 'validation', field_errors: [{ field: 'name', message: 'n' }] }
		};
		const r = classifySaveFailure(validation, isConflict, fallback);
		expect(r.kind).toBe('validation');
		expect(classifySaveFailure(new Error('x'), isConflict, fallback)).toEqual({
			kind: 'other',
			message: 'boom'
		});
		expect(classifySaveFailure(null, isConflict, fallback).kind).toBe('other');
	});
});

describe('ペンの属性（8b2b）', () => {
	const base = (): FreeTrendDraft => ({
		...emptyDraft(),
		pens: [styled, penToInput({ tagId: 9, colorSlot: null } as never)]
	});
	it('タグ ID で引いて属性を変える。空の名前・単位は null。無いタグは何もしない', () => {
		const d = base();
		const next = setPenStyle(d, 9, { lineStyle: 'dashed', limitLines: true, label: '' });
		expect(next.pens[1]).toMatchObject({ lineStyle: 'dashed', limitLines: true, label: null });
		expect(next.pens[0]).toEqual(styled);
		expect(setPenStyle(d, 5, { unit: '' }).pens[0].unit).toBeNull();
		expect(setPenStyle(d, 99, { lineStyle: 'dashed' })).toBe(d);
	});
	it('数の欄は読めた値だけを入れる。読めない文字・同じ値は下書きを変えない', () => {
		const d = base();
		expect(applyPenNumberText(d, 9, 'yMin', ' 1.5 ').pens[1].yMin).toBe(1.5);
		expect(applyPenNumberText(d, 5, 'limitH', '').pens[0].limitH).toBeNull();
		expect(applyPenNumberText(d, 9, 'yMin', '-')).toBe(d);
		expect(applyPenNumberText(d, 5, 'yMax', '100')).toBe(d);
		expect(applyPenNumberText(d, 99, 'yMin', '1')).toBe(d);
	});
	it('保存の前の誤りは行ごと（入力の文字を含めて、表示グループと同じ規則）', () => {
		const d = base();
		expect(penStyleRowProblems(d.pens, {})).toEqual({});
		expect(penStyleRowProblems(d.pens, { 9: { yMin: 'abc' } })).toEqual({
			1: ['ペン 2 の縦軸の最小が数ではありません']
		});
		expect(penStyleRowProblems(d.pens, { 5: { yMin: '200' } })[0]).toEqual([
			'ペン 1 の縦軸は、最小を最大より小さくしてください'
		]);
		expect(penStyleRowProblems([{ ...styled, limitH: 95 }], {})[0]).toEqual([
			'ペン 1 の管理線は、LL ≦ L ≦ H ≦ HH の順にしてください（H が HH より大きい）'
		]);
	});
	it('ペンを外したら、そのタグの入力の文字を捨てる', () => {
		expect(withoutPenTexts({ 5: { yMin: 'x' }, 9: { yMax: '1' } }, 5)).toEqual({
			9: { yMax: '1' }
		});
	});
});

describe('draftFromDisplayGroup（8c）', () => {
	const pen = (tagId: number, extra: Record<string, unknown> = {}) =>
		({ ...EMPTY_PEN_STYLE, tagId, colorSlot: null, ...extra }) as Pen;

	it('ペンを順に写し、色の枠と属性を落とさない', () => {
		const { draft, skipped } = draftFromDisplayGroup(
			{
				name: ' ライン A ',
				pens: [
					pen(3, {
						colorSlot: 5,
						label: '温度',
						lineStyle: 'dashed',
						limitHh: 90,
						bitStyle: 'band'
					}),
					pen(1)
				]
			},
			300
		);
		expect(skipped).toBe(0);
		expect(draft.baseRevision).toBeNull();
		expect(draft.name).toBe('ライン A');
		expect(draft.timeWindowSec).toBe(300);
		expect(draft.pens.map((p) => p.tagId)).toEqual([3, 1]);
		expect(draft.pens[0]).toMatchObject({
			colorSlot: 5,
			label: '温度',
			lineStyle: 'dashed',
			limitHh: 90,
			bitStyle: 'band'
		});
	});

	it('tagId の無いペンは外し、同じタグは最初の 1 本だけ', () => {
		const { draft, skipped } = draftFromDisplayGroup(
			{
				name: 'g',
				pens: [
					pen(2),
					pen(0),
					{ ...pen(4), tagId: null } as unknown as Pen,
					pen(2, { colorSlot: 9 }),
					pen(7)
				]
			},
			600
		);
		expect(draft.pens.map((p) => p.tagId)).toEqual([2, 7]);
		expect(draft.pens[0].colorSlot).toBeNull();
		expect(skipped).toBe(3);
	});

	it('16 本を超えたら先頭 16 本', () => {
		const pens = Array.from({ length: 20 }, (_, i) => pen(i + 1));
		const { draft, skipped } = draftFromDisplayGroup({ name: 'g', pens }, 600);
		expect(draft.pens).toHaveLength(16);
		expect(draft.pens[15].tagId).toBe(16);
		expect(skipped).toBe(4);
	});

	it('名前は 64 文字に切る', () => {
		const { draft } = draftFromDisplayGroup({ name: 'あ'.repeat(70), pens: [] }, 600);
		expect([...draft.name]).toHaveLength(64);
	});

	it('選択肢に無い時間窓は 600 に置き換える', () => {
		expect(draftFromDisplayGroup({ name: 'g', pens: [] }, 7200).draft.timeWindowSec).toBe(600);
		expect(draftFromDisplayGroup({ name: 'g', pens: [] }, 60).draft.timeWindowSec).toBe(60);
	});
});
