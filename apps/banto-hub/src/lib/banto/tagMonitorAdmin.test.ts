/**
 * `connectTagStream` の再接続の扱いのユニットテスト（#441）。
 *
 * 守りたいこと:
 * - close `1008` を受けたら**再接続しない**（新しい `WebSocket` を作らない）。
 *   `session_revoked` / `commissioning_ended` は `onHalt` に
 *   `recheckSession`、`api_key_*`・未知の理由文は `halt` を渡す。
 * - 通常の切断（`1006`・`1000`・`1013`）は従来どおり再接続する。
 * - 止まった購読は `resume()` で戻る（回復導線）。`disconnect()` の後の
 *   `resume()` は何もしない。
 * - #445: 再接続が続けて失敗したら（切れている間の失効はブラウザには `1006`
 *   にしか見えない）、`probeSession` を 1 回だけ呼ぶ。`session` なら再接続を
 *   続けて数え直す、`login` なら再接続をやめて `onHalt` の `recheckSession`
 *   へ合流する、`unverified` ならバックオフを守って続ける。
 *
 * ブラウザの `WebSocket` は偽物に差し替える。`$lib/session.svelte`（Svelte 5
 * rune）と `@banto/admin-core` のパッケージ入口はこの最小 vitest 構成では
 * 読めない（`commissioning.test.ts` の doc comment 参照）ので、使う部分だけ
 * 差し替える。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const session = vi.hoisted(() => ({ commissioningMode: false }));
const auth = vi.hoisted(() => ({ token: 'tok-1' as string | null }));

vi.mock('$lib/session.svelte', () => ({ sessionStore: session }));
vi.mock('./setup', () => ({ CSRF_HEADER: { 'X-Banto-Client': 'banto' } }));
vi.mock('@banto/admin-core', () => ({
	getAuthProvider: () => ({ getToken: () => auth.token }),
	ProviderError: class extends Error {}
}));

import { connectTagStream } from './tagMonitorAdmin';
import type { SessionProbeResult } from './streamClose';
import {
	cellDisplayMode,
	initialStreamView,
	monitorCellDisplay,
	streamViewReducer,
	type StreamHalt,
	type StreamViewEvent
} from './monitorStreamView';

class FakeWebSocket {
	static readonly CONNECTING = 0;
	static readonly OPEN = 1;
	static readonly CLOSING = 2;
	static readonly CLOSED = 3;
	static instances: FakeWebSocket[] = [];

	readyState = FakeWebSocket.CONNECTING;
	onopen: (() => void) | null = null;
	onmessage: ((ev: { data: unknown }) => void) | null = null;
	onclose: ((ev: { code: number; reason: string }) => void) | null = null;
	onerror: (() => void) | null = null;
	sent: string[] = [];

	constructor(
		readonly url: string,
		readonly protocols?: string[]
	) {
		FakeWebSocket.instances.push(this);
	}

	send(data: string): void {
		this.sent.push(data);
	}

	close(): void {
		this.readyState = FakeWebSocket.CLOSED;
	}

	/** サーバー側から開く。 */
	open(): void {
		this.readyState = FakeWebSocket.OPEN;
		this.onopen?.();
	}

	/** サーバー側（またはネットワーク）から閉じる。 */
	serverClose(code: number, reason = ''): void {
		this.readyState = FakeWebSocket.CLOSED;
		this.onclose?.({ code, reason });
	}
}

function latest(): FakeWebSocket {
	const ws = FakeWebSocket.instances.at(-1);
	if (!ws) throw new Error('WebSocket がまだ作られていない');
	return ws;
}

beforeEach(() => {
	vi.useFakeTimers();
	FakeWebSocket.instances = [];
	session.commissioningMode = false;
	auth.token = 'tok-1';
	vi.stubGlobal('WebSocket', FakeWebSocket);
	vi.stubGlobal('location', { protocol: 'http:', host: 'hub.test' });
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
});

function start() {
	const onHalt = vi.fn();
	const onStatusChange = vi.fn();
	const stream = connectTagStream(
		{ onData: () => {}, onConfigChanged: () => {}, onStatusChange, onHalt },
		() => ['*']
	);
	return { stream, onHalt, onStatusChange };
}

/** 再接続の最大バックオフ（30 秒）より十分長く進める。 */
const WELL_PAST_BACKOFF_MS = 120_000;

describe('connectTagStream: close 1008 で再接続しない（#441）', () => {
	it.each([
		['session_revoked', { kind: 'recheckSession', reason: 'session_revoked' }],
		['commissioning_ended', { kind: 'recheckSession', reason: 'commissioning_ended' }]
	])('1008 + %s は再接続せず、ログイン状態の確認を求める', (reason, expected) => {
		const { onHalt, onStatusChange } = start();
		expect(FakeWebSocket.instances).toHaveLength(1);
		latest().open();
		latest().serverClose(1008, reason);

		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(1);
		expect(onHalt).toHaveBeenCalledTimes(1);
		expect(onHalt).toHaveBeenCalledWith(expected);
		// 画面が「接続中」のまま残らない。
		expect(onStatusChange).toHaveBeenLastCalledWith(false, 1008);
	});

	it.each(['api_key_revoked', 'api_key_expired', 'api_key_tripped', 'api_key_not_found', 'x'])(
		'1008 + %s は再接続せず、理由を渡す',
		(reason) => {
			const { onHalt } = start();
			latest().open();
			latest().serverClose(1008, reason);

			vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
			expect(FakeWebSocket.instances).toHaveLength(1);
			expect(onHalt).toHaveBeenCalledTimes(1);
			expect(onHalt.mock.calls[0][0]).toMatchObject({ kind: 'halt', reason });
		}
	);

	it('試運転モードの /api/tag-stream でも 1008 で止まる', () => {
		session.commissioningMode = true;
		const { onHalt } = start();
		expect(latest().url).toBe('ws://hub.test/api/tag-stream');
		latest().open();
		latest().serverClose(1008, 'commissioning_ended');

		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(1);
		expect(onHalt).toHaveBeenCalledWith({
			kind: 'recheckSession',
			reason: 'commissioning_ended'
		});
	});

	it('resume() で止まった購読を再開する（すぐに接続し、購読し直す）', () => {
		const { stream } = start();
		latest().open();
		latest().serverClose(1008, 'api_key_revoked');
		expect(FakeWebSocket.instances).toHaveLength(1);

		stream.resume();
		expect(FakeWebSocket.instances).toHaveLength(2);
		expect(latest().url).toBe('ws://hub.test/api/v1/stream');
		expect(latest().protocols).toEqual(['bearer', 'tok-1']);
		latest().open();
		expect(JSON.parse(latest().sent[0])).toMatchObject({ op: 'subscribe', tags: ['*'] });

		// 2 回目の resume() は止まっていないので何もしない。
		stream.resume();
		expect(FakeWebSocket.instances).toHaveLength(2);
	});

	it('disconnect() の後の resume() は何もしない（画面を離れた後に確認が終わっても繋がない）', () => {
		const { stream } = start();
		latest().open();
		latest().serverClose(1008, 'session_revoked');
		stream.disconnect();
		stream.resume();
		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(1);
	});

	it('止まっていないときの resume() は何もしない', () => {
		const { stream } = start();
		latest().open();
		stream.resume();
		expect(FakeWebSocket.instances).toHaveLength(1);
	});
});

describe('connectTagStream: 通常の切断は従来どおり再接続する', () => {
	it.each([1006, 1000, 1001, 1011, 1013])('close %i は再接続する', (code) => {
		const { onHalt } = start();
		latest().open();
		latest().serverClose(code, '');
		expect(FakeWebSocket.instances).toHaveLength(1);

		// 初回のバックオフは 1 秒。
		vi.advanceTimersByTime(1000);
		expect(FakeWebSocket.instances).toHaveLength(2);
		expect(onHalt).not.toHaveBeenCalled();
	});

	it('1006 の理由文が session_revoked でも再接続する（コードで判断する）', () => {
		const { onHalt } = start();
		latest().open();
		latest().serverClose(1006, 'session_revoked');
		vi.advanceTimersByTime(1000);
		expect(FakeWebSocket.instances).toHaveLength(2);
		expect(onHalt).not.toHaveBeenCalled();
	});

	it('1008 で止まった後に resume() し、次の 1006 はまた再接続する', () => {
		const { stream, onHalt } = start();
		latest().open();
		latest().serverClose(1008, 'session_revoked');
		stream.resume();
		latest().open();
		latest().serverClose(1006, '');
		vi.advanceTimersByTime(1000);
		expect(FakeWebSocket.instances).toHaveLength(3);
		expect(onHalt).toHaveBeenCalledTimes(1);
	});
});

describe('connectTagStream + streamViewReducer: 停止中の表の表示（#441 レビュー対応）', () => {
	it('良好な値 → 未知の理由の 1008 → 最終受信値の表示 → 再接続ボタン → スナップショットで通常へ', () => {
		let view = initialStreamView();
		const dispatch = (event: StreamViewEvent) => {
			view = streamViewReducer(view, event);
		};
		// モニタ画面（`monitor/+page.svelte`）と同じ配線。
		const stream = connectTagStream(
			{
				onData: () => dispatch({ type: 'data' }),
				onConfigChanged: () => {},
				onStatusChange: (connected, code) =>
					dispatch(connected ? { type: 'connected' } : { type: 'disconnected', code }),
				onHalt: (action) => dispatch({ type: 'halted', action })
			},
			() => ['*']
		);
		const row = { v: 42, q: 'good', unit: null };
		const shown = () => monitorCellDisplay(row, cellDisplayMode(view));
		const snapshot = (v: number) =>
			latest().onmessage?.({
				data: JSON.stringify({ op: 'data', values: [{ tag: 'a', v, q: 'good', t: v }] })
			});

		latest().open();
		snapshot(42);
		expect(shown()).toEqual({ value: '42', qualityClass: 'good', qualityLabel: '良好' });

		latest().serverClose(1008, 'e2e_unknown');
		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(1);
		expect(shown()).toEqual({
			value: '42',
			qualityClass: 'stale',
			qualityLabel: '陳腐化（受信時: 良好）'
		});

		// 画面の「再接続」ボタン（`resumeStream`）と同じ順。
		dispatch({ type: 'resumed' });
		stream.resume();
		expect(FakeWebSocket.instances).toHaveLength(2);
		expect(shown().value).toBe('--');
		latest().open();
		expect(shown().value).toBe('--');
		snapshot(43);
		expect(shown()).toEqual({ value: '42', qualityClass: 'good', qualityLabel: '良好' });
	});
});

describe('connectTagStream: 切れている間の失効（#445、再接続が続けて失敗したときの確認）', () => {
	/** 再接続の待ち（1 秒 → 2 秒 → 4 秒 …、上限 30 秒）。 */
	const BACKOFF_MS = [1000, 2000, 4000, 8000, 16000, 30000, 30000];

	function deferred<T>() {
		let resolve: (value: T) => void = () => {};
		const promise = new Promise<T>((res) => {
			resolve = res;
		});
		return { promise, resolve };
	}

	function startWithProbe(probe: () => Promise<SessionProbeResult>) {
		const onHalt = vi.fn();
		const onStatusChange = vi.fn();
		const probeSession = vi.fn(probe);
		const stream = connectTagStream(
			{ onData: () => {}, onConfigChanged: () => {}, onStatusChange, onHalt, probeSession },
			() => ['*']
		);
		return { stream, onHalt, onStatusChange, probeSession };
	}

	/** 開いていた接続が 1006 で切れる（ここは失敗に数えない）。 */
	function dropOpenConnection(): void {
		latest().open();
		latest().serverClose(1006);
	}

	/** `attempt` 回目（0 始まり）の再接続を待ち、開く前に 1006 で閉じる（= 拒否された）。 */
	function rejectReconnect(attempt: number): void {
		const before = FakeWebSocket.instances.length;
		vi.advanceTimersByTime(BACKOFF_MS[attempt]);
		expect(FakeWebSocket.instances).toHaveLength(before + 1);
		latest().serverClose(1006);
	}

	const flush = () => vi.advanceTimersByTimeAsync(0);

	it('1006 が 2 回続いたら確認を 1 回だけ呼ぶ（確認中にまた失敗しても重ねない）', () => {
		const { probeSession } = startWithProbe(() => new Promise(() => {}));
		dropOpenConnection();
		expect(probeSession).not.toHaveBeenCalled();
		rejectReconnect(0);
		expect(probeSession).not.toHaveBeenCalled();
		rejectReconnect(1);
		expect(probeSession).toHaveBeenCalledTimes(1);
		rejectReconnect(2);
		rejectReconnect(3);
		expect(probeSession).toHaveBeenCalledTimes(1);
	});

	it('通常の一時的な切断（1 回拒否されてから繋がる）では確認を呼ばない', async () => {
		const { probeSession, onHalt } = startWithProbe(async () => 'login');
		dropOpenConnection();
		rejectReconnect(0);
		vi.advanceTimersByTime(BACKOFF_MS[1]);
		latest().open();
		// 開いたので数え直す: また 1 回だけの失敗なら確かめない。
		latest().serverClose(1006);
		rejectReconnect(0);
		await flush();
		expect(probeSession).not.toHaveBeenCalled();
		expect(onHalt).not.toHaveBeenCalled();
	});

	it('有効（session）なら再接続を続け、数え直す（次の確認はまた 2 回失敗してから）', async () => {
		const { probeSession, onHalt } = startWithProbe(async () => 'session');
		dropOpenConnection();
		rejectReconnect(0);
		rejectReconnect(1);
		expect(probeSession).toHaveBeenCalledTimes(1);
		await flush();

		rejectReconnect(2);
		expect(probeSession).toHaveBeenCalledTimes(1);
		rejectReconnect(3);
		expect(probeSession).toHaveBeenCalledTimes(2);
		await flush();
		expect(onHalt).not.toHaveBeenCalled();

		// 再接続は続いていて、繋がれば通常どおり。
		vi.advanceTimersByTime(BACKOFF_MS[4]);
		latest().open();
		expect(FakeWebSocket.instances).toHaveLength(6);
	});

	it('失効（login）なら再接続をやめ、1008 + session_revoked と同じ確認の経路へ合流する', async () => {
		let view = initialStreamView();
		const dispatch = (event: StreamViewEvent) => {
			view = streamViewReducer(view, event);
		};
		const probeSession = vi.fn(async (): Promise<SessionProbeResult> => 'login');
		const onHalt = vi.fn((action: StreamHalt) => dispatch({ type: 'halted', action }));
		connectTagStream(
			{
				onData: () => {},
				onConfigChanged: () => {},
				onStatusChange: (connected, code) =>
					dispatch(connected ? { type: 'connected' } : { type: 'disconnected', code }),
				onHalt,
				probeSession
			},
			() => ['*']
		);
		dropOpenConnection();
		rejectReconnect(0);
		rejectReconnect(1);
		// 確認の結果を待つ間も「接続中」ではない。
		expect(view.connected).toBe(false);
		await flush();

		expect(onHalt).toHaveBeenCalledTimes(1);
		expect(onHalt).toHaveBeenCalledWith({ kind: 'recheckSession', reason: 'reconnect_rejected' });
		expect(view.connected).toBe(false);
		expect(cellDisplayMode(view)).toBe('halted');

		const count = FakeWebSocket.instances.length;
		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(count);
		expect(probeSession).toHaveBeenCalledTimes(1);
	});

	it('失効が分かったときに開く途中のソケットがあれば捨て、resume() では 1 本だけ張り直す', async () => {
		const probe = deferred<SessionProbeResult>();
		const { stream, probeSession, onHalt } = startWithProbe(() => probe.promise);
		dropOpenConnection();
		rejectReconnect(0);
		rejectReconnect(1);
		expect(probeSession).toHaveBeenCalledTimes(1);
		// 確認の結果より先に、次の再接続が始まる（開く途中）。
		vi.advanceTimersByTime(BACKOFF_MS[2]);
		const connecting = latest();
		expect(connecting.readyState).toBe(FakeWebSocket.CONNECTING);

		probe.resolve('login');
		await flush();
		expect(onHalt).toHaveBeenCalledTimes(1);
		expect(connecting.readyState).toBe(FakeWebSocket.CLOSED);
		expect(connecting.onclose).toBeNull();
		const count = FakeWebSocket.instances.length;
		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(count);

		// ルートガードが「まだ有効」と判断したら、画面が resume() する。
		stream.resume();
		expect(FakeWebSocket.instances).toHaveLength(count + 1);
		vi.advanceTimersByTime(WELL_PAST_BACKOFF_MS);
		expect(FakeWebSocket.instances).toHaveLength(count + 1);
	});

	it.each([
		['unverified', async (): Promise<SessionProbeResult> => 'unverified'],
		['reject', (): Promise<SessionProbeResult> => Promise.reject(new Error('boom'))]
	])(
		'照合できない（%s）なら、バックオフを守って再接続を続け、確認は再接続の待ちごとに高々 1 回',
		async (_label, probe) => {
			const probeTimes: number[] = [];
			const { probeSession, onHalt } = startWithProbe(() => {
				probeTimes.push(Date.now());
				return probe();
			});
			dropOpenConnection();
			for (let attempt = 0; attempt < BACKOFF_MS.length; attempt++) {
				rejectReconnect(attempt);
				await flush();
			}
			expect(onHalt).not.toHaveBeenCalled();
			// 2 回目の失敗から毎回（= 再接続の待ちごとに 1 回）。
			expect(probeSession).toHaveBeenCalledTimes(BACKOFF_MS.length - 1);
			const gaps = probeTimes.slice(1).map((t, i) => t - probeTimes[i]);
			expect(gaps).toEqual(BACKOFF_MS.slice(2));
		}
	);

	it('確認の結果が返る前に接続が開いたら、その結果（login）で止めない', async () => {
		const probe = deferred<SessionProbeResult>();
		const { onHalt } = startWithProbe(() => probe.promise);
		dropOpenConnection();
		rejectReconnect(0);
		rejectReconnect(1);
		vi.advanceTimersByTime(BACKOFF_MS[2]);
		const socket = latest();
		socket.open();

		probe.resolve('login');
		await flush();
		expect(onHalt).not.toHaveBeenCalled();
		expect(socket.readyState).toBe(FakeWebSocket.OPEN);
	});

	it('disconnect() の後に返った結果では何もしない', async () => {
		const probe = deferred<SessionProbeResult>();
		const { stream, onHalt } = startWithProbe(() => probe.promise);
		dropOpenConnection();
		rejectReconnect(0);
		rejectReconnect(1);
		stream.disconnect();
		probe.resolve('login');
		await flush();
		expect(onHalt).not.toHaveBeenCalled();
	});
});
