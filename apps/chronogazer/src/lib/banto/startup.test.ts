// banto v3.0.0（タグ v3.0.0 = f0dcece）の admin-template
// `apps/admin-template/src/lib/banto/startup.test.ts` からコピー（I2c、banto #286）。
// chronogazer 固有の差: なし（本文は無改変）。chronogazer 固有のテストは `setup.test.ts`。
/**
 * Startup resolution (Issue #286): Tauri / LAN browser (server-served) /
 * intended demo, plus the transient-failure paths that must NOT become demo.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { probeBackend, PROBE_TIMEOUT_MS, type BackendProbe } from './environment';
import {
	resolveStartupTarget,
	STARTUP_AUTO_RETRIES,
	type StartupDeps,
	type StartupStatus
} from './startup';

function makeDeps(
	probes: BackendProbe[],
	over: Partial<StartupDeps> = {}
): { deps: StartupDeps; statuses: StartupStatus[]; probe: ReturnType<typeof vi.fn> } {
	const statuses: StartupStatus[] = [];
	const queue = [...probes];
	const probe = vi.fn(async () => queue.shift() ?? 'unreachable');
	const deps: StartupDeps = {
		isTauri: () => false,
		isDemoBuild: () => false,
		probe,
		sleep: async () => {},
		setStatus: (s) => statuses.push(s),
		waitForRetry: async () => {},
		...over
	};
	return { deps, statuses, probe };
}

describe('resolveStartupTarget', () => {
	it('Tauri: never probes', async () => {
		const { deps, probe } = makeDeps([], { isTauri: () => true });
		expect(await resolveStartupTarget(deps)).toBe('tauri');
		expect(probe).not.toHaveBeenCalled();
	});

	it('intended demo build: demo without probing, even if a backend would fail', async () => {
		const { deps, probe } = makeDeps(['unreachable'], { isDemoBuild: () => true });
		expect(await resolveStartupTarget(deps)).toBe('demo');
		expect(probe).not.toHaveBeenCalled();
	});

	it('static host answering "no API here" is the demo', async () => {
		const { deps } = makeDeps(['none']);
		expect(await resolveStartupTarget(deps)).toBe('demo');
	});

	it('LAN browser: Banto server answering is server mode', async () => {
		const { deps, statuses } = makeDeps(['server']);
		expect(await resolveStartupTarget(deps)).toBe('server');
		expect(statuses).toEqual(['connecting']);
	});

	it('transient failure then success: auto retry reaches server, never demo', async () => {
		const { deps, statuses, probe } = makeDeps(['unreachable', 'unreachable', 'server']);
		expect(await resolveStartupTarget(deps)).toBe('server');
		expect(probe).toHaveBeenCalledTimes(3);
		expect(statuses).not.toContain('unreachable');
	});

	it('persistent failure: shows unreachable, waits for the user, then server on retry', async () => {
		const failures = Array<BackendProbe>(STARTUP_AUTO_RETRIES + 1).fill('unreachable');
		const retried = vi.fn(async () => {});
		const { deps, statuses, probe } = makeDeps([...failures, 'server'], { waitForRetry: retried });
		expect(await resolveStartupTarget(deps)).toBe('server');
		expect(retried).toHaveBeenCalledTimes(1);
		expect(probe).toHaveBeenCalledTimes(STARTUP_AUTO_RETRIES + 2);
		expect(statuses).toEqual(['connecting', 'unreachable', 'connecting']);
	});

	it('does not resolve while the user has not retried', async () => {
		const failures = Array<BackendProbe>(STARTUP_AUTO_RETRIES + 1).fill('unreachable');
		const { deps } = makeDeps(failures, { waitForRetry: () => new Promise(() => {}) });
		const outcome = await Promise.race([
			resolveStartupTarget(deps),
			new Promise((r) => setTimeout(() => r('pending'), 20))
		]);
		expect(outcome).toBe('pending');
	});
});

describe('probeBackend', () => {
	afterEach(() => vi.unstubAllGlobals());

	function probeWith(impl: () => Promise<Response>, timeoutMs?: number) {
		vi.stubGlobal('location', { origin: 'http://lan.test' });
		return probeBackend(vi.fn(impl) as unknown as typeof fetch, timeoutMs);
	}
	const json = (status: number, body: unknown) =>
		new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } });

	it('200 / 401 -> server', async () => {
		expect(await probeWith(async () => json(200, true))).toBe('server');
		expect(await probeWith(async () => json(401, { kind: 'unauthorized' }))).toBe('server');
	});

	it('Banto JSON error body (#204) -> server', async () => {
		expect(await probeWith(async () => json(500, { kind: 'internal' }))).toBe('server');
	});

	it('static host 404 HTML -> none (demo)', async () => {
		expect(await probeWith(async () => new Response('<html>', { status: 404 }))).toBe('none');
	});

	it('network error -> unreachable', async () => {
		expect(
			await probeWith(async () => {
				throw new TypeError('fetch failed');
			})
		).toBe('unreachable');
	});

	it('reverse-proxy HTML 503 -> unreachable', async () => {
		expect(await probeWith(async () => new Response('<html>', { status: 503 }))).toBe(
			'unreachable'
		);
	});

	it('hung request is bounded by the timeout -> unreachable', async () => {
		vi.stubGlobal('location', { origin: 'http://lan.test' });
		const hang = ((_url: string, init?: RequestInit) =>
			new Promise((_resolve, reject) => {
				init?.signal?.addEventListener('abort', () => reject(init.signal?.reason));
			})) as unknown as typeof fetch;
		expect(await probeBackend(hang, 20)).toBe('unreachable');
		expect(PROBE_TIMEOUT_MS).toBeGreaterThan(0);
	});
});
