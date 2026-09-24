/**
 * `auditBlocks.ts` のユニットテスト（#410）。純関数の表テストと、偽の
 * `audit_log` 相手に `AuditBlockLoader` を回す回帰テスト（`eventBlocks.test.ts`
 * と同じ作り）。
 *
 * 回帰テストは、修正前の `AuditLogWindow`（`+page.svelte` に埋め込まれていた
 * ブロック読み込み）で**実際に起きた**ことを確かめたシナリオをそのまま使う
 * （再現の結果は PR #410 の本文）:
 *
 * 1. 総件数が未取得だと要求が 1 本も出ない（初回失敗の後 / 正常な 0 件の後）、
 * 2. ブロックの合間の追加で境界が重複し末尾が漏れる / 削除で欠落する、
 * 3. 失敗が状態に残らない（トーストだけ）。
 */
import { describe, expect, it } from 'vitest';
import type { AuditLogEntry, AuditLogList } from '$lib/banto/auditLogAdmin';
import {
	BLOCK_SIZE,
	blocksToFetch,
	initialCache,
	markInFlight,
	newGeneration,
	newQuery,
	type BlockRequest
} from '$lib/blockCache';
import {
	AUDIT_BOUNDARY_MISMATCH_MESSAGE,
	AUDIT_POLICY,
	AuditBlockLoader,
	auditViewState,
	type AuditBlockCache,
	type AuditBlockFailure,
	type AuditBlockOutcome,
	type AuditViewState
} from './auditBlocks';

// --- 足場 -------------------------------------------------------------------

async function flush(): Promise<void> {
	for (let i = 0; i < 5; i++) await new Promise((resolve) => setTimeout(resolve, 0));
}

function entry(id: number, action: string): AuditLogEntry {
	return {
		id,
		ts: `2026-09-24 00:00:00`,
		actorUsername: 'admin',
		actorRole: 'admin',
		action,
		resource: 'auth',
		entityId: null,
		detail: null,
		origin: 'rest',
		result: 'ok'
	};
}

interface Query {
	direction: 'asc' | 'desc';
	action: string | null;
}

/**
 * 偽の `audit_log`。backend と同じ意味づけ: 並びは `id`（= `ts`, `id` の
 * タイブレーク）、件数も行も `id <= asOfId` かつ絞り込みで数える。
 * `pruneOldest` は保持件数の削除（小さい `id` から消す）。
 */
class FakeAuditLog {
	readonly requests: (BlockRequest & { query: Query })[] = [];
	#nextId = 1;
	#rows: { id: number; action: string }[] = [];
	failNext = false;

	add(count = 1, action = 'login'): void {
		for (let i = 0; i < count; i++) this.#rows.push({ id: this.#nextId++, action });
	}

	pruneOldest(count: number): void {
		this.#rows.splice(0, count);
	}

	fetchWith = (query: () => Query) => async (request: BlockRequest) => {
		const q = query();
		this.requests.push({ ...request, query: q });
		if (this.failNext) {
			this.failNext = false;
			return { kind: 'error', message: 'サーバーに接続できません' } as AuditBlockOutcome;
		}
		return { kind: 'ready', list: this.list(request, q) } as AuditBlockOutcome;
	};

	list(request: BlockRequest, q: Query): AuditLogList {
		const asOfId = request.asOfId ?? this.#rows.at(-1)?.id ?? 0;
		const visible = this.#rows
			.filter((r) => r.id <= asOfId && (q.action === null || r.action === q.action))
			.sort((a, b) => (q.direction === 'asc' ? a.id - b.id : b.id - a.id));
		return {
			rows: visible
				.slice(request.offset, request.offset + request.limit)
				.map((r) => entry(r.id, r.action)),
			totalCount: visible.length,
			asOfId
		};
	}
}

interface Harness {
	rows: (AuditLogEntry | undefined)[];
	view: AuditViewState;
	query: Query;
	loader: AuditBlockLoader;
	ids(): (number | undefined)[];
}

function harness(
	fetcher: (query: () => Query) => (request: BlockRequest) => Promise<AuditBlockOutcome>
): Harness {
	const state: Harness = {
		rows: [],
		view: auditViewState(initialCache<AuditBlockFailure>()),
		query: { direction: 'desc', action: null },
		loader: null as unknown as AuditBlockLoader,
		ids: () => state.rows.map((r) => r?.id)
	};
	state.loader = new AuditBlockLoader(
		fetcher(() => state.query),
		{
			resetRows(length) {
				state.rows = new Array<AuditLogEntry | undefined>(length);
			},
			writeRows(offset, block) {
				if (state.rows.length < offset + block.length) state.rows.length = offset + block.length;
				for (let i = 0; i < block.length; i++) state.rows[offset + i] = block[i];
			},
			update(next) {
				state.view = next;
			}
		}
	);
	return state;
}

function deferred<T>(): { promise: Promise<T>; resolve: (value: T) => void } {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((r) => {
		resolve = r;
	});
	return { promise, resolve };
}

function pinned(asOfId = 10, totalCount = 1000): AuditBlockCache {
	return {
		...initialCache<AuditBlockFailure>(),
		snapshot: { asOfId, totalCount },
		everRead: true,
		totalCount
	};
}

// --- A: 純関数 --------------------------------------------------------------

describe('AUDIT_POLICY', () => {
	const snapshot = { asOfId: 10, totalCount: 5 };
	it.each([
		['同じ境界・同じ件数は採る', { asOfId: 10, totalCount: 5 }, null],
		[
			'境界が違う応答は採らない（`/events` と同じ）',
			{ asOfId: 11, totalCount: 5 },
			{ kind: 'error', message: AUDIT_BOUNDARY_MISMATCH_MESSAGE }
		],
		['同じ境界で件数が減った = 削除 = 失効', { asOfId: 10, totalCount: 4 }, { kind: 'expired' }],
		[
			'同じ境界で件数が増えた（起きないはず）も失効として採らない',
			{ asOfId: 10, totalCount: 6 },
			{ kind: 'expired' }
		]
	])('%s', (_label, list, expected) => {
		expect(AUDIT_POLICY.reject(snapshot, { rows: [], ...list })).toEqual(expected);
	});

	it('失効だけが世代を止める', () => {
		expect(AUDIT_POLICY.haltsGeneration?.({ kind: 'expired' })).toBe(true);
		expect(AUDIT_POLICY.haltsGeneration?.({ kind: 'error', message: 'x' })).toBe(false);
	});
});

describe('blocksToFetch（/audit-log の方針）', () => {
	it('総件数が未取得なら、表示範囲が空でも先頭ブロックを取る', () => {
		expect(blocksToFetch(initialCache<AuditBlockFailure>(), 0, 0, AUDIT_POLICY)).toEqual([0]);
	});

	it('今の世代で失効したら、表示範囲にまだ取っていないブロックがあっても取らない', () => {
		const cache: AuditBlockCache = {
			...pinned(),
			loaded: new Set([0]),
			failed: new Map([[1, { failure: { kind: 'expired' }, generation: 0 }]])
		};
		expect(blocksToFetch(cache, 0, BLOCK_SIZE * 4, AUDIT_POLICY)).toEqual([]);
		// 往復の失敗は世代を止めない（取れるブロックは取る）。
		const errored: AuditBlockCache = {
			...cache,
			failed: new Map([[1, { failure: { kind: 'error', message: 'x' }, generation: 0 }]])
		};
		expect(blocksToFetch(errored, 0, BLOCK_SIZE * 4, AUDIT_POLICY)).toEqual([2, 3]);
	});

	it('「再読み込み」（新しい世代）なら止まらず、先頭から取り直す', () => {
		const cache: AuditBlockCache = {
			...pinned(),
			loaded: new Set([0]),
			failed: new Map([[1, { failure: { kind: 'expired' }, generation: 0 }]])
		};
		expect(blocksToFetch(newGeneration(cache), 0, 0, AUDIT_POLICY)).toEqual([0]);
	});
});

describe('newQuery（並べ替え・絞り込みの変更）', () => {
	it('前の問い合わせの失敗・件数・「読めたか」を持ち越さない（再読み込みは持ち越す）', () => {
		const cache: AuditBlockCache = markInFlight(
			{
				...pinned(10, 42),
				loaded: new Set([0]),
				failed: new Map([[3, { failure: { kind: 'error', message: 'x' }, generation: 0 }]])
			},
			[1]
		);
		const query = newQuery(cache);
		expect(query.generation).toBe(1);
		expect(query.snapshot).toBeNull();
		expect(query.loaded.size).toBe(0);
		expect(query.inFlight.size).toBe(0);
		expect(query.failed.size).toBe(0);
		expect(auditViewState(query)).toMatchObject({
			loading: false,
			totalCount: null,
			failedBlockCount: 0
		});
		// 対照: 「再読み込み」は失敗を残し、件数も見せ続ける。
		const reload = newGeneration(cache);
		expect(reload.failed.size).toBe(1);
		expect(auditViewState(reload).totalCount).toBe(42);
	});
});

describe('auditViewState', () => {
	it('まだ一度も読めていないときは件数を言わない（0 件と言い切らない）', () => {
		expect(auditViewState(initialCache<AuditBlockFailure>()).totalCount).toBeNull();
	});

	it('失効は今の世代のものだけ「止めている」と言う', () => {
		const cache: AuditBlockCache = {
			...pinned(),
			failed: new Map([[1, { failure: { kind: 'expired' }, generation: 0 }]])
		};
		expect(auditViewState(cache).expired).toBe(true);
		expect(auditViewState(newGeneration(cache)).expired).toBe(false);
		expect(auditViewState(newGeneration(cache)).failedBlockCount).toBe(1);
	});
});

// --- B: 修正前に再現した欠陥の回帰テスト -------------------------------------

describe('AuditBlockLoader', () => {
	it('欠陥1: 初回取得に失敗しても、「再読み込み」で要求が出て一覧が復旧する', async () => {
		const table = new FakeAuditLog();
		table.add(3);
		table.failNext = true;
		const h = harness(table.fetchWith);

		h.loader.setRange(0, 100);
		await flush();
		h.loader.setRange(0, 0); // 総件数 0 のあいだ BantoGrid は {0,0} を通知する
		await flush();
		expect(table.requests).toHaveLength(1);
		expect(h.view).toMatchObject({
			totalCount: null,
			failedBlockCount: 1,
			errorText: 'サーバーに接続できません',
			loading: false
		});

		h.loader.reload();
		await flush();
		expect(table.requests.length).toBeGreaterThan(1);
		expect(h.view).toMatchObject({ totalCount: 3, failedBlockCount: 0, errorText: null });
		expect(h.ids()).toEqual([3, 2, 1]);
	});

	it('欠陥1: 初回取得に失敗した後、並べ替えを変えても要求が出る', async () => {
		const table = new FakeAuditLog();
		table.add(3);
		table.failNext = true;
		const h = harness(table.fetchWith);
		h.loader.setRange(0, 100);
		await flush();
		h.loader.setRange(0, 0);
		await flush();

		h.query = { direction: 'asc', action: null };
		h.loader.restart();
		await flush();
		expect(table.requests).toHaveLength(2);
		expect(h.ids()).toEqual([1, 2, 3]);
	});

	it('欠陥1: 正常な 0 件（絞り込み）の後に絞り込みを外すと、要求が出て件数が戻る', async () => {
		const table = new FakeAuditLog();
		table.add(3);
		const h = harness(table.fetchWith);
		h.loader.setRange(0, 100);
		await flush();
		expect(h.view.totalCount).toBe(3);

		h.query = { direction: 'desc', action: 'nope' };
		h.loader.restart();
		await flush();
		expect(h.view.totalCount).toBe(0);
		h.loader.setRange(0, 0); // 0 件で BantoGrid が {0,0} を通知
		await flush();

		const before = table.requests.length;
		h.query = { direction: 'desc', action: null };
		h.loader.restart();
		await flush();
		expect(table.requests.length).toBe(before + 1);
		expect(h.view.totalCount).toBe(3);
		expect(h.ids()).toEqual([3, 2, 1]);
	});

	it('並べ替え・絞り込みを変えた直後は、前の問い合わせの行と件数を見せない', async () => {
		const table = new FakeAuditLog();
		table.add(3);
		const gate = deferred<AuditBlockOutcome>();
		let hold = false;
		const h = harness((query) => {
			const inner = table.fetchWith(query);
			return (request) => (hold ? gate.promise : inner(request));
		});
		h.loader.setRange(0, 100);
		await flush();
		expect(h.rows).toHaveLength(3);

		hold = true;
		h.query = { direction: 'asc', action: null };
		h.loader.restart();
		await flush();
		expect(h.rows).toHaveLength(0);
		expect(h.view.totalCount).toBeNull();
		expect(h.view.loading).toBe(true);
	});

	it('欠陥2: ブロックの合間に行が増えても、境界で重複せず末尾まで辿れる', async () => {
		const table = new FakeAuditLog();
		table.add(BLOCK_SIZE * 2); // 400 件（id 400 が最新）
		const h = harness(table.fetchWith);

		h.loader.setRange(0, 100);
		await flush();
		expect(table.requests[0].asOfId).toBeNull();

		table.add(1); // id 401
		h.loader.setRange(BLOCK_SIZE - 10, BLOCK_SIZE + 10);
		await flush();

		expect(table.requests[1]).toMatchObject({ block: 1, asOfId: BLOCK_SIZE * 2 });
		const ids = h.ids();
		expect(ids).toHaveLength(BLOCK_SIZE * 2);
		expect(ids[BLOCK_SIZE - 1]).toBe(BLOCK_SIZE + 1);
		expect(ids[BLOCK_SIZE]).toBe(BLOCK_SIZE); // 修正前はここも 201（重複）
		expect(new Set(ids).size).toBe(ids.length);
		expect(ids.at(-1)).toBe(1); // 修正前は id 1 が漏れた
		expect(ids).not.toContain(BLOCK_SIZE * 2 + 1);

		// 「再読み込み」で新しい記録が入る。
		h.loader.reload();
		await flush();
		expect(h.view.totalCount).toBe(BLOCK_SIZE * 2 + 1);
		expect(h.rows[0]?.id).toBe(BLOCK_SIZE * 2 + 1);
	});

	it('欠陥2: ブロックの合間に保持期間の削除が走ったら、ずれたブロックを採らずに止まる（昇順）', async () => {
		const table = new FakeAuditLog();
		table.add(BLOCK_SIZE * 3); // 600 件
		const h = harness(table.fetchWith);
		h.query = { direction: 'asc', action: null };

		h.loader.setRange(0, 100); // ブロック 0 = id 1..200
		await flush();
		table.add(1);
		table.pruneOldest(1); // id 1 が消える - 修正前はここで id 201 が欠落した

		h.loader.setRange(BLOCK_SIZE - 10, BLOCK_SIZE + 10);
		await flush();
		expect(h.view.expired).toBe(true);
		expect(h.view.failedBlockCount).toBe(1);
		expect(h.rows[BLOCK_SIZE - 1]?.id).toBe(BLOCK_SIZE);
		expect(h.rows[BLOCK_SIZE]).toBeUndefined(); // ずれたブロックは入れない

		// この世代ではもう取らない（取っても採れない）。
		const before = table.requests.length;
		h.loader.setRange(BLOCK_SIZE * 2, BLOCK_SIZE * 3);
		await flush();
		expect(table.requests.length).toBe(before);

		// 「再読み込み」で最新の集合から読み直すと、続きも取れて抜けが無い。
		h.loader.reload();
		await flush();
		h.loader.setRange(BLOCK_SIZE - 10, BLOCK_SIZE + 10);
		await flush();
		expect(h.view).toMatchObject({ expired: false, failedBlockCount: 0 });
		expect(h.rows[0]?.id).toBe(2); // 新しい世代は id 1 が消えた集合
		expect(h.ids().slice(BLOCK_SIZE - 1, BLOCK_SIZE + 1)).toEqual([BLOCK_SIZE + 1, BLOCK_SIZE + 2]);
	});

	it('欠陥2: 同じ世代の最初の応答より後に削除が挟まれば、並びの向きに関係なく採らない（新しい順）', async () => {
		const table = new FakeAuditLog();
		table.add(BLOCK_SIZE * 2);
		const h = harness(table.fetchWith);
		h.loader.setRange(0, 100);
		await flush();
		table.pruneOldest(1);
		h.loader.setRange(BLOCK_SIZE, BLOCK_SIZE + 10);
		await flush();
		expect(h.view.expired).toBe(true);
		expect(h.rows[BLOCK_SIZE]).toBeUndefined();
	});

	for (const failFirst of [true, false]) {
		it(`欠陥3: 並列取得の一方が失敗しても、他方の成功で失敗が消えず再試行できる（先に${failFirst ? '失敗' : '成功'}が返る）`, async () => {
			const table = new FakeAuditLog();
			table.add(BLOCK_SIZE * 3);
			const pending = new Map<number, { resolve: (outcome: AuditBlockOutcome) => void }>();
			const requests: BlockRequest[] = [];
			const h = harness((query) => {
				const inner = table.fetchWith(query);
				return async (request) => {
					requests.push(request);
					if (request.block === 0) return inner(request);
					const gate = deferred<AuditBlockOutcome>();
					pending.set(request.block, gate);
					return gate.promise;
				};
			});

			h.loader.setRange(0, 100);
			await flush();
			h.loader.setRange(BLOCK_SIZE, BLOCK_SIZE * 3);
			await flush();
			expect([...pending.keys()].sort()).toEqual([1, 2]);

			const failure: AuditBlockOutcome = { kind: 'error', message: 'タイムアウト' };
			const success: AuditBlockOutcome = {
				kind: 'ready',
				list: table.list(
					{ block: 2, offset: BLOCK_SIZE * 2, limit: BLOCK_SIZE, asOfId: BLOCK_SIZE * 3 },
					h.query
				)
			};
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

			expect(h.view).toMatchObject({
				failedBlockCount: 1,
				errorText: 'タイムアウト',
				loading: false,
				totalCount: BLOCK_SIZE * 3
			});
			expect(h.rows[BLOCK_SIZE * 2]?.id).toBe(BLOCK_SIZE);
			expect(h.rows[BLOCK_SIZE]).toBeUndefined();

			// 同じ世代ではスクロールで取り直さない（自動で再試行しない）。
			const scrolled = requests.length;
			h.loader.setRange(BLOCK_SIZE, BLOCK_SIZE * 3);
			await flush();
			expect(requests.length).toBe(scrolled);

			// 「再読み込み」で失敗していたブロックを取り直す。
			h.loader.reload();
			await flush();
			pending.get(1)!.resolve({
				kind: 'ready',
				list: table.list(
					{ block: 1, offset: BLOCK_SIZE, limit: BLOCK_SIZE, asOfId: BLOCK_SIZE * 3 },
					h.query
				)
			});
			await flush();
			expect(requests.slice(scrolled).some((request) => request.block === 1)).toBe(true);
			expect(h.view.failedBlockCount).toBe(0);
			expect(h.view.errorText).toBeNull();
		});
	}

	it('前の世代（前の問い合わせ）の遅れた応答は、新しい世代に混ざらない', async () => {
		const table = new FakeAuditLog();
		table.add(3);
		const gates: { resolve: (outcome: AuditBlockOutcome) => void }[] = [];
		const h = harness(() => () => {
			const gate = deferred<AuditBlockOutcome>();
			gates.push(gate);
			return gate.promise;
		});
		h.loader.setRange(0, 100);
		await flush();
		h.query = { direction: 'asc', action: null };
		h.loader.restart();
		await flush();
		expect(gates).toHaveLength(2);

		// 新しい問い合わせの応答が先に、古い問い合わせ（新しい順）の応答が後に返る。
		gates[1].resolve({
			kind: 'ready',
			list: table.list(
				{ block: 0, offset: 0, limit: BLOCK_SIZE, asOfId: null },
				{ direction: 'asc', action: null }
			)
		});
		await flush();
		gates[0].resolve({
			kind: 'ready',
			list: table.list(
				{ block: 0, offset: 0, limit: BLOCK_SIZE, asOfId: null },
				{ direction: 'desc', action: null }
			)
		});
		await flush();
		expect(h.ids()).toEqual([1, 2, 3]);
	});

	it('fetcher が reject しても、失敗として残り loading を抱えない', async () => {
		const h = harness(() => () => Promise.reject(new Error('壊れた')));
		h.loader.setRange(0, 100);
		await flush();
		expect(h.view).toMatchObject({
			loading: false,
			failedBlockCount: 1,
			errorText: 'Error: 壊れた'
		});
	});
});
