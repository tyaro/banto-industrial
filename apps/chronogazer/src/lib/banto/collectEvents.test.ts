/**
 * `collectAdmin.ts` の**イベント一覧（`/events`）の節**のユニットテスト。
 * ブロック読み込みは banto の `createSnapshotListResource`（banto #248）に
 * 任せ、ここで固定するのは ChronoGazer 側の継ぎ目と、#409 オーナーレビュー
 * P2 で固定した挙動が本物のリソースの上でも保たれること:
 *
 * - `createCollectEventsFetcher` が `offset`・`limit`・**境界（`asOfId`）**・
 *   `signal` を**そのまま**サーバーへ渡し、`Readout` の `unavailable` /
 *   `notRunning` を `EventsReadoutError` にして投げること（空一覧に潰さない）、
 * - `eventsView` の表（注記・赤字・失効の案内。いちばん前のブロックの失敗、
 *   注記と赤字の両立、一度も読めていない間は「0 件」と言わない）、
 * - `createCollectEventsResource`（本物のリソース）と組み合わせた回帰:
 *   #409 P2-1（初回失敗からの回復）・P2-2（ブロックの合間の追加で重複・欠落
 *   しない）・P2-3（別ブロックの成功が失敗を消さない、応答順序によらない）、
 *   読めなかった失敗が**同じオブジェクトのまま** `failures` に残ること、
 *   4 秒の上限・日本語の文言・**トーストを出さない**こと。
 *
 * 以前は同じ挙動を `routes/(app)/events/eventBlocks.test.ts` が自前の
 * `EventBlockLoader` 相手に固定していた。無応答は「解決しない Promise」と
 * `vi.useFakeTimers()` で作る。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	adaptLegacyAuthProvider,
	createSnapshotListResource,
	initBanto,
	isSnapshotListError,
	ProviderError,
	type DataProvider,
	type SnapshotListFailure,
	type SnapshotListRequest
} from '@banto/admin-core';
import {
	COLLECT_READ_TIMEOUT_MS,
	EVENTS_BOUNDARY_MISMATCH_MESSAGE,
	EVENTS_LOADING_NOTE,
	EVENTS_MALFORMED_MESSAGE,
	EVENTS_MESSAGES,
	EVENTS_SNAPSHOT_EXPIRED_MESSAGE,
	EventsReadoutError,
	collectEventsNote,
	createCollectEventsFetcher,
	createCollectEventsResource,
	eventsTimeoutMessage,
	eventsView,
	isEventsReadoutError,
	type CollectEventList,
	type CollectEventRow,
	type CollectEventsLister,
	type Readout
} from './collectAdmin';

const BLOCK_SIZE = 200;

// --- 足場 -------------------------------------------------------------------

function row(id: number): CollectEventRow {
	return {
		id,
		tsMs: id * 1000,
		kind: 'plc_connected',
		connectionKey: 'conn:1',
		tagKey: null,
		level: null,
		value: null
	};
}

function ready(data: CollectEventList): Readout<CollectEventList> {
	return { state: 'ready', data };
}

/** マイクロタスクとタイマー 1 周を流す（偽サーバーは同期的に resolve する）。 */
async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) {
		for (let j = 0; j < 10; j++) await Promise.resolve();
		await new Promise((resolve) => setTimeout(resolve, 0));
	}
}

/** 手で resolve できる Promise（応答順序を入れ替えるため）。 */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((r) => {
		resolve = r;
	});
	return { promise, resolve };
}

interface ListCall {
	offset: number;
	limit: number;
	asOfId: number | null;
}

/**
 * 偽のイベント表。**新しい順 = id の降順**。`asOfId` の意味づけも backend と
 * 同じ: 件数も行も `id <= asOfId` で絞り、`null` ならその時点の最大 id
 * （空なら 0）を境界にする。`ignoreBoundary` は要求の `asOfId` を無視する
 * （境界の食い違いを起こす）。
 */
class FakeEvents {
	readonly calls: ListCall[] = [];
	#nextId = 1;
	#ids: number[] = [];
	/** 次の 1 本だけ往復の失敗にする（初回取得の失敗を作る）。 */
	failNext = false;
	ignoreBoundary = false;

	add(count = 1): void {
		for (let i = 0; i < count; i++) this.#ids.unshift(this.#nextId++);
	}

	answer(offset: number, limit: number, asOfId: number | null): Readout<CollectEventList> {
		const boundary = this.ignoreBoundary ? (this.#ids[0] ?? 0) : (asOfId ?? this.#ids[0] ?? 0);
		const visible = this.#ids.filter((id) => id <= boundary);
		return ready({
			rows: visible.slice(offset, offset + limit).map(row),
			totalCount: visible.length,
			asOfId: boundary
		});
	}

	list: CollectEventsLister = async (offset, limit, asOfId) => {
		this.calls.push({ offset, limit, asOfId });
		if (this.failNext) {
			this.failNext = false;
			throw new ProviderError({ kind: 'other', message: '接続できません' });
		}
		return this.answer(offset, limit, asOfId);
	};
}

function request(over: Partial<SnapshotListRequest> = {}): SnapshotListRequest {
	return {
		pagination: { offset: 200, limit: 200 },
		sort: [],
		filters: [],
		asOfId: 42,
		...over
	};
}

function errorFailure(block: number, error: ProviderError): SnapshotListFailure {
	return { block, kind: 'error', code: 'request', error };
}

// --- A: 取得関数 -------------------------------------------------------------

describe('createCollectEventsFetcher', () => {
	const list42: CollectEventList = { rows: [row(1)], totalCount: 1, asOfId: 42 };

	it('offset・limit と境界（asOfId）をそのまま渡し、ready の一覧をそのまま返す', async () => {
		const list = vi.fn<CollectEventsLister>(async () => ready(list42));
		const fetcher = createCollectEventsFetcher(list);

		await expect(fetcher(request(), new AbortController().signal)).resolves.toBe(list42);

		expect(list).toHaveBeenCalledTimes(1);
		const [offset, limit, asOfId] = list.mock.calls[0] ?? [];
		expect([offset, limit, asOfId]).toEqual([200, 200, 42]);
	});

	it('世代の最初（asOfId: null）は null のまま渡す', async () => {
		const list = vi.fn<CollectEventsLister>(async () =>
			ready({ rows: [], totalCount: 0, asOfId: 0 })
		);
		await createCollectEventsFetcher(list)(request({ asOfId: null }), new AbortController().signal);
		expect(list.mock.calls[0]?.[2]).toBeNull();
	});

	it('リソースの signal（上限切れ・新しい世代・dispose）をそのまま渡す', async () => {
		const list = vi.fn<CollectEventsLister>(async () => ready(list42));
		const controller = new AbortController();
		await createCollectEventsFetcher(list)(request(), controller.signal);
		expect(list.mock.calls[0]?.[3]).toBe(controller.signal);
	});

	for (const readout of ['unavailable', 'notRunning'] as const) {
		it(`${readout} は空一覧に潰さず EventsReadoutError を投げる`, async () => {
			const list = vi.fn<CollectEventsLister>(async () => ({ state: readout }));
			const thrown = await createCollectEventsFetcher(list)(
				request(),
				new AbortController().signal
			).catch((err: unknown) => err);
			expect(isEventsReadoutError(thrown)).toBe(true);
			expect(thrown).toBeInstanceOf(ProviderError);
			expect((thrown as EventsReadoutError).readout).toBe(readout);
			expect((thrown as EventsReadoutError).message).toBe(collectEventsNote(readout, 0));
		});
	}

	it('往復の失敗はそのまま投げる（握り潰さない・読めなかったにしない）', async () => {
		const failure = new ProviderError({ kind: 'forbidden' });
		const list = vi.fn<CollectEventsLister>(async () => {
			throw failure;
		});
		await expect(
			createCollectEventsFetcher(list)(request(), new AbortController().signal)
		).rejects.toBe(failure);
	});
});

// --- B: 画面に出す文（純関数の表） --------------------------------------------

describe('eventsView', () => {
	const unavailable = new EventsReadoutError('unavailable');
	const notRunning = new EventsReadoutError('notRunning');
	const roundTrip = new ProviderError({ kind: 'other', message: '接続できません' });
	const timeout = new ProviderError({ kind: 'other', message: eventsTimeoutMessage() });

	it('一度も読めていない・失敗なし → 「読み込んでいます」（0 件と言い切らない）', () => {
		expect(eventsView({ failures: [], totalCount: null, expired: false })).toEqual({
			note: EVENTS_LOADING_NOTE,
			errorText: null,
			expiredText: null
		});
	});

	it('読めた → 件数（0 件と非 0 件を言い分ける）', () => {
		expect(eventsView({ failures: [], totalCount: 0, expired: false }).note).toBe(
			collectEventsNote('ready', 0)
		);
		expect(eventsView({ failures: [], totalCount: 1234, expired: false }).note).toBe(
			collectEventsNote('ready', 1234)
		);
	});

	it('読めなかったブロックがあれば、行と件数があっても注記は「読み取れませんでした」', () => {
		const view = eventsView({
			failures: [errorFailure(1, unavailable)],
			totalCount: 300,
			expired: false
		});
		expect(view.note).toBe(collectEventsNote('unavailable', 300));
		expect(view.note).toContain('0件ではありません');
		expect(view.errorText).toBeNull();
	});

	it('一度も読めていなくても、読めなかった失敗は「読み込んでいます」にしない', () => {
		const view = eventsView({
			failures: [errorFailure(0, unavailable)],
			totalCount: null,
			expired: false
		});
		expect(view.note).toBe(collectEventsNote('unavailable', 0));
	});

	it('往復の失敗は赤字。一度も読めていなければ注記は「読み込んでいます」のまま（従来どおり）', () => {
		const view = eventsView({
			failures: [errorFailure(0, roundTrip)],
			totalCount: null,
			expired: false
		});
		expect(view.note).toBe(EVENTS_LOADING_NOTE);
		expect(view.errorText).toBe('接続できません');
	});

	it('読めなかった失敗と往復の失敗が別ブロックにあれば、注記と赤字の両方を出す', () => {
		const view = eventsView({
			failures: [errorFailure(1, timeout), errorFailure(2, unavailable)],
			totalCount: 600,
			expired: false
		});
		expect(view.note).toBe(collectEventsNote('unavailable', 600));
		expect(view.errorText).toBe(eventsTimeoutMessage());
	});

	it('それぞれの種類でいちばん前のブロックの失敗を出す', () => {
		const later = new ProviderError({ kind: 'other', message: '後ろのブロック' });
		const view = eventsView({
			failures: [
				errorFailure(1, notRunning),
				errorFailure(2, roundTrip),
				errorFailure(3, unavailable),
				errorFailure(4, later)
			],
			totalCount: 1000,
			expired: false
		});
		expect(view.note).toBe(collectEventsNote('notRunning', 1000));
		expect(view.errorText).toBe('接続できません');
	});

	it('失効は別の案内で出す（失敗の expired 項目は赤字にしない）', () => {
		const view = eventsView({
			failures: [{ block: 1, kind: 'expired' }],
			totalCount: 300,
			expired: true
		});
		expect(view.expiredText).toBe(EVENTS_SNAPSHOT_EXPIRED_MESSAGE);
		expect(view.errorText).toBeNull();
		expect(view.note).toBe(collectEventsNote('ready', 300));
	});
});

// --- C: 本物の SnapshotListResource と組み合わせて ----------------------------

describe('createCollectEventsResource（本物の SnapshotListResource）', () => {
	const notifier = vi.fn();

	beforeEach(() => {
		notifier.mockReset();
		// 失敗のトーストが出たら `notifier` に届くようにする（`notify: false` の確認）。
		initBanto({
			dataProvider: {} as DataProvider,
			authProvider: adaptLegacyAuthProvider({
				login: async () => ({ success: true }),
				logout: async () => {},
				check: async () => true,
				getIdentity: async () => ({ id: 'viewer', name: 'Viewer' })
			}),
			notifier: { notify: notifier },
			resources: []
		});
	});

	afterEach(() => {
		vi.useRealTimers();
	});

	it('初回取得に失敗しても、「再読み込み」で再試行できて一覧が復旧する（#409 P2-1）', async () => {
		const server = new FakeEvents();
		server.add(3);
		server.failNext = true;
		const events = createCollectEventsResource(server.list);
		try {
			events.ensureRange(0, 100);
			await settle();
			// `BantoGrid` は総件数が未取得・0 のあいだ空の表示範囲を通知してくる。
			events.ensureRange(0, 0);
			await settle();

			expect(server.calls).toHaveLength(1);
			expect(events.failures).toHaveLength(1);
			expect(events.totalCount).toBeNull();
			expect(events.loading).toBe(false);
			expect(eventsView(events)).toMatchObject({
				note: EVENTS_LOADING_NOTE,
				errorText: '接続できません'
			});

			events.refresh();
			await settle();

			expect(server.calls.length).toBeGreaterThan(1);
			expect(events.failures).toEqual([]);
			expect(events.totalCount).toBe(3);
			expect(events.rows.map((r) => r?.id)).toEqual([3, 2, 1]);
			expect(eventsView(events).note).toBe(collectEventsNote('ready', 3));
		} finally {
			events.dispose();
		}
	});

	it('ブロックの合間にイベントが増えても、境界で重複せず末尾まで辿れる（#409 P2-2）', async () => {
		const server = new FakeEvents();
		server.add(BLOCK_SIZE + 100); // 300 件（id 300 が最新）
		const events = createCollectEventsResource(server.list);
		try {
			events.ensureRange(0, 100);
			await settle();
			expect(events.totalCount).toBe(BLOCK_SIZE + 100);
			expect(server.calls[0]?.asOfId).toBeNull();

			// 先頭ブロックの取得後・次のブロックの取得前に、新しいイベントが入る。
			server.add(2);

			events.ensureRange(BLOCK_SIZE - 10, BLOCK_SIZE + 10);
			await settle();

			expect(server.calls).toHaveLength(2);
			expect(server.calls[1]).toEqual({
				offset: BLOCK_SIZE,
				limit: BLOCK_SIZE,
				asOfId: BLOCK_SIZE + 100
			});
			expect(events.failures).toEqual([]);
			const ids = events.rows.map((r) => r?.id);
			expect(ids).toHaveLength(BLOCK_SIZE + 100);
			expect(ids).toEqual(ids.slice().sort((a, b) => (b ?? 0) - (a ?? 0)));
			expect(new Set(ids).size).toBe(ids.length); // 重複なし
			expect(ids[ids.length - 1]).toBe(1); // 末尾まで辿れる（欠落なし）
			expect(ids).not.toContain(BLOCK_SIZE + 101); // 境界の後のイベントは入らない

			// 「再読み込み」は新しい世代 = 新しいイベントがここで入る。
			events.refresh();
			await settle();
			expect(events.totalCount).toBe(BLOCK_SIZE + 102);
			expect(events.rows[0]?.id).toBe(BLOCK_SIZE + 102);
		} finally {
			events.dispose();
		}
	});

	// 応答順序で結果が変わらないこと（オーナー指定「両方の順序」）。
	for (const failFirst of [true, false]) {
		it(`並列取得の一方が読めなくても他方の成功で消えない（先に${failFirst ? '失敗' : '成功'}が返る / #409 P2-3）`, async () => {
			const server = new FakeEvents();
			server.add(BLOCK_SIZE * 3); // ブロック 0〜2
			const pending = new Map<number, (value: Readout<CollectEventList>) => void>();
			const calls: ListCall[] = [];
			const list: CollectEventsLister = async (offset, limit, asOfId) => {
				calls.push({ offset, limit, asOfId });
				if (offset === 0) return server.answer(offset, limit, asOfId);
				const gate = deferred<Readout<CollectEventList>>();
				pending.set(offset / BLOCK_SIZE, gate.resolve);
				return gate.promise;
			};
			const events = createCollectEventsResource(list);
			try {
				// 先頭ブロックで境界を決めてから、ブロック 1 と 2 を並列に取らせる。
				events.ensureRange(0, 100);
				await settle();
				events.ensureRange(BLOCK_SIZE, BLOCK_SIZE * 3);
				await settle();
				expect([...pending.keys()].sort()).toEqual([1, 2]);

				const success = server.answer(BLOCK_SIZE * 2, BLOCK_SIZE, BLOCK_SIZE * 3);
				if (failFirst) {
					pending.get(1)!({ state: 'unavailable' });
					await settle();
					pending.get(2)!(success);
				} else {
					pending.get(2)!(success);
					await settle();
					pending.get(1)!({ state: 'unavailable' });
				}
				await settle();

				// ブロック 2 の成功でブロック 1 の失敗が消えていないこと。
				expect(events.failures).toHaveLength(1);
				expect(events.failures[0]?.block).toBe(1);
				expect(eventsView(events).note).toBe(collectEventsNote('unavailable', BLOCK_SIZE * 3));
				expect(events.loading).toBe(false);
				// 件数も行も消さない。
				expect(events.totalCount).toBe(BLOCK_SIZE * 3);
				expect(events.rows[BLOCK_SIZE * 2]?.id).toBe(BLOCK_SIZE); // 成功した側は入っている
				expect(events.rows[BLOCK_SIZE]).toBeUndefined(); // 失敗した側は空のまま

				// 「再読み込み」で、失敗していたブロックを取り直す。
				pending.clear();
				const before = calls.length;
				events.refresh();
				await settle();
				// 新しい世代の先頭ブロックが境界を決めたあと、失敗していたブロック 1 を取る。
				expect(calls.slice(before).some((call) => call.offset === BLOCK_SIZE)).toBe(true);
				// 取り直しが返るまで、失敗の表示は残る。
				expect(eventsView(events).note).toBe(collectEventsNote('unavailable', BLOCK_SIZE * 3));
				pending.get(1)!(server.answer(BLOCK_SIZE, BLOCK_SIZE, BLOCK_SIZE * 3));
				await settle();
				expect(events.failures).toEqual([]);
				expect(eventsView(events).note).toBe(collectEventsNote('ready', BLOCK_SIZE * 3));
			} finally {
				events.dispose();
			}
		});
	}

	it('読めなかった失敗は、取得関数が投げた EventsReadoutError が同じオブジェクトのまま残る', async () => {
		const server = new FakeEvents();
		server.add(3);
		const inner = createCollectEventsFetcher(async () => ({ state: 'unavailable' }));
		let thrown: unknown;
		const resource = createSnapshotListResource<CollectEventRow>(
			(req, signal) =>
				inner(req, signal).catch((err: unknown) => {
					thrown = err;
					throw err;
				}),
			{ requestTimeoutMs: COLLECT_READ_TIMEOUT_MS, messages: EVENTS_MESSAGES, notify: false }
		);
		const events = createCollectEventsResource(async () => ({ state: 'unavailable' }));
		try {
			resource.ensureRange(0, 100);
			events.ensureRange(0, 100);
			await settle();

			const failure = resource.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.code).toBe('request');
			expect(isEventsReadoutError(thrown)).toBe(true);
			expect(failure.error).toBe(thrown);

			// 画面が使う工場関数でも、失敗は EventsReadoutError のまま届く。
			const viaFactory = events.failures[0];
			if (viaFactory?.kind !== 'error') throw new Error('expected an error failure');
			expect(isEventsReadoutError(viaFactory.error)).toBe(true);
			expect(events.totalCount).toBeNull();
			expect(eventsView(events).note).toBe(collectEventsNote('unavailable', 0));
			expect(eventsView(events).errorText).toBeNull();
		} finally {
			resource.dispose();
			events.dispose();
		}
	});

	it('別のブロックが読めない・上限切れなら、注記と赤字の両方が出る', async () => {
		vi.useFakeTimers();
		const server = new FakeEvents();
		server.add(BLOCK_SIZE * 3);
		const list: CollectEventsLister = async (offset, limit, asOfId) => {
			if (offset === BLOCK_SIZE) return new Promise<Readout<CollectEventList>>(() => {});
			if (offset === BLOCK_SIZE * 2) return { state: 'unavailable' };
			return server.answer(offset, limit, asOfId);
		};
		const events = createCollectEventsResource(list);
		try {
			events.ensureRange(0, 100);
			await vi.advanceTimersByTimeAsync(0);
			events.ensureRange(BLOCK_SIZE, BLOCK_SIZE * 3);
			await vi.advanceTimersByTimeAsync(COLLECT_READ_TIMEOUT_MS);

			expect(events.failures.map((f) => f.block)).toEqual([1, 2]);
			expect(eventsView(events)).toEqual({
				note: collectEventsNote('unavailable', BLOCK_SIZE * 3),
				errorText: eventsTimeoutMessage(COLLECT_READ_TIMEOUT_MS),
				expiredText: null
			});
			expect(events.rows[0]?.id).toBe(BLOCK_SIZE * 3);
		} finally {
			events.dispose();
		}
	});

	it('4 秒以内に返ってこないブロックは code: timeout・日本語の文言で失敗し、要求を中断する', async () => {
		vi.useFakeTimers();
		let captured: AbortSignal | undefined;
		const list = vi.fn<CollectEventsLister>((_offset, _limit, _asOfId, signal) => {
			captured = signal;
			return new Promise<Readout<CollectEventList>>(() => {});
		});
		const events = createCollectEventsResource(list);
		try {
			events.ensureRange(0, 100);
			// banto の既定（30 秒）ではなく 4 秒で切れる。
			await vi.advanceTimersByTimeAsync(COLLECT_READ_TIMEOUT_MS - 1);
			expect(events.failures).toEqual([]);
			expect(events.loading).toBe(true);
			expect(captured?.aborted).toBe(false);

			await vi.advanceTimersByTimeAsync(1);
			const failure = events.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.code).toBe('timeout');
			expect(isSnapshotListError(failure.error)).toBe(true);
			expect(failure.error.message).toBe(eventsTimeoutMessage(COLLECT_READ_TIMEOUT_MS));
			expect(failure.error.message).toContain('4秒以内に返りませんでした');
			expect(captured?.aborted).toBe(true);
			expect(events.loading).toBe(false);
			expect(events.totalCount).toBeNull();
		} finally {
			events.dispose();
		}
	});

	it('境界の食い違い（サーバーが asOfId を無視した）は日本語の文言で失敗し、行を採らない', async () => {
		const server = new FakeEvents();
		server.add(BLOCK_SIZE + 100);
		server.ignoreBoundary = true;
		const events = createCollectEventsResource(server.list);
		try {
			events.ensureRange(0, 100);
			await settle();
			server.add(5);
			events.ensureRange(BLOCK_SIZE - 10, BLOCK_SIZE + 10);
			await settle();

			const failure = events.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.block).toBe(1);
			expect(failure.code).toBe('boundaryMismatch');
			expect(failure.error.message).toBe(EVENTS_BOUNDARY_MISMATCH_MESSAGE);
			expect(events.rows[BLOCK_SIZE]).toBeUndefined();
			expect(eventsView(events).errorText).toBe(EVENTS_BOUNDARY_MISMATCH_MESSAGE);
		} finally {
			events.dispose();
		}
	});

	it('形の正しくない応答は日本語の文言で失敗する', async () => {
		const list: CollectEventsLister = async () =>
			ready({ rows: 'broken', totalCount: 1, asOfId: 1 } as unknown as CollectEventList);
		const events = createCollectEventsResource(list);
		try {
			events.ensureRange(0, 100);
			await settle();
			const failure = events.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.code).toBe('malformed');
			expect(failure.error.message).toBe(EVENTS_MALFORMED_MESSAGE);
		} finally {
			events.dispose();
		}
	});

	it('失敗してもトーストを出さない（notify: false、2026-10-06 オーナー決定）', async () => {
		const server = new FakeEvents();
		server.add(3);
		server.failNext = true;
		const failing = createCollectEventsResource(server.list);
		const unreadable = createCollectEventsResource(async () => ({ state: 'unavailable' }));
		// 足場の確認: 同じ失敗を既定（notify: true）のリソースに渡すと通知が届く。
		const control = createSnapshotListResource<CollectEventRow>(
			createCollectEventsFetcher(async () => ({ state: 'unavailable' }))
		);
		try {
			failing.ensureRange(0, 100);
			unreadable.ensureRange(0, 100);
			await settle();
			expect(failing.failures).toHaveLength(1);
			expect(unreadable.failures).toHaveLength(1);
			expect(notifier).not.toHaveBeenCalled();

			control.ensureRange(0, 100);
			await settle();
			expect(notifier).toHaveBeenCalledTimes(1);
		} finally {
			failing.dispose();
			unreadable.dispose();
			control.dispose();
		}
	});

	it('「再読み込み」は処理中でも効き、飛行中の要求を中断して新しい世代を始める', async () => {
		const signals: AbortSignal[] = [];
		const server = new FakeEvents();
		server.add(3);
		let hang = true;
		const list: CollectEventsLister = async (offset, limit, asOfId, signal) => {
			if (signal) signals.push(signal);
			if (hang) return new Promise<Readout<CollectEventList>>(() => {});
			return server.answer(offset, limit, asOfId);
		};
		const events = createCollectEventsResource(list);
		try {
			events.ensureRange(0, 100);
			await settle();
			expect(events.loading).toBe(true);

			hang = false;
			events.refresh();
			await settle();
			expect(signals[0]?.aborted).toBe(true);
			expect(events.loading).toBe(false);
			expect(events.totalCount).toBe(3);
		} finally {
			events.dispose();
		}
	});
});
