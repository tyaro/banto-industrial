/**
 * 振り分けの制約（設計 §3.1）の試験。差し替える先は試験ごとの物（`globalThis` は触らない）。
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
	classify,
	idleEventStream,
	installDemoBackend,
	json,
	matchPath,
	type DemoBackendHandle,
	type DemoGlobals,
	type DemoRoutes
} from './index';

const ORIGIN = 'https://demo.example';
const BASE = '/banto-industrial/hub';

let handle: DemoBackendHandle | undefined;
afterEach(() => {
	handle?.uninstall();
	handle = undefined;
});

/** 差し替える先。`fetch` は呼ばれたら記録する本物の代わり。 */
function setup(routes: DemoRoutes) {
	const original = vi.fn(async () => new Response('asset', { status: 200 }));
	const RealWebSocket = vi.fn();
	const RealEventSource = vi.fn();
	const target: DemoGlobals = {
		fetch: original as unknown as typeof fetch,
		WebSocket: RealWebSocket as unknown as typeof WebSocket,
		EventSource: RealEventSource as unknown as typeof EventSource,
		location: { origin: ORIGIN }
	};
	handle = installDemoBackend(routes, { basePath: BASE, target });
	return { target, original, RealWebSocket, RealEventSource };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 5));

describe('classify', () => {
	it('/api/ はオリジンを問わず api', () => {
		expect(classify(new URL(`${ORIGIN}/api/tags`), ORIGIN, BASE)).toBe('api');
		expect(classify(new URL('https://other.example/api/tags'), ORIGIN, BASE)).toBe('api');
		expect(classify(new URL(`${ORIGIN}/api`), ORIGIN, BASE)).toBe('api');
		expect(classify(new URL(`${ORIGIN}/apix`), ORIGIN, BASE)).toBe('blocked');
	});

	it('ベースパスの下の拡張子のある資産だけ static', () => {
		expect(classify(new URL(`${ORIGIN}${BASE}/_app/immutable/a.js`), ORIGIN, BASE)).toBe('static');
		expect(classify(new URL(`${ORIGIN}${BASE}/manifest.json`), ORIGIN, BASE)).toBe('static');
		expect(classify(new URL(`${ORIGIN}${BASE}/tags`), ORIGIN, BASE)).toBe('blocked');
		expect(classify(new URL(`${ORIGIN}/other/a.js`), ORIGIN, BASE)).toBe('blocked');
		expect(classify(new URL(`${ORIGIN}/banto-industrial/hubx/a.js`), ORIGIN, BASE)).toBe('blocked');
		expect(classify(new URL(`https://cdn.example${BASE}/a.js`), ORIGIN, BASE)).toBe('blocked');
	});
});

describe('matchPath', () => {
	it(':名前 は 1 区切りだけ受ける', () => {
		expect(matchPath('/api/ui-settings/:key', '/api/ui-settings/theme')).toEqual({ key: 'theme' });
		expect(matchPath('/api/ui-settings/:key', '/api/ui-settings/a/b')).toBeNull();
		expect(matchPath('/api/ui-settings/:key', '/api/ui-settings/')).toBeNull();
		expect(matchPath(/\/api\/x\/(?<id>\d+)/, '/api/x/12')).toEqual({ id: '12' });
		expect(matchPath(/\/api\/x/, '/api/x/12')).toBeNull();
	});
});

describe('fetch', () => {
	it('口のある /api は模擬が答え、元の fetch を呼ばない', async () => {
		const { target, original } = setup({
			http: [
				{
					method: 'POST',
					path: '/api/echo/:id',
					handle: async (req) => json({ id: req.params.id, body: await req.json() })
				}
			]
		});
		const res = await target.fetch('/api/echo/7', { method: 'POST', body: '{"a":1}' });
		expect(res.status).toBe(200);
		expect(await res.json()).toEqual({ id: '7', body: { a: 1 } });
		expect(original).not.toHaveBeenCalled();
	});

	it('文字列・URL・Request のどれでも同じ判定', async () => {
		const { target, original } = setup({
			http: [{ method: 'GET', path: '/api/ping', handle: () => json('pong') }]
		});
		const forms: Array<RequestInfo | URL> = [
			'/api/ping',
			`${ORIGIN}/api/ping`,
			new URL('/api/ping', ORIGIN),
			new Request(`${ORIGIN}/api/ping`)
		];
		for (const input of forms) {
			const res = await target.fetch(input);
			expect(await res.json()).toBe('pong');
		}
		// 口の無い /api も、どの形でも 501。
		for (const input of [
			'/api/none',
			new URL(`${ORIGIN}/api/none`),
			new Request(`${ORIGIN}/api/none`)
		]) {
			expect((await target.fetch(input)).status).toBe(501);
		}
		// ベースパス外は、どの形でも失敗。
		for (const input of ['/other', new URL(`${ORIGIN}/other`), new Request(`${ORIGIN}/other`)]) {
			await expect(target.fetch(input)).rejects.toBeInstanceOf(TypeError);
		}
		expect(original).not.toHaveBeenCalled();
	});

	it('口の無い /api は 501 と demo_unsupported（別のオリジンの /api も）で、元の fetch を呼ばない', async () => {
		const { target, original } = setup({ http: [] });
		for (const url of [
			'/api/tags',
			'https://other.example/api/tags',
			'http://127.0.0.1:8722/api/x'
		]) {
			const res = await target.fetch(url, { method: 'GET' });
			expect(res.status).toBe(501);
			expect(await res.json()).toEqual({ error: 'demo_unsupported' });
		}
		expect(original).not.toHaveBeenCalled();
	});

	it('メソッドが合わない口は口が無いのと同じ（501）', async () => {
		const { target } = setup({
			http: [{ method: 'GET', path: '/api/ping', handle: () => json('pong') }]
		});
		expect((await target.fetch('/api/ping', { method: 'DELETE' })).status).toBe(501);
	});

	it('同じオリジンのベースパスの下の静的な資産だけ元の fetch へ渡す', async () => {
		const { target, original } = setup({ http: [] });
		const res = await target.fetch(`${BASE}/_app/immutable/entry/start.js`);
		expect(await res.text()).toBe('asset');
		await target.fetch(`${ORIGIN}${BASE}/favicon.png`);
		expect(original).toHaveBeenCalledTimes(2);
	});

	it('別のオリジン・ベースパスの外・拡張子の無いパスは失敗し、元の fetch を呼ばない', async () => {
		const { target, original } = setup({ http: [] });
		for (const url of [
			'https://other.example/app.js',
			'https://fonts.example/a.woff2',
			'/banto-industrial/chronogazer/a.js',
			`${BASE}/tags`,
			`${BASE}`,
			'data:text/plain,hi'
		]) {
			await expect(target.fetch(url)).rejects.toBeInstanceOf(TypeError);
		}
		expect(original).not.toHaveBeenCalled();
	});

	it('口の失敗は 500 にする', async () => {
		const { target } = setup({
			http: [
				{
					method: 'GET',
					path: '/api/boom',
					handle: () => {
						throw new Error('boom');
					}
				}
			]
		});
		const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
		expect((await target.fetch('/api/boom')).status).toBe(500);
		spy.mockRestore();
	});

	it('idleEventStream は何も送らず、止めると読み出しが終わる', async () => {
		const { target } = setup({
			http: [{ method: 'GET', path: '/api/events', handle: (req) => idleEventStream(req.signal) }]
		});
		const controller = new AbortController();
		const res = await target.fetch('/api/events', { signal: controller.signal });
		expect(res.headers.get('content-type')).toBe('text/event-stream');
		const reader = res.body!.getReader();
		const read = reader.read();
		let settled = false;
		void read.then(
			() => (settled = true),
			() => (settled = true)
		);
		await tick();
		expect(settled).toBe(false);
		controller.abort();
		await expect(read).rejects.toBeDefined();
	});

	it('二重に入れると投げる', () => {
		const { target } = setup({ http: [] });
		expect(() => installDemoBackend({ http: [] }, { basePath: BASE, target })).toThrow();
	});
});

describe('WebSocket', () => {
	it('本物を作らない。口の無いパスは開かずに error と close', async () => {
		const { target, RealWebSocket } = setup({ http: [] });
		const ws = new target.WebSocket!('wss://demo.example/api/v1/stream');
		const events: string[] = [];
		ws.onopen = () => events.push('open');
		ws.onerror = () => events.push('error');
		ws.addEventListener('close', (e) => events.push(`close:${(e as CloseEvent).code}`));
		await vi.waitFor(() => expect(events).toEqual(['error', 'close:1006']));
		expect(ws.readyState).toBe(3);
		expect(RealWebSocket).not.toHaveBeenCalled();
	});

	it('口のあるパスは開き、模擬と message をやり取りできる', async () => {
		const received: string[] = [];
		const { target, RealWebSocket } = setup({
			http: [],
			sockets: [
				{
					path: '/api/v1/stream',
					connect(conn) {
						conn.onMessage((data) => {
							received.push(data);
							conn.send(`echo:${data}`);
						});
					}
				}
			]
		});
		const ws = new target.WebSocket!('ws://anywhere.example/api/v1/stream');
		const messages: string[] = [];
		ws.onmessage = (e) => messages.push(String(e.data));
		await new Promise<void>((resolve) => ws.addEventListener('open', () => resolve()));
		ws.send('hi');
		await vi.waitFor(() => expect(messages).toEqual(['echo:hi']));
		expect(received).toEqual(['hi']);
		ws.close();
		await vi.waitFor(() => expect(ws.readyState).toBe(3));
		expect(RealWebSocket).not.toHaveBeenCalled();
	});
});

describe('EventSource', () => {
	it('本物を作らない。口の無いパスは error で CLOSED', async () => {
		const { target, RealEventSource } = setup({ http: [] });
		const es = new target.EventSource!('/api/events');
		const events: string[] = [];
		es.onerror = () => events.push('error');
		await vi.waitFor(() => expect(events).toEqual(['error']));
		expect(es.readyState).toBe(2);
		expect(RealEventSource).not.toHaveBeenCalled();
	});

	it('口のあるパスは開き、模擬から送れる', async () => {
		const { target, RealEventSource } = setup({
			http: [],
			eventSources: [{ path: '/api/events', connect: (conn) => conn.send('hello') }]
		});
		const es = new target.EventSource!('/api/events');
		const messages: string[] = [];
		es.onmessage = (e) => messages.push(String(e.data));
		await vi.waitFor(() => expect(messages).toEqual(['hello']));
		expect(es.readyState).toBe(1);
		es.close();
		expect(es.readyState).toBe(2);
		expect(RealEventSource).not.toHaveBeenCalled();
	});
});

describe('uninstall', () => {
	it('元に戻す', () => {
		const { target, original } = setup({ http: [] });
		handle!.uninstall();
		handle = undefined;
		expect(target.fetch).toBe(original);
	});
});
