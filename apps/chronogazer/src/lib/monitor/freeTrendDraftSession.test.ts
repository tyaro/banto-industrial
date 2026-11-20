import { describe, expect, it } from 'vitest';
import { DRAFT_STORAGE_PREFIX, draftStorageKey, type DraftStorage } from './freeTrendDraft';
import {
	DRAFT_OWNER_MARK_KEY,
	applyDraftSessionStep,
	draftOwnerOfSnapshot,
	draftSessionStep,
	readDraftOwnerMark,
	type DraftSessionPhase,
	type DraftSessionSnapshot
} from './freeTrendDraftSession';

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

const loading: DraftSessionSnapshot = { status: 'unknown', kind: null, identity: null };
const none: DraftSessionSnapshot = { status: 'none', kind: null, identity: null };
const account = (id: string): DraftSessionSnapshot => ({
	status: 'active',
	kind: 'account',
	identity: { id }
});
const localDesktop: DraftSessionSnapshot = {
	status: 'active',
	kind: 'localDesktop',
	identity: { id: 'local' }
};
const publicViewer: DraftSessionSnapshot = {
	status: 'active',
	kind: 'publicViewer',
	identity: { id: 'public' }
};
const authDisabled: DraftSessionSnapshot = {
	status: 'active',
	kind: 'local',
	identity: { id: 0 }
};

/** 見張りと同じ手順で、snapshot の列を流す。 */
function run(
	storage: ReturnType<typeof memoryStorage>,
	snapshots: DraftSessionSnapshot[],
	phase: DraftSessionPhase = { kind: 'initial' }
): { phase: DraftSessionPhase; generation: number } {
	let generation = 0;
	for (const snapshot of snapshots) {
		const step = draftSessionStep(phase, snapshot, readDraftOwnerMark(storage));
		applyDraftSessionStep(storage, step);
		phase = step.next;
		if (step.advance) generation += 1;
	}
	return { phase, generation };
}

const aliceKey = draftStorageKey('account:alice', 7);
const aliceNew = draftStorageKey('account:alice', 'new');
const bobKey = draftStorageKey('account:bob', 7);
const localKey = draftStorageKey('localDesktop', 'new');
const draftKeys = (s: ReturnType<typeof memoryStorage>) =>
	[...s.data.keys()].filter((k) => k.startsWith(DRAFT_STORAGE_PREFIX)).sort();

describe('draftOwnerOfSnapshot', () => {
	it.each([
		['読み込み中', loading, null],
		['ログアウト', none, null],
		['アカウント（ログイン名）', account('alice'), 'account:alice'],
		['この PC', localDesktop, 'localDesktop'],
		['閲覧公開', publicViewer, null],
		['ログイン不要モード', authDisabled, null],
		['ID の無いアカウント', { status: 'active', kind: 'account', identity: {} }, null]
	] as const)('%s', (_n, snapshot, owner) => {
		expect(draftOwnerOfSnapshot(snapshot)).toBe(owner);
	});
});

interface BoundaryCase {
	name: string;
	initial: Record<string, string>;
	snapshots: DraftSessionSnapshot[];
	/** 1 回目の列の後に足すキー（捨て損ねの再現）。 */
	afterEnd?: Record<string, string>;
	then?: DraftSessionSnapshot[];
	kept: string[];
	mark: string | null;
}

describe('セッションの境界で下書きを捨てる', () => {
	it.each<BoundaryCase>([
		{
			name: '再読み込み（印あり・読み込み中 → 同じ持ち主で有効）= 残す',
			initial: { [aliceKey]: '{}', [aliceNew]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, account('alice')],
			kept: [aliceKey, aliceNew],
			mark: 'account:alice'
		},
		{
			name: '読み込み中だけ（取り戻している途中）= 残す',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, loading],
			kept: [aliceKey],
			mark: 'account:alice'
		},
		{
			name: '有効 → ログアウト = 捨てる',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [account('alice'), none],
			kept: [],
			mark: null
		},
		{
			name: 'ログアウト → 同じ名前で有効 = 前に捨ててあるので残っていない',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [account('alice'), none, account('alice')],
			kept: [],
			mark: 'account:alice'
		},
		{
			name: 'ログアウトの後に同じ名前のキーが残っていても（捨て損ね）、有効になったときに捨てる',
			initial: { [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [account('alice'), none],
			afterEnd: { [aliceKey]: '{}' },
			then: [account('alice')],
			kept: [],
			mark: 'account:alice'
		},
		{
			name: '有効 A → 有効 B（別のタブのログイン）= 捨てる',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [account('alice'), account('bob')],
			kept: [],
			mark: 'account:bob'
		},
		{
			name: '有効 A → 読み込み中（hold）→ 有効 A（別のタブで同じ名前のログイン）= 捨てる',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [account('alice'), loading, account('alice')],
			kept: [],
			mark: 'account:alice'
		},
		{
			name: '印と違う持ち主で有効 = 捨てる',
			initial: { [aliceKey]: '{}', [bobKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, account('bob')],
			kept: [],
			mark: 'account:bob'
		},
		{
			name: '印が無いまま有効 = 捨てる（持ち主の分からない下書きを引き継がない）',
			initial: { [aliceKey]: '{}' },
			snapshots: [loading, account('alice')],
			kept: [],
			mark: 'account:alice'
		},
		{
			name: '閲覧公開 = 捨てる',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, publicViewer],
			kept: [],
			mark: null
		},
		{
			name: 'ログイン不要モード = 捨てる',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, authDisabled],
			kept: [],
			mark: null
		},
		{
			name: '未ログイン（ログイン画面）= 捨てる',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, none],
			kept: [],
			mark: null
		},
		{
			name: 'この PC の再読み込み = 残す',
			initial: { [localKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'localDesktop' },
			snapshots: [loading, localDesktop],
			kept: [localKey],
			mark: 'localDesktop'
		},
		{
			name: '同じ持ち主のままの再確認 = 残す',
			initial: { [aliceKey]: '{}', [DRAFT_OWNER_MARK_KEY]: 'account:alice' },
			snapshots: [loading, account('alice'), account('alice')],
			kept: [aliceKey],
			mark: 'account:alice'
		}
	])('$name', ({ initial, snapshots, afterEnd, then, kept, mark }) => {
		const storage = memoryStorage({ ...initial, other: 'keep' });
		const { phase } = run(storage, snapshots);
		if (afterEnd) for (const [k, v] of Object.entries(afterEnd)) storage.setItem(k, v);
		if (then) run(storage, then, phase);
		expect(draftKeys(storage)).toEqual([...kept].sort());
		expect(storage.getItem(DRAFT_OWNER_MARK_KEY)).toBe(mark);
		// 下書き以外のキーは触らない。
		expect(storage.getItem('other')).toBe('keep');
	});
});

describe('セッションの世代', () => {
	it.each([
		['読み込み中 → 有効 A', [loading, account('alice')], 1],
		['有効 A の再確認では進まない', [loading, account('alice'), account('alice')], 1],
		['有効 A → ログアウト', [account('alice'), none], 2],
		['有効 A → hold → 有効 A', [account('alice'), loading, account('alice')], 3],
		['有効 A → 有効 B', [account('alice'), account('bob')], 2],
		['ログアウトのまま（ログイン画面）', [none, none], 1],
		['読み込み中のままでは進まない', [loading, loading], 0]
	])('%s', (_n, snapshots, generation) => {
		expect(run(memoryStorage(), snapshots).generation).toBe(generation);
	});
});

describe('ストレージが使えない', () => {
	it('投げても止まらない', () => {
		const broken: DraftStorage = {
			get length(): number {
				throw new Error('denied');
			},
			key: () => {
				throw new Error('denied');
			},
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
		expect(readDraftOwnerMark(broken)).toBeNull();
		expect(() => applyDraftSessionStep(broken, { discardAll: true, mark: 'x' })).not.toThrow();
		expect(() => applyDraftSessionStep(undefined, { discardAll: true, mark: null })).not.toThrow();
	});
});
