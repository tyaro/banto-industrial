import { describe, expect, it } from 'vitest';
import {
	classifyLogFeedResponse,
	LogFeedPoller,
	logRowView,
	mergeLogEntries,
	type LogFeedEntry,
	type LogFeedFetchOutcome,
	type LogFeedSnapshot,
	type LogFeedTimers,
	type WriteLogEntry
} from './logFeed';

function conn(key: string, ts: number, kind = 'plc_connected'): LogFeedEntry {
	return {
		source: 'connection',
		id: 1,
		key,
		ts_ms: ts,
		kind,
		connection_id: 1,
		connection_name: 'PLC1',
		detail: null,
		transport: null,
		reason: null
	};
}

function write(key: string, ts: number, result: string): WriteLogEntry {
	return {
		source: 'write',
		id: 1,
		key,
		ts_ms: ts,
		change_seq: 1,
		action: 'write',
		result,
		tag_id: 1,
		tag_name: 'T1',
		value: 1,
		value_text: null,
		detail: null,
		api_key_id: 1,
		api_key_name: 'key1',
		api_key_prefix: null,
		correlation_id: null,
		operator_claim: null
	};
}

function op(key: string, ts: number, result = 'ok'): LogFeedEntry {
	return {
		source: 'operation',
		id: 1,
		key,
		ts_ms: ts,
		origin: 'rest',
		actor_username: 'admin',
		actor_role: 'admin',
		action: 'login',
		resource: 'auth',
		entity_id: null,
		result,
		detail: null
	};
}

const keys = (rows: readonly LogFeedEntry[]) => rows.map((r) => r.key);

describe('mergeLogEntries', () => {
	it('時刻の降順に並べ、後から届いた古い時刻の行は時刻の位置に差し込む', () => {
		const current = [conn('c:3', 300), conn('c:1', 100)];
		const merged = mergeLogEntries(current, [op('a:9', 200), conn('c:4', 400)], 100);
		expect(keys(merged)).toEqual(['c:4', 'c:3', 'a:9', 'c:1']);
	});

	it('同じ key は置き換える（暫定の failed → 確定）', () => {
		const current = [write('w:1', 100, 'failed'), conn('c:1', 50)];
		const merged = mergeLogEntries(current, [write('w:1', 100, 'ok')], 100);
		expect(merged).toHaveLength(2);
		expect((merged[0] as WriteLogEntry).result).toBe('ok');
	});

	it('同じ時刻は key の降順で決める', () => {
		const merged = mergeLogEntries([], [conn('c:1', 100), conn('c:2', 100)], 100);
		expect(keys(merged)).toEqual(['c:2', 'c:1']);
	});

	it('上限を超えたら古い時刻から捨てる', () => {
		const merged = mergeLogEntries(
			[conn('c:1', 100), conn('c:2', 200)],
			[conn('c:3', 300), conn('c:0', 50)],
			2
		);
		expect(keys(merged)).toEqual(['c:3', 'c:2']);
	});

	it('元の配列は変えない', () => {
		const current = [conn('c:1', 100)];
		mergeLogEntries(current, [conn('c:2', 200)], 10);
		expect(keys(current)).toEqual(['c:1']);
	});
});

describe('classifyLogFeedResponse', () => {
	const ok = { entries: [], cursor: 'x', more: false };
	it.each<[number, unknown, LogFeedFetchOutcome['kind']]>([
		[200, ok, 'ok'],
		[200, { entries: [] }, 'failed'],
		[409, { kind: 'conflict', current: { code: 'cursor_expired' } }, 'reset'],
		[409, { kind: 'conflict', current: { code: 'other' } }, 'failed'],
		[400, { kind: 'bad_request' }, 'reset'],
		[401, null, 'unauthorized'],
		[403, null, 'unauthorized'],
		[500, null, 'failed'],
		[503, null, 'failed']
	])('%i → %s', (status, body, kind) => {
		expect(classifyLogFeedResponse(status, body).kind).toBe(kind);
	});
});

/** 手で進める時計。 */
class ManualTimers implements LogFeedTimers {
	private next = 1;
	readonly pending = new Map<number, { fn: () => void; ms: number }>();
	setTimeout = (fn: () => void, ms: number): unknown => {
		const id = this.next++;
		this.pending.set(id, { fn, ms });
		return id;
	};
	clearTimeout = (handle: unknown): void => {
		this.pending.delete(handle as number);
	};
	/** 予約のうち、取得の期限（15 秒）を除いたもの。 */
	scheduled(): number[] {
		return [...this.pending.values()].filter((t) => t.ms !== 15_000).map((t) => t.ms);
	}
	/** 期限以外の予約を 1 つ走らせる。 */
	async runNext(): Promise<void> {
		const entry = [...this.pending.entries()].find(([, t]) => t.ms !== 15_000);
		if (!entry) throw new Error('予約がありません');
		this.pending.delete(entry[0]);
		entry[1].fn();
		await flush();
	}
}

async function flush(): Promise<void> {
	for (let i = 0; i < 10; i++) await Promise.resolve();
}

interface Call {
	cursor: string | null;
	signal: AbortSignal;
	resolve: (outcome: LogFeedFetchOutcome) => void;
}

function setup(options: { maxBurst?: number; maxRows?: number } = {}) {
	const timers = new ManualTimers();
	const calls: Call[] = [];
	const snapshots: LogFeedSnapshot[] = [];
	const poller = new LogFeedPoller({
		fetch: (cursor, signal) =>
			new Promise<LogFeedFetchOutcome>((resolve) => calls.push({ cursor, signal, resolve })),
		onChange: (s) => snapshots.push(s),
		timers,
		maxBurst: options.maxBurst,
		maxRows: options.maxRows
	});
	const last = () => snapshots[snapshots.length - 1] ?? poller.snapshot();
	async function answer(outcome: LogFeedFetchOutcome): Promise<void> {
		const call = calls[calls.length - 1];
		call.resolve(outcome);
		await flush();
	}
	const okResponse = (
		entries: LogFeedEntry[],
		cursor: string,
		more = false
	): LogFeedFetchOutcome => ({
		kind: 'ok',
		response: { entries, cursor, more }
	});
	return { timers, calls, poller, last, answer, okResponse };
}

describe('LogFeedPoller', () => {
	it('初回は cursor 無し、以後は前回の cursor を渡して既定 5 秒おき', async () => {
		const t = setup();
		t.poller.setActive(true);
		expect(t.timers.scheduled()).toEqual([0]);
		await t.timers.runNext();
		expect(t.calls[0].cursor).toBeNull();
		await t.answer(t.okResponse([conn('c:1', 100)], 'k1'));
		expect(keys(t.last().rows)).toEqual(['c:1']);
		expect(t.last().sources).toEqual(['connection']);
		expect(t.timers.scheduled()).toEqual([5000]);
		await t.timers.runNext();
		expect(t.calls[1].cursor).toBe('k1');
	});

	it('more: true ならすぐ続けて読み、続ける回数には上限がある', async () => {
		const t = setup({ maxBurst: 2 });
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer(t.okResponse([], 'k1', true));
		expect(t.timers.scheduled()).toEqual([0]);
		await t.timers.runNext();
		await t.answer(t.okResponse([], 'k2', true));
		expect(t.timers.scheduled()).toEqual([0]);
		await t.timers.runNext();
		await t.answer(t.okResponse([], 'k3', true));
		// 上限（2 回）に当たったら通常の間隔に戻る。
		expect(t.timers.scheduled()).toEqual([5000]);
		await t.timers.runNext();
		expect(t.calls[3].cursor).toBe('k3');
	});

	it.each<[string, LogFeedFetchOutcome]>([
		['409 cursor_expired', { kind: 'reset' }],
		['400', { kind: 'reset' }]
	])('%s では位置と行を全部捨て、すぐ初回から読み直す', async (_name, outcome) => {
		const t = setup();
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer(t.okResponse([op('a:1', 100), conn('c:1', 90)], 'k1'));
		expect(t.last().sources).toEqual(['connection', 'operation']);
		await t.timers.runNext();
		const epoch = t.last().epoch;
		await t.answer(outcome);
		expect(t.last().rows).toEqual([]);
		expect(t.last().sources).toEqual([]);
		expect(t.last().epoch).toBe(epoch + 1);
		expect(t.timers.scheduled()).toEqual([0]);
		await t.timers.runNext();
		expect(t.calls[2].cursor).toBeNull();
		// 続けて断られたら、詰めて読みに行かない（通常の間隔）。
		await t.answer({ kind: 'reset' });
		expect(t.timers.scheduled()).toEqual([5000]);
	});

	it('401 では止め、回し続けない（見せ直したら 1 回は試す）', async () => {
		const t = setup();
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer({ kind: 'unauthorized' });
		expect(t.last().status).toBe('unauthorized');
		expect(t.timers.scheduled()).toEqual([]);
		t.poller.setActive(false);
		t.poller.setActive(true);
		expect(t.timers.scheduled()).toEqual([0]);
	});

	it('失敗は間隔を倍々に延ばして再試行し、成功で戻す', async () => {
		const t = setup();
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer({ kind: 'failed', message: 'offline' });
		expect(t.last().status).toBe('retrying');
		expect(t.last().error).toBe('offline');
		expect(t.timers.scheduled()).toEqual([10_000]);
		await t.timers.runNext();
		await t.answer({ kind: 'failed', message: 'offline' });
		expect(t.timers.scheduled()).toEqual([20_000]);
		await t.timers.runNext();
		await t.answer(t.okResponse([], 'k1'));
		expect(t.last().status).toBe('ok');
		expect(t.last().error).toBeNull();
		expect(t.timers.scheduled()).toEqual([5000]);
	});

	it('失敗の間隔には上限がある', async () => {
		const t = setup();
		t.poller.setActive(true);
		for (let i = 0; i < 8; i++) {
			await t.timers.runNext();
			await t.answer({ kind: 'failed', message: 'x' });
		}
		expect(t.timers.scheduled()).toEqual([60_000]);
	});

	it('止めたら予約を消し、飛行中の応答は捨てる。再開は続きの位置から', async () => {
		const t = setup();
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer(t.okResponse([conn('c:1', 100)], 'k1'));
		await t.timers.runNext();
		const inflight = t.calls[1];
		t.poller.setActive(false);
		expect(inflight.signal.aborted).toBe(true);
		expect(t.timers.scheduled()).toEqual([]);
		inflight.resolve(t.okResponse([conn('c:2', 200)], 'k2'));
		await flush();
		// 捨てた応答は行にも位置にも効かない。
		expect(keys(t.last().rows)).toEqual(['c:1']);
		expect(t.last().active).toBe(false);
		t.poller.setActive(true);
		await t.timers.runNext();
		expect(t.calls[2].cursor).toBe('k1');
		// 行は残っている。
		expect(keys(t.last().rows)).toEqual(['c:1']);
	});

	it('消去は表示の行だけ捨て、位置は残す', async () => {
		const t = setup();
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer(t.okResponse([conn('c:1', 100)], 'k1'));
		t.poller.clear();
		expect(t.last().rows).toEqual([]);
		await t.timers.runNext();
		expect(t.calls[1].cursor).toBe('k1');
	});

	it('保持の上限を超えたら古い時刻から捨てる', async () => {
		const t = setup({ maxRows: 2 });
		t.poller.setActive(true);
		await t.timers.runNext();
		await t.answer(t.okResponse([conn('c:1', 100), conn('c:2', 200), conn('c:3', 300)], 'k1'));
		expect(keys(t.last().rows)).toEqual(['c:3', 'c:2']);
	});

	it('dispose の後は何もしない', async () => {
		const t = setup();
		t.poller.setActive(true);
		t.poller.dispose();
		expect(t.timers.scheduled()).toEqual([]);
		t.poller.setActive(true);
		expect(t.timers.scheduled()).toEqual([]);
	});

	it('取得が投げたら失敗として扱い、再試行する', async () => {
		const timers = new ManualTimers();
		const poller = new LogFeedPoller({
			fetch: () => Promise.reject(new Error('boom')),
			onChange: () => {},
			timers
		});
		poller.setActive(true);
		await timers.runNext();
		expect(poller.snapshot().status).toBe('retrying');
		expect(poller.snapshot().error).toBe('boom');
		expect(timers.scheduled()).toEqual([10_000]);
	});
});

describe('logRowView', () => {
	it('書き込みの失敗・不確定・拒否は目立たせ、成功は目立たせない', () => {
		expect(logRowView(write('w:1', 1, 'uncertain')).tone).toBe('alert');
		expect(logRowView(write('w:1', 1, 'failed')).tone).toBe('alert');
		expect(logRowView(write('w:1', 1, 'rejected')).tone).toBe('alert');
		expect(logRowView(write('w:1', 1, 'ok')).tone).toBe('normal');
		expect(logRowView(write('w:1', 1, 'ok')).who).toBe('key1');
	});

	it('接続の切断は目立たせる', () => {
		expect(logRowView(conn('c:1', 1, 'plc_disconnected')).tone).toBe('alert');
		expect(logRowView(conn('c:1', 1, 'ingest_disconnected')).tone).toBe('alert');
		expect(logRowView(conn('c:1', 1, 'plc_reconnected')).result).toBe('復帰');
		expect(logRowView(conn('c:1', 1, 'plc_reconnected')).tone).toBe('normal');
	});

	it('操作の行は誰と対象を出す', () => {
		const view = logRowView(op('a:1', 1, 'denied'));
		expect(view.who).toBe('admin');
		expect(view.target).toBe('auth');
		expect(view.tone).toBe('alert');
		expect(view.description).toContain('rest login');
	});
});
