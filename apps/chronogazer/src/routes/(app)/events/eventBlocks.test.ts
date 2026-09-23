/**
 * `eventBlocks.ts` のユニットテスト（#409 オーナーレビュー P2 の 3 件。
 * Refs #383）。純関数の表テストと、偽のサーバー相手に `EventBlockLoader` を
 * 回す回帰テスト（`tagsPageLogic.test.ts` と同じ describe/it スタイル）。
 *
 * オーナー指定の 3 本:
 *
 * 1. 初回取得を失敗させ、次は正常応答に切り替えてから「再読み込み」を押すと
 *    **リクエスト数が増えて一覧が復旧する**（表示範囲が `{0, 0}` のまま
 *    でも）、
 * 2. 先頭ブロック取得後・次のブロック取得前にイベントを追加しても、
 *    **境界で ID が重複せず、取得対象の末尾まで辿れる**、
 * 3. 2 ブロックを並列に取り、一方が失敗・他方が成功したとき、**応答順序に
 *    かかわらず**失敗の表示と再試行手段が残る。
 */
import { describe, expect, it } from 'vitest';
import type { CollectEventRow } from '$lib/banto/collectAdmin';
import {
	BLOCK_SIZE,
	EventBlockLoader,
	applyOutcome,
	blockRequest,
	blocksFor,
	blocksToFetch,
	initialCache,
	markInFlight,
	newGeneration,
	viewState,
	type BlockCache,
	type BlockOutcome,
	type BlockRequest,
	type EventsViewState
} from './eventBlocks';

// --- 足場 -------------------------------------------------------------------

/** マイクロタスクを全部流す（偽サーバーは同期的に resolve する）。 */
async function flush(): Promise<void> {
	for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

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

/**
 * 偽のイベント表。**新しい順 = id の降順**（`ts` は id と同じ向きに増える）。
 * `asOfId` の意味づけも backend と同じ: 件数も行も `id <= asOfId` で絞る。
 */
class FakeEvents {
	readonly requests: BlockRequest[] = [];
	#nextId = 1;
	#ids: number[] = [];
	/** 次の 1 本だけ失敗させる（初回取得の失敗を作る）。 */
	failNext = false;

	add(count = 1): void {
		for (let i = 0; i < count; i++) this.#ids.unshift(this.#nextId++);
	}

	fetch = async (request: BlockRequest): Promise<BlockOutcome> => {
		this.requests.push(request);
		if (this.failNext) {
			this.failNext = false;
			return { kind: 'error', message: '接続できません' };
		}
		const asOfId = request.asOfId ?? this.#ids[0] ?? 0;
		const visible = this.#ids.filter((id) => id <= asOfId);
		return {
			kind: 'ready',
			list: {
				rows: visible.slice(request.offset, request.offset + request.limit).map(row),
				totalCount: visible.length,
				asOfId
			}
		};
	};
}

interface Harness {
	rows: (CollectEventRow | undefined)[];
	view: EventsViewState;
	loader: EventBlockLoader;
}

function harness(fetcher: (request: BlockRequest) => Promise<BlockOutcome>): Harness {
	const state: Harness = {
		rows: [],
		view: viewState(initialCache()),
		loader: null as unknown as EventBlockLoader
	};
	state.loader = new EventBlockLoader(fetcher, {
		resetRows(length) {
			state.rows = new Array<CollectEventRow | undefined>(length);
		},
		writeRows(offset, block) {
			if (state.rows.length < offset + block.length) state.rows.length = offset + block.length;
			for (let i = 0; i < block.length; i++) state.rows[offset + i] = block[i];
		},
		update(next) {
			state.view = next;
		}
	});
	return state;
}

/** 手で resolve できる Promise（応答順序を入れ替えるため）。 */
function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((r) => {
		resolve = r;
	});
	return { promise, resolve };
}

// --- A: 純関数 --------------------------------------------------------------

describe('blocksFor', () => {
	it('空範囲は空配列（表示範囲が決まる前の {0,0} がここに来る）', () => {
		expect(blocksFor(0, 0)).toEqual([]);
		expect(blocksFor(50, 50)).toEqual([]);
	});

	it('跨いだブロックを全部返す', () => {
		expect(blocksFor(0, 100)).toEqual([0]);
		expect(blocksFor(0, BLOCK_SIZE)).toEqual([0]);
		expect(blocksFor(BLOCK_SIZE - 1, BLOCK_SIZE + 1)).toEqual([0, 1]);
		expect(blocksFor(BLOCK_SIZE * 2, BLOCK_SIZE * 3)).toEqual([2]);
	});
});

describe('blocksToFetch', () => {
	function ready(cache: BlockCache, asOfId = 10, totalCount = 1000): BlockCache {
		return { ...cache, snapshot: { asOfId, totalCount }, everRead: true, totalCount };
	}

	it('総件数が未取得なら、表示範囲が空でも先頭ブロックを取る（#409 P2-1）', () => {
		expect(blocksToFetch(initialCache(), 0, 0)).toEqual([0]);
	});

	it('境界が決まるまでは 1 本しか投げない（世代ごとに 1 つの集合から取るため）', () => {
		expect(blocksToFetch(initialCache(), 0, BLOCK_SIZE * 3)).toEqual([0]);
		const inFlight = markInFlight(initialCache(), [0]);
		expect(blocksToFetch(inFlight, 0, BLOCK_SIZE * 3)).toEqual([]);
	});

	it('境界が決まったら、表示範囲のブロックを並列に取る', () => {
		const cache = ready(initialCache());
		expect(blocksToFetch(cache, BLOCK_SIZE - 1, BLOCK_SIZE * 2 + 1)).toEqual([0, 1, 2]);
	});

	it('取得済み・飛行中は取らない', () => {
		const cache = markInFlight({ ...ready(initialCache()), loaded: new Set([0]) }, [1]);
		expect(blocksToFetch(cache, 0, BLOCK_SIZE * 3)).toEqual([2]);
	});

	it('今の世代で失敗したブロックはスクロールでは取り直さない（自動で再試行しない）', () => {
		const cache: BlockCache = {
			...ready(initialCache()),
			failed: new Map([[1, { failure: { kind: 'error', message: 'x' }, generation: 0 }]])
		};
		expect(blocksToFetch(cache, 0, BLOCK_SIZE * 2)).toEqual([0]);
	});

	it('「再読み込み」（新しい世代）は、失敗したブロックを表示範囲の外でも取り直す', () => {
		const failed: BlockCache = {
			...ready(initialCache()),
			loaded: new Set([0]),
			failed: new Map([
				[5, { failure: { kind: 'readout', readout: 'unavailable' }, generation: 0 }]
			])
		};
		const reloaded = newGeneration(failed);
		// 境界が外れているので、まずは先頭ブロック 1 本だけ。
		expect(blocksToFetch(reloaded, 0, 100)).toEqual([0]);
		// 境界が決まったら、失敗していたブロックも対象に戻る。
		const pinned = {
			...reloaded,
			snapshot: { asOfId: 10, totalCount: 1000 },
			loaded: new Set([0])
		};
		expect(blocksToFetch(pinned, 0, 100)).toEqual([5]);
	});
});

describe('blockRequest', () => {
	it('世代の境界を必ず載せる（最初の 1 本だけ null）', () => {
		expect(blockRequest(initialCache(), 2)).toEqual({
			block: 2,
			offset: BLOCK_SIZE * 2,
			limit: BLOCK_SIZE,
			asOfId: null
		});
		const pinned: BlockCache = { ...initialCache(), snapshot: { asOfId: 42, totalCount: 7 } };
		expect(blockRequest(pinned, 1).asOfId).toBe(42);
	});
});

describe('applyOutcome', () => {
	const list = { rows: [row(3), row(2)], totalCount: 3, asOfId: 3 };

	it('世代違いの応答は採らない（遅れてきた前世代が新しい状態を巻き戻さない）', () => {
		const cache = markInFlight(initialCache(), [0]);
		const result = applyOutcome(cache, 0, cache.generation - 1, { kind: 'ready', list });
		expect(result.cache).toBe(cache);
		expect(result.write).toBeNull();
	});

	it('世代の最初の ready で境界・総件数を固定し、行を作り直す', () => {
		const cache = markInFlight(initialCache(), [0]);
		const result = applyOutcome(cache, 0, cache.generation, { kind: 'ready', list });
		expect(result.cache.snapshot).toEqual({ asOfId: 3, totalCount: 3 });
		expect(result.resetRows).toBe(3);
		expect(result.write).toEqual({ offset: 0, rows: list.rows });
		expect(viewState(result.cache)).toMatchObject({
			loading: false,
			readout: 'ready',
			errorText: null,
			failedBlockCount: 0,
			totalCount: 3
		});
	});

	it('失敗はブロック単位に残り、別ブロックの成功では消えない（#409 P2-3）', () => {
		let cache = markInFlight(initialCache(), [0]);
		cache = applyOutcome(cache, 0, cache.generation, { kind: 'ready', list }).cache;
		cache = markInFlight(cache, [1, 2]);
		cache = applyOutcome(cache, 1, cache.generation, {
			kind: 'readout',
			readout: 'unavailable'
		}).cache;
		cache = applyOutcome(cache, 2, cache.generation, {
			kind: 'ready',
			list: { rows: [], totalCount: 3, asOfId: 3 }
		}).cache;
		expect(viewState(cache)).toMatchObject({
			loading: false,
			readout: 'unavailable',
			failedBlockCount: 1
		});
	});

	it('要求した境界と違う応答は採らない（採ると重複・欠落が戻る）', () => {
		let cache = markInFlight(initialCache(), [0]);
		cache = applyOutcome(cache, 0, cache.generation, { kind: 'ready', list }).cache;
		cache = markInFlight(cache, [1]);
		const result = applyOutcome(cache, 1, cache.generation, {
			kind: 'ready',
			list: { rows: [row(1)], totalCount: 4, asOfId: 4 }
		});
		expect(result.write).toBeNull();
		expect(result.cache.loaded.has(1)).toBe(false);
		expect(viewState(result.cache).failedBlockCount).toBe(1);
	});

	it('読めなかったときは総件数も行も触らない（0 件に潰さない）', () => {
		let cache = markInFlight(initialCache(), [0]);
		cache = applyOutcome(cache, 0, cache.generation, { kind: 'ready', list }).cache;
		cache = markInFlight(newGeneration(cache), [0]);
		const result = applyOutcome(cache, 0, cache.generation, {
			kind: 'readout',
			readout: 'unavailable'
		});
		expect(result.resetRows).toBeNull();
		expect(viewState(result.cache).totalCount).toBe(3);
	});
});

// --- B: オーナー指定の回帰テスト -------------------------------------------

describe('EventBlockLoader', () => {
	it('初回取得に失敗しても、「再読み込み」で再試行できて一覧が復旧する（#409 P2-1）', async () => {
		const server = new FakeEvents();
		server.add(3);
		server.failNext = true;
		const h = harness(server.fetch);

		h.loader.setRange(0, 100);
		await flush();
		// `BantoGrid` は総件数 0 のあいだ空の表示範囲を通知してくる。
		h.loader.setRange(0, 0);
		await flush();

		expect(server.requests).toHaveLength(1);
		expect(h.view.failedBlockCount).toBe(1);
		expect(h.view.errorText).toBe('接続できません');
		expect(h.view.loading).toBe(false);

		h.loader.reload();
		await flush();

		expect(server.requests.length).toBeGreaterThan(1);
		expect(h.view.failedBlockCount).toBe(0);
		expect(h.view.readout).toBe('ready');
		expect(h.view.totalCount).toBe(3);
		expect(h.rows.map((r) => r?.id)).toEqual([3, 2, 1]);
	});

	it('ブロックの合間にイベントが増えても、境界で重複せず末尾まで辿れる（#409 P2-2）', async () => {
		const server = new FakeEvents();
		server.add(BLOCK_SIZE + 100); // 300 件（id 300 が最新）
		const h = harness(server.fetch);

		h.loader.setRange(0, 100);
		await flush();
		expect(h.view.totalCount).toBe(BLOCK_SIZE + 100);
		expect(server.requests[0].asOfId).toBeNull();

		// 先頭ブロックの取得後・次のブロックの取得前に、新しいイベントが入る。
		server.add(2);

		h.loader.setRange(BLOCK_SIZE - 10, BLOCK_SIZE + 10);
		await flush();

		expect(server.requests).toHaveLength(2);
		expect(server.requests[1]).toMatchObject({ block: 1, asOfId: BLOCK_SIZE + 100 });
		const ids = h.rows.map((r) => r?.id);
		expect(ids).toHaveLength(BLOCK_SIZE + 100);
		expect(ids).toEqual(ids.slice().sort((a, b) => (b ?? 0) - (a ?? 0)));
		expect(new Set(ids).size).toBe(ids.length); // 重複なし
		expect(ids[ids.length - 1]).toBe(1); // 末尾まで辿れる（欠落なし）
		expect(ids).not.toContain(BLOCK_SIZE + 101); // 境界の後のイベントは入らない

		// 「再読み込み」は新しい世代 = 新しいイベントがここで入る。
		h.loader.reload();
		await flush();
		expect(h.view.totalCount).toBe(BLOCK_SIZE + 102);
		expect(h.rows[0]?.id).toBe(BLOCK_SIZE + 102);
	});

	// 応答順序で結果が変わらないこと（オーナー指定「両方の順序」）。
	for (const failFirst of [true, false]) {
		it(`並列取得の一方が失敗しても他方の成功で消えない（先に${failFirst ? '失敗' : '成功'}が返る / #409 P2-3）`, async () => {
			const server = new FakeEvents();
			server.add(BLOCK_SIZE * 3); // ブロック 0〜2
			const pending = new Map<number, { resolve: (outcome: BlockOutcome) => void }>();
			const requests: BlockRequest[] = [];
			const h = harness(async (request) => {
				requests.push(request);
				if (request.block === 0) return server.fetch(request);
				const gate = deferred<BlockOutcome>();
				pending.set(request.block, gate);
				return gate.promise;
			});

			// 先頭ブロックで境界を決めてから、ブロック 1 と 2 を並列に取らせる。
			h.loader.setRange(0, 100);
			await flush();
			h.loader.setRange(BLOCK_SIZE, BLOCK_SIZE * 3);
			await flush();
			expect([...pending.keys()].sort()).toEqual([1, 2]);

			const failure: BlockOutcome = { kind: 'readout', readout: 'unavailable' };
			const success = await server.fetch({
				block: 2,
				offset: BLOCK_SIZE * 2,
				limit: BLOCK_SIZE,
				asOfId: BLOCK_SIZE * 3
			});
			if (failFirst) {
				pending.get(1)!.resolve(failure);
				await flush();
				pending.get(2)!.resolve(success);
			} else {
				pending.get(2)!.resolve(success);
				await flush();
				pending.get(1)!.resolve(failure);
			}
			await flush();

			// ブロック 2 の成功でブロック 1 の失敗が消えていないこと。
			expect(h.view.failedBlockCount).toBe(1);
			expect(h.view.readout).toBe('unavailable');
			expect(h.view.loading).toBe(false);
			expect(h.rows[BLOCK_SIZE * 2]?.id).toBe(BLOCK_SIZE); // 成功した側は入っている
			expect(h.rows[BLOCK_SIZE]).toBeUndefined(); // 失敗した側は空のまま

			// 再試行（「再読み込み」）で、失敗していたブロックを取り直す。
			const before = requests.length;
			h.loader.reload();
			await flush();
			pending.get(1)!.resolve(
				await server.fetch({
					block: 1,
					offset: BLOCK_SIZE,
					limit: BLOCK_SIZE,
					asOfId: BLOCK_SIZE * 3
				})
			);
			await flush();
			expect(requests.length).toBeGreaterThan(before);
			expect(requests.slice(before).some((request) => request.block === 1)).toBe(true);
		});
	}
});
