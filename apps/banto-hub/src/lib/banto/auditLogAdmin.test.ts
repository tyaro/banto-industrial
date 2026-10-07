/**
 * `auditLogAdmin.ts` のユニットテスト。監査ログ画面のブロック読み込みは
 * banto の `createSnapshotListResource`（banto #248）に任せ、ここで固定するのは
 * banto-hub 側の継ぎ目だけ:
 *
 * - `createAuditLogFetcher` が要求の並べ替え・絞り込み・ページングと**境界
 *   （`asOfId`）**、リソースの `signal` を**そのまま**サーバーへ渡すこと
 *   （境界を落とすとブロックの合間の追加で行がずれる - 下の「リソースと
 *   組み合わせて」で実際に確かめる）、
 * - `createAuditLogResource` の上限（15 秒）と、リソース自身が作る失敗
 *   （上限切れ・境界の食い違い・応答の形の不正）の日本語の文言（banto v5.0.0
 *   の `messages`）。本物の `createSnapshotListResource` で確かめる、
 * - `listAuditLog` の URL（`?asOfId=`）。
 *
 * 無応答は「解決しない Promise」と `vi.useFakeTimers()` で作る。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	isListBlockError,
	ProviderError,
	type ListParams,
	type SnapshotListRequest
} from '@banto/admin-core';

// `./setup` は読み込んだだけで接続を始める（`connectEvents`）ので、使う定数だけを
// 差し替える。トークンは持たない（`Authorization` を付けない）。
vi.mock('./setup', () => ({
	CSRF_HEADER: { 'X-Banto-Client': 'banto' }
}));
vi.mock('@banto/admin-core', async (importOriginal) => ({
	...(await importOriginal<typeof import('@banto/admin-core')>()),
	getAuthProvider: () => ({ getToken: () => null })
}));

import {
	AUDIT_BOUNDARY_MISMATCH_MESSAGE,
	AUDIT_MALFORMED_MESSAGE,
	auditListTimeoutMessage,
	createAuditLogFetcher,
	createAuditLogResource,
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
		origin: 'rest',
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
	it('並べ替え・絞り込み・ページングと境界（asOfId）をそのまま渡す', async () => {
		const answer: AuditLogList = { rows: [entry(1)], totalCount: 1, asOfId: 42, deletionEpoch: 0 };
		const list = vi.fn<AuditLogLister>(async () => answer);
		const fetcher = createAuditLogFetcher(list);

		const req = request();
		await expect(fetcher(req, new AbortController().signal)).resolves.toBe(answer);

		expect(list).toHaveBeenCalledTimes(1);
		const [params, asOfId] = list.mock.calls[0] ?? [];
		expect(params).toEqual({ pagination: req.pagination, sort: req.sort, filters: req.filters });
		expect(asOfId).toBe(42);
	});

	it('世代の最初（asOfId: null）は null のまま渡す', async () => {
		const list = vi.fn<AuditLogLister>(async () => ({ rows: [], totalCount: 0, asOfId: 0 }));
		await createAuditLogFetcher(list)(request({ asOfId: null }), new AbortController().signal);
		expect(list.mock.calls[0]?.[1]).toBeNull();
	});

	it('リソースの signal（上限切れ・新しい世代・dispose）をそのまま渡す', async () => {
		const list = vi.fn<AuditLogLister>(async () => ({ rows: [], totalCount: 0, asOfId: 0 }));
		const controller = new AbortController();
		await createAuditLogFetcher(list)(request(), controller.signal);
		expect(list.mock.calls[0]?.[2]).toBe(controller.signal);
	});

	it('サーバーの失敗はそのまま投げる（握り潰さない）', async () => {
		const failure = new ProviderError({ kind: 'forbidden' });
		const list = vi.fn<AuditLogLister>(async () => {
			throw failure;
		});
		await expect(createAuditLogFetcher(list)(request(), new AbortController().signal)).rejects.toBe(
			failure
		);
	});
});

describe('listAuditLog', () => {
	afterEach(() => {
		vi.unstubAllGlobals();
	});

	it('境界があれば ?asOfId= を付け、無ければ付けない', async () => {
		const fetchMock = vi.fn(
			async () =>
				new Response(JSON.stringify({ rows: [], totalCount: 0, asOfId: 7, deletionEpoch: 0 }), {
					status: 200,
					headers: { 'Content-Type': 'application/json' }
				})
		);
		vi.stubGlobal('fetch', fetchMock);
		const params: ListParams = { pagination: { offset: 0, limit: 200 }, sort: [], filters: [] };

		await listAuditLog(params, null);
		await listAuditLog(params, 7);

		expect(fetchMock.mock.calls.map((call) => (call as unknown as [string])[0])).toEqual([
			'/api/audit-log/list',
			'/api/audit-log/list?asOfId=7'
		]);
	});
});

/**
 * `createAuditLogResource`（本物の `createSnapshotListResource`）と組み合わせた
 * 確認。ブロックの合間に記録が増えても、同じ世代の 2 ブロック目は**最初の
 * 境界の集合**から読まれ、境界の行と重ならない。`createAuditLogFetcher` が
 * `asOfId` を落とすと、偽のサーバーは新しい境界で答え、リソースは境界の
 * 食い違いとして 2 ブロック目を採らない（下の 1 本目が落ちる）。`messages` を
 * 渡し忘れると、上限切れ・境界の食い違い・応答の形の不正が英語の既定の文言に
 * なる（下の 2〜4 本目が落ちる）。
 */
describe('SnapshotListResource と組み合わせて', () => {
	afterEach(() => {
		vi.useRealTimers();
	});

	const PARAMS = { sort: [{ field: 'ts', direction: 'desc' as const }], filters: [] };

	/**
	 * id 降順で返す偽のサーバー（`audit_log_router` と同じ境界の約束）。
	 * `ignoreBoundary` は要求の `asOfId` を無視して、その時点の最大 id を境界に
	 * 答える（境界の食い違いを起こす）。
	 */
	function fakeServer(initial: number, ignoreBoundary = false) {
		let maxId = initial;
		const list = vi.fn<AuditLogLister>(async (params, asOfId) => {
			const boundary = ignoreBoundary ? maxId : (asOfId ?? maxId);
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
		const resource = createAuditLogResource(PARAMS, server.list);
		try {
			resource.ensureRange(0, 100);
			await settle();
			expect(resource.totalCount).toBe(300);
			expect(resource.rows[0]?.id).toBe(300);

			// 一覧を開いた後に 5 件記録された。
			server.record(5);

			resource.ensureRange(150, 260);
			await settle();

			expect(resource.failures).toEqual([]);
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

	it('15 秒以内に返ってこないブロックは code: timeout・日本語の文言で失敗し、要求を中断する', async () => {
		vi.useFakeTimers();
		let captured: AbortSignal | undefined;
		const list = vi.fn<AuditLogLister>((_params, _asOfId, signal) => {
			captured = signal;
			return new Promise<AuditLogList>(() => {});
		});
		const resource = createAuditLogResource(PARAMS, list);
		try {
			resource.ensureRange(0, 100);
			// banto の既定（30 秒）ではなく 15 秒で切れる。
			await vi.advanceTimersByTimeAsync(LIMIT_MS - 1);
			expect(resource.failures).toEqual([]);
			expect(captured?.aborted).toBe(false);

			await vi.advanceTimersByTimeAsync(1);
			const failure = resource.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.code).toBe('timeout');
			expect(isListBlockError(failure.error)).toBe(true);
			expect(failure.error.message).toBe(auditListTimeoutMessage(LIMIT_MS));
			expect(failure.error.message).toContain('15秒以内に返りませんでした');
			expect(captured?.aborted).toBe(true);
			expect(resource.totalCount).toBeNull();
		} finally {
			resource.dispose();
		}
	});

	it('境界の食い違い（サーバーが asOfId を無視した）は日本語の文言で失敗する', async () => {
		const server = fakeServer(300, true);
		const resource = createAuditLogResource(PARAMS, server.list);
		try {
			resource.ensureRange(0, 100);
			await settle();
			server.record(5);
			resource.ensureRange(150, 260);
			await settle();

			expect(resource.failures).toHaveLength(1);
			const failure = resource.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.block).toBe(1);
			expect(failure.code).toBe('boundaryMismatch');
			expect(failure.error.message).toBe(AUDIT_BOUNDARY_MISMATCH_MESSAGE);
			// 食い違った応答の行は採らない。
			expect(resource.rows[200]).toBeUndefined();
		} finally {
			resource.dispose();
		}
	});

	it('形の正しくない応答は日本語の文言で失敗する', async () => {
		const list = vi.fn<AuditLogLister>(
			async () => ({ rows: 'broken', totalCount: 1, asOfId: 1 }) as unknown as AuditLogList
		);
		const resource = createAuditLogResource(PARAMS, list);
		try {
			resource.ensureRange(0, 100);
			await settle();
			const failure = resource.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.code).toBe('malformed');
			expect(failure.error.message).toBe(AUDIT_MALFORMED_MESSAGE);
		} finally {
			resource.dispose();
		}
	});

	it('サーバーの失敗は code: request で、サーバーの ProviderError のまま残る', async () => {
		const serverError = new ProviderError({ kind: 'storage', message: 'データベースを読めません' });
		const list = vi.fn<AuditLogLister>(async () => {
			throw serverError;
		});
		const resource = createAuditLogResource(PARAMS, list);
		try {
			resource.ensureRange(0, 100);
			await settle();
			const failure = resource.failures[0];
			if (failure?.kind !== 'error') throw new Error('expected an error failure');
			expect(failure.code).toBe('request');
			expect(failure.error).toBe(serverError);
		} finally {
			resource.dispose();
		}
	});
});
