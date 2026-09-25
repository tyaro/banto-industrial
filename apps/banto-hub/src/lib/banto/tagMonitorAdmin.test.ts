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
import {
	cellDisplayMode,
	initialStreamView,
	monitorCellDisplay,
	streamViewReducer,
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
