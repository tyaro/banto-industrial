/**
 * `auditLogAdmin.ts` のユニットテスト。監査ログ画面のブロック読み込みは
 * banto の `createSnapshotListResource`（banto #248）に任せ、ここで固定するのは
 * ChronoGazer 側の継ぎ目だけ:
 *
 * - `createAuditLogFetcher` が要求の並べ替え・絞り込み・ページングと**境界
 *   （`asOfId`）をそのまま**サーバーへ渡すこと（落とすとブロックの合間の追加で
 *   行がずれる - 下の「リソースと組み合わせて」で実際に確かめる）、
 * - 上限（15 秒）に当たったら日本語の文言で失敗し、要求を中断すること、
 * - リソースの `signal`（新しい世代・`dispose()`）でも要求を中断すること、
 * - `listAuditLog` の URL（`?asOfId=`）と Tauri の `audit_log_list` の引数。
 *
 * 無応答は「解決しない Promise」と `vi.useFakeTimers()` で作る
 * （`runWithLimit.test.ts` と同じ）。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	createSnapshotListResource,
	ProviderError,
	SNAPSHOT_BOUNDARY_MISMATCH_MESSAGE,
	type ListParams,
	type SnapshotListRequest
} from '@banto/admin-core';

// `./setup` の実体は読み込んだだけで環境判定を始めるので、使う口だけを
// 差し替える。`mode` で REST（`server`）と Tauri を切り替える。
const env = vi.hoisted(() => ({ mode: 'server' as 'server' | 'tauri' | 'demo' }));
const invokeMock = vi.hoisted(() => vi.fn());
vi.mock('./setup', () => ({
	CSRF_HEADER: { 'X-Banto-Client': 'banto' },
	getBantoMode: () => env.mode
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));
vi.mock('@banto/admin-core', async (importOriginal) => ({
	...(await importOriginal<typeof import('@banto/admin-core')>()),
	getAuthProvider: () => ({ getToken: () => null })
}));

import {
	AUDIT_BOUNDARY_MISMATCH_MESSAGE,
	auditErrorText,
	auditListTimeoutMessage,
	createAuditLogFetcher,
	listAuditLog,
	type AuditLogEntry,
	type AuditLogLister,
	type AuditLogList
} from './auditLogAdmin';

const LIMIT_MS = 15000;

function entry(id: number): AuditLogEntry {
	return {
		id,
		ts: `2026-10-06T00:00:${String(id % 60).padStart(2, '0')}Z`,
		actorUsername: 'admin',
		actorRole: 'admin',
		action: 'login',
		resource: 'auth',
		entityId: null,
		detail: null,
		origin: 'tauri',
		result: 'ok'
	};
}

function request(over: Partial<SnapshotListRequest> = {}): SnapshotListRequest {
	return {
		pagination: { offset: 200, limit: 200 },
		sort: [{ field: 'ts', direction: 'desc' }],
		filters: [{ field: 'action', op: 'contains', value: 'login' }],
		asOfId: 42,
		...over
	};
}

describe('createAuditLogFetcher', () => {
	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	it('並べ替え・絞り込み・ページングと境界（asOfId）をそのまま渡す', async () => {
		const answer: AuditLogList = { rows: [entry(1)], totalCount: 1, asOfId: 42, deletionEpoch: 0 };
		const list = vi.fn<AuditLogLister>(async () => answer);
		const fetcher = createAuditLogFetcher(list, LIMIT_MS);

		const req = request();
		await expect(fetcher(req, new AbortController().signal)).resolves.toBe(answer);

		expect(list).toHaveBeenCalledTimes(1);
		const [params, asOfId] = list.mock.calls[0] ?? [];
		expect(params).toEqual({ pagination: req.pagination, sort: req.sort, filters: req.filters });
		expect(asOfId).toBe(42);
	});

	it('世代の最初（asOfId: null）は null のまま渡す', async () => {
		const list = vi.fn<AuditLogLister>(async () => ({ rows: [], totalCount: 0, asOfId: 0 }));
		await createAuditLogFetcher(list, LIMIT_MS)(
			request({ asOfId: null }),
			new AbortController().signal
		);
		expect(list.mock.calls[0]?.[1]).toBeNull();
	});

	it('上限を過ぎても返ってこない要求は日本語の文言で失敗し、要求を中断する', async () => {
		vi.useFakeTimers();
		let captured: AbortSignal | undefined;
		const list = vi.fn<AuditLogLister>((_params, _asOfId, signal) => {
			captured = signal;
			return new Promise<AuditLogList>(() => {});
		});
		const pending = createAuditLogFetcher(list, LIMIT_MS)(request(), new AbortController().signal);
		const settled = pending.then(
			() => null,
			(err: unknown) => err
		);

		await vi.advanceTimersByTimeAsync(LIMIT_MS);
		const err = await settled;
		expect(err).toBeInstanceOf(ProviderError);
		expect((err as ProviderError).message).toBe(auditListTimeoutMessage(LIMIT_MS));
		expect((err as ProviderError).message).toContain('15秒以内に返りませんでした');
		expect(captured?.aborted).toBe(true);
	});

	it('リソースの signal（新しい世代・dispose）が畳まれたら要求も中断する', async () => {
		let captured: AbortSignal | undefined;
		const list = vi.fn<AuditLogLister>((_params, _asOfId, signal) => {
			captured = signal;
			return new Promise<AuditLogList>(() => {});
		});
		const controller = new AbortController();
		void createAuditLogFetcher(list, LIMIT_MS)(request(), controller.signal);
		await Promise.resolve();
		expect(captured?.aborted).toBe(false);
		controller.abort();
		expect(captured?.aborted).toBe(true);
	});

	it('サーバーの失敗はそのまま投げる（握り潰さない）', async () => {
		const failure = new ProviderError({ kind: 'forbidden' });
		const list = vi.fn<AuditLogLister>(async () => {
			throw failure;
		});
		await expect(
			createAuditLogFetcher(list, LIMIT_MS)(request(), new AbortController().signal)
		).rejects.toBe(failure);
	});
});

describe('auditErrorText', () => {
	it('banto の境界の食い違い（英語の固定文）だけを日本語にする', () => {
		expect(
			auditErrorText(
				new ProviderError({ kind: 'other', message: SNAPSHOT_BOUNDARY_MISMATCH_MESSAGE })
			)
		).toBe(AUDIT_BOUNDARY_MISMATCH_MESSAGE);
		expect(
			auditErrorText(new ProviderError({ kind: 'other', message: 'サーバーに接続できません' }))
		).toBe('サーバーに接続できません');
	});
});

describe('listAuditLog', () => {
	afterEach(() => {
		env.mode = 'server';
		invokeMock.mockReset();
		vi.unstubAllGlobals();
	});

	const params: ListParams = { pagination: { offset: 0, limit: 200 }, sort: [], filters: [] };
	const empty: AuditLogList = { rows: [], totalCount: 0, asOfId: 7, deletionEpoch: 0 };

	it('REST: 境界があれば ?asOfId= を付け、無ければ付けない', async () => {
		const fetchMock = vi.fn(
			async () =>
				new Response(JSON.stringify(empty), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
		);
		vi.stubGlobal('fetch', fetchMock);

		await listAuditLog(params, null);
		await listAuditLog(params, 7);

		expect(fetchMock.mock.calls.map((call) => (call as unknown as [string])[0])).toEqual([
			'/api/audit-log/list',
			'/api/audit-log/list?asOfId=7'
		]);
	});

	it('Tauri: 境界があれば asOfId を渡し、無ければ渡さない', async () => {
		env.mode = 'tauri';
		invokeMock.mockResolvedValue(empty);

		await listAuditLog(params, null);
		await listAuditLog(params, 7);

		expect(invokeMock.mock.calls).toEqual([
			['audit_log_list', { params }],
			['audit_log_list', { params, asOfId: 7 }]
		]);
	});

	it('デモモードでは読まない', async () => {
		env.mode = 'demo';
		await expect(listAuditLog(params, null)).rejects.toBeInstanceOf(ProviderError);
	});
});

/**
 * banto の `SnapshotListResource` と組み合わせた確認。ブロックの合間に記録が
 * 増えても、同じ世代の 2 ブロック目は**最初の境界の集合**から読まれ、境界の
 * 行と重ならない。`createAuditLogFetcher` が `asOfId` を落とすと、偽の
 * サーバーは新しい境界で答え、リソースは境界の食い違いとして 2 ブロック目を
 * 採らない（このテストが落ちる）。
 */
describe('SnapshotListResource と組み合わせて', () => {
	/** id 降順で返す偽のサーバー（`audit_log_router` と同じ境界の約束）。 */
	function fakeServer(initial: number) {
		let maxId = initial;
		const list = vi.fn<AuditLogLister>(async (params, asOfId) => {
			const boundary = asOfId ?? maxId;
			const ids: number[] = [];
			for (let id = boundary; id >= 1; id--) ids.push(id);
			const { offset, limit } = params.pagination ?? { offset: 0, limit: 200 };
			return {
				rows: ids.slice(offset, offset + limit).map(entry),
				totalCount: ids.length,
				asOfId: boundary,
				deletionEpoch: 0
			};
		});
		return {
			list,
			record(count: number) {
				maxId += count;
			}
		};
	}

	async function settle(): Promise<void> {
		for (let i = 0; i < 10; i++) await Promise.resolve();
		await new Promise((resolve) => setTimeout(resolve, 0));
	}

	it('ブロックの合間に記録が増えても、2 ブロック目は最初の境界から読まれ、重複しない', async () => {
		const server = fakeServer(300);
		const resource = createSnapshotListResource<AuditLogEntry>(
			createAuditLogFetcher(server.list, LIMIT_MS),
			{ params: { sort: [{ field: 'ts', direction: 'desc' }], filters: [] } }
		);
		try {
			resource.ensureRange(0, 100);
			await settle();
			expect(resource.totalCount).toBe(300);
			expect(resource.rows[0]?.id).toBe(300);

			// 一覧を開いた後に 5 件記録された。
			server.record(5);

			resource.ensureRange(150, 260);
			await settle();

			expect(resource.error).toBeNull();
			expect(resource.failedBlocks).toEqual([]);
			expect(resource.totalCount).toBe(300);
			expect(resource.rows[199]?.id).toBe(101);
			expect(resource.rows[200]?.id).toBe(100);
			const ids = resource.rows.filter((row) => row !== undefined).map((row) => row.id);
			expect(new Set(ids).size).toBe(ids.length);
			expect(server.list.mock.calls[1]?.[1]).toBe(300);

			// 「再読み込み」= 新しい世代で、増えた分が入る。
			resource.refresh();
			await settle();
			expect(resource.totalCount).toBe(305);
			expect(resource.rows[0]?.id).toBe(305);
		} finally {
			resource.dispose();
		}
	});
});
