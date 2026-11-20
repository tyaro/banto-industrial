import { describe, expect, it } from 'vitest';
import {
	addPens,
	afterFreeTrendSave,
	afterFreeTrendSaveFailure,
	shouldDiscardSavedDraft,
	canCreateFreeTrend,
	classifySaveFailure,
	clearDraft,
	createBlocker,
	DRAFT_STORAGE_PREFIX,
	draftFromFreeTrend,
	draftKeysToDiscard,
	draftStorageKey,
	draftToInput,
	draftToKeep,
	emptyDraft,
	loadDraft,
	movePenAt,
	parseDraft,
	penToInput,
	pruneDrafts,
	removePenAt,
	remainingPens,
	saveDraft,
	setPenColor,
	sortFreeTrendFieldErrors,
	usedTagKeys,
	type DraftStorage,
	type FreeTrendDraft
} from './freeTrendDraft';
import type { FreeTrend } from '../banto/freeTrendsAdmin';

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
	it('対象ごとのキーで往復し、属性を保つ', () => {
		const storage = memoryStorage();
		const draft = draftFromFreeTrend(saved);
		saveDraft(storage, 7, draft);
		saveDraft(storage, 'new', emptyDraft());
		expect(storage.data.has(`${DRAFT_STORAGE_PREFIX}7`)).toBe(true);
		expect(draftStorageKey('new')).toBe(`${DRAFT_STORAGE_PREFIX}new`);
		expect(loadDraft(storage, 7)).toEqual(draft);
		clearDraft(storage, 7);
		expect(loadDraft(storage, 7)).toBeNull();
		expect(loadDraft(storage, 'new')).toEqual(emptyDraft());
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
		const storage = memoryStorage({ [`${DRAFT_STORAGE_PREFIX}1`]: '{oops' });
		expect(loadDraft(storage, 1)).toBeNull();
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
		expect(loadDraft(throwing, 1)).toBeNull();
		expect(() => saveDraft(throwing, 1, emptyDraft())).not.toThrow();
		expect(() => clearDraft(throwing, 1)).not.toThrow();
		expect(loadDraft(undefined, 1)).toBeNull();
	});
});

describe('一覧に無い ID の下書きを捨てる', () => {
	it('判定', () => {
		const keys = [
			`${DRAFT_STORAGE_PREFIX}1`,
			`${DRAFT_STORAGE_PREFIX}2`,
			`${DRAFT_STORAGE_PREFIX}new`,
			`${DRAFT_STORAGE_PREFIX}abc`,
			'other'
		];
		expect(draftKeysToDiscard(keys, new Set([1]))).toEqual([
			`${DRAFT_STORAGE_PREFIX}2`,
			`${DRAFT_STORAGE_PREFIX}abc`
		]);
	});
	it('ストレージから消す。new と無関係なキーは残す', () => {
		const storage = memoryStorage({
			[`${DRAFT_STORAGE_PREFIX}1`]: '{}',
			[`${DRAFT_STORAGE_PREFIX}2`]: '{}',
			[`${DRAFT_STORAGE_PREFIX}new`]: '{}',
			other: 'x'
		});
		pruneDrafts(storage, [1]);
		expect([...storage.data.keys()].sort()).toEqual(
			[`${DRAFT_STORAGE_PREFIX}1`, `${DRAFT_STORAGE_PREFIX}new`, 'other'].sort()
		);
		expect(() => pruneDrafts(undefined, [])).not.toThrow();
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
		expect(shouldDiscardSavedDraft(loadDraft(storage, 7), sent)).toBe(false);
	});
});

describe('afterFreeTrendSaveFailure', () => {
	it('同じ対象を編集中なら編集欄に出し、一覧を読み直す', () => {
		expect(
			afterFreeTrendSaveFailure({ destroyed: false, startedFor: 7, currentTarget: 7 })
		).toEqual({ applyToEditor: true, reload: true });
		expect(
			afterFreeTrendSaveFailure({ destroyed: false, startedFor: 'new', currentTarget: 'new' })
		).toEqual({ applyToEditor: true, reload: true });
	});
	it('別のものを編集中・編集欄を閉じたなら編集欄に出さない', () => {
		const other = { applyToEditor: false, reload: true };
		expect(
			afterFreeTrendSaveFailure({ destroyed: false, startedFor: 7, currentTarget: 8 })
		).toEqual(other);
		expect(
			afterFreeTrendSaveFailure({ destroyed: false, startedFor: 7, currentTarget: null })
		).toEqual(other);
		expect(
			afterFreeTrendSaveFailure({ destroyed: false, startedFor: 'new', currentTarget: 7 })
		).toEqual(other);
	});
	it('画面を離れたあとは何もしない', () => {
		expect(afterFreeTrendSaveFailure({ destroyed: true, startedFor: 7, currentTarget: 7 })).toEqual(
			{ applyToEditor: false, reload: false }
		);
	});
});

describe('afterFreeTrendSave', () => {
	it('同じ対象を編集中なら移る。下書きは捨て、一覧を読み直す', () => {
		expect(afterFreeTrendSave({ destroyed: false, startedFor: 7, currentTarget: 7 })).toEqual({
			clearDraft: true,
			navigate: true,
			reload: true
		});
		expect(
			afterFreeTrendSave({ destroyed: false, startedFor: 'new', currentTarget: 'new' }).navigate
		).toBe(true);
	});
	it('待つ間に別のものへ移った・編集欄を閉じたら、移らない', () => {
		const other = afterFreeTrendSave({ destroyed: false, startedFor: 7, currentTarget: 8 });
		expect(other).toEqual({ clearDraft: true, navigate: false, reload: true });
		expect(
			afterFreeTrendSave({ destroyed: false, startedFor: 7, currentTarget: null }).navigate
		).toBe(false);
		expect(
			afterFreeTrendSave({ destroyed: false, startedFor: 'new', currentTarget: 7 }).navigate
		).toBe(false);
	});
	it('画面が破棄されていたら、下書きだけ捨てて何もしない', () => {
		expect(afterFreeTrendSave({ destroyed: true, startedFor: 7, currentTarget: 7 })).toEqual({
			clearDraft: true,
			navigate: false,
			reload: false
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
