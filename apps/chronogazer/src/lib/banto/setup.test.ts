/**
 * `setup.ts` の `bantoReady` が起動時の環境判定（banto v3.0.0 #286、I2c）を
 * 本当に使っているかの確認。判定そのもの（`probeBackend` の 3 値と
 * `resolveStartupTarget` の流れ）は admin-template の写しの
 * `startup.test.ts` が固定している。ここは chronogazer の合成ルート
 * （`setup.ts`）を、**本物の** `environment.ts` / `startup.ts` /
 * `startupState.svelte.ts` の上で動かし、次を見る:
 *
 * - LAN/REST のビルドで `/api/auth/check` に一時的に届かない（fetch の例外・
 *   逆プロキシの `503`・Banto の本文を持たない `500`）とき、demo に落ちない。
 *   `initBanto` を呼ばずに起動待ちのまま「サーバーに接続できません」になり、
 *   届くようになってから「再接続」すると server になる。以前の
 *   `isEmbeddedServer()` はこの 3 つを全部「サーバー無し」と読んで demo
 *   （メモリ上の空データ）に落ち、二度と戻らなかった。
 * - `VITE_BANTO_DEMO=1` のビルドは probe せずに demo。
 * - 同じオリジンが「`/api` は無い」と確定的に答えた（静的ホストの `404`）
 *   ときも demo（admin-template と同じ。`vite dev`/`vite preview` の経路）。
 *
 * `initBanto` / `connectEvents` だけ差し替える（プロバイダーの配線そのもの
 * ではなく、どの経路を選んだかを見るため）。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { STARTUP_AUTO_RETRIES, STARTUP_RETRY_DELAY_MS } from './startup';

const core = vi.hoisted(() => ({ initBanto: vi.fn(), connectEvents: vi.fn() }));
vi.mock('@banto/admin-core', async (importOriginal) => ({
	...(await importOriginal<typeof import('@banto/admin-core')>()),
	initBanto: core.initBanto,
	connectEvents: core.connectEvents
}));

const json = (status: number, body: unknown) =>
	new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

/** 自動再試行（`STARTUP_AUTO_RETRIES` 回、間隔 `STARTUP_RETRY_DELAY_MS`）を進める。 */
async function runAutoRetries(): Promise<void> {
	for (let i = 0; i < STARTUP_AUTO_RETRIES; i++) {
		await vi.advanceTimersByTimeAsync(STARTUP_RETRY_DELAY_MS);
	}
}

async function loadSetup() {
	const setup = await import('./setup');
	const state = await import('./startupState.svelte');
	let settled = false;
	void setup.bantoReady.then(() => {
		settled = true;
	});
	return { setup, state, settled: () => settled };
}

beforeEach(() => {
	vi.resetModules();
	vi.useFakeTimers();
	core.initBanto.mockReset();
	core.connectEvents.mockReset();
	vi.stubGlobal('location', { origin: 'http://lan.test' });
});

afterEach(() => {
	vi.useRealTimers();
	vi.unstubAllGlobals();
	vi.unstubAllEnvs();
});

describe('bantoReady: 一時的に届かないときは demo に落ちない（#286）', () => {
	const transient: [string, () => Promise<Response>][] = [
		[
			'fetch の例外（サーバーが止まっている）',
			async () => {
				throw new TypeError('fetch failed');
			}
		],
		['逆プロキシの HTML 503', async () => new Response('<html>', { status: 503 })],
		['Banto の本文を持たない 500', async () => json(500, { error: 'x' })]
	];

	for (const [label, failure] of transient) {
		it(`${label}: 起動待ちのまま「接続できません」になり、再接続で server になる`, async () => {
			let up = false;
			const fetchMock = vi.fn(async () => (up ? json(200, false) : failure()));
			vi.stubGlobal('fetch', fetchMock);

			const { setup, state, settled } = await loadSetup();
			await runAutoRetries();

			expect(fetchMock).toHaveBeenCalledTimes(STARTUP_AUTO_RETRIES + 1);
			expect(state.startupState.status).toBe('unreachable');
			expect(settled()).toBe(false);
			expect(core.initBanto).not.toHaveBeenCalled();

			// 利用者が「再接続」を押すまでは再試行しない（叩き続けない）。
			await vi.advanceTimersByTimeAsync(STARTUP_RETRY_DELAY_MS * 10);
			expect(fetchMock).toHaveBeenCalledTimes(STARTUP_AUTO_RETRIES + 1);

			up = true;
			state.retryStartup();
			await setup.bantoReady;

			expect(setup.getBantoMode()).toBe('server');
			expect(core.initBanto).toHaveBeenCalledTimes(1);
			expect(core.connectEvents).toHaveBeenCalledTimes(1);
		});
	}

	it('自動再試行のうちに届けば「接続できません」を出さずに server', async () => {
		let calls = 0;
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => {
				calls += 1;
				if (calls === 1) throw new TypeError('fetch failed');
				return json(401, { kind: 'unauthorized' });
			})
		);

		const { setup, state } = await loadSetup();
		await runAutoRetries();
		await setup.bantoReady;

		expect(setup.getBantoMode()).toBe('server');
		expect(state.startupState.status).toBe('connecting');
	});
});

describe('bantoReady: demo になる経路', () => {
	it('VITE_BANTO_DEMO=1 のビルドは probe せずに demo', async () => {
		vi.stubEnv('VITE_BANTO_DEMO', '1');
		const fetchMock = vi.fn(async () => json(200, false));
		vi.stubGlobal('fetch', fetchMock);

		const { setup } = await loadSetup();
		await setup.bantoReady;

		expect(setup.getBantoMode()).toBe('demo');
		expect(fetchMock).not.toHaveBeenCalled();
		expect(core.initBanto).toHaveBeenCalledTimes(1);
		expect(core.connectEvents).not.toHaveBeenCalled();
	});

	it('静的ホストの 404（「/api は無い」と確定）は demo', async () => {
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => new Response('<html>', { status: 404 }))
		);

		const { setup } = await loadSetup();
		await setup.bantoReady;

		expect(setup.getBantoMode()).toBe('demo');
		expect(core.connectEvents).not.toHaveBeenCalled();
	});

	it('VITE_BANTO_DEMO が 1 以外なら demo ビルドとは扱わない（server が答えれば server）', async () => {
		vi.stubEnv('VITE_BANTO_DEMO', '0');
		vi.stubGlobal(
			'fetch',
			vi.fn(async () => json(200, false))
		);

		const { setup } = await loadSetup();
		await setup.bantoReady;

		expect(setup.getBantoMode()).toBe('server');
	});
});
