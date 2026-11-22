/**
 * `setup.ts` の `bantoReady` のデモの分岐（GitHub Pages のデモ、docs/demo-pages-design.md
 * §3.1・§3.2）の確認:
 *
 * - `VITE_BANTO_DEMO=1` のビルドは、模擬（`#lib/demo/`）を入れ終えてから通常の provider を
 *   作る。トークンはデモ専用のキーで、loopback の掃除（`purgeLoopbackRememberedToken`）は呼ばない。
 * - 模擬の初期化が失敗したら、通常の provider を作らずに reject する。
 * - フラグの無いビルドは今までどおり（掃除の後に既定のキーで作る。模擬は読み込まない）。
 *
 * provider の配線そのものではなく、呼ぶ順と引数を見るため、`initBanto`・`connectEvents`・
 * `createHttpAuthProvider` と掃除・模擬を差し替える。
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const core = vi.hoisted(() => ({
	initBanto: vi.fn(),
	connectEvents: vi.fn(),
	createHttpAuthProvider: vi.fn()
}));
const purge = vi.hoisted(() => ({ purgeLoopbackRememberedToken: vi.fn() }));
const demo = vi.hoisted(() => ({ startDemo: vi.fn() }));

vi.mock('@banto/admin-core', async (importOriginal) => ({
	...(await importOriginal<typeof import('@banto/admin-core')>()),
	initBanto: core.initBanto,
	connectEvents: core.connectEvents,
	createHttpAuthProvider: core.createHttpAuthProvider
}));
vi.mock('./loopbackTokenPurge', () => purge);
vi.mock('#lib/demo/index.js', () => demo);

const DEMO_KEY = 'banto-industrial.demo.hub.token';

async function loadSetup() {
	const setup = await import('./setup');
	// 失敗の経路の reject は各試験が見る。
	void setup.bantoReady.catch(() => {});
	return setup;
}

beforeEach(() => {
	vi.resetModules();
	core.initBanto.mockReset();
	core.connectEvents.mockReset();
	core.createHttpAuthProvider.mockReset();
	core.createHttpAuthProvider.mockImplementation(() => ({ getToken: () => null }));
	purge.purgeLoopbackRememberedToken.mockReset();
	purge.purgeLoopbackRememberedToken.mockResolvedValue(undefined);
	demo.startDemo.mockReset();
});

afterEach(() => {
	vi.unstubAllEnvs();
});

describe('bantoReady: デモのビルド', () => {
	it('模擬を入れ終えてから、デモ専用のキーで通常の provider を作る（掃除はしない）', async () => {
		vi.stubEnv('VITE_BANTO_DEMO', '1');
		const order: string[] = [];
		demo.startDemo.mockImplementation(async () => {
			order.push('startDemo');
			return { tokenStorageKey: DEMO_KEY };
		});
		core.createHttpAuthProvider.mockImplementation(() => {
			order.push('createHttpAuthProvider');
			return { getToken: () => null };
		});
		core.initBanto.mockImplementation(() => order.push('initBanto'));

		const setup = await loadSetup();
		await setup.bantoReady;

		expect(order).toEqual(['startDemo', 'createHttpAuthProvider', 'initBanto']);
		expect(core.createHttpAuthProvider).toHaveBeenCalledWith({ storageKey: DEMO_KEY });
		expect(core.connectEvents).toHaveBeenCalledTimes(1);
		expect(purge.purgeLoopbackRememberedToken).not.toHaveBeenCalled();
	});

	it('模擬の初期化が失敗したら、通常の provider を作らずに reject', async () => {
		vi.stubEnv('VITE_BANTO_DEMO', '1');
		demo.startDemo.mockRejectedValue(new Error('demo init failed'));

		const setup = await loadSetup();
		await expect(setup.bantoReady).rejects.toThrow('demo init failed');

		expect(core.createHttpAuthProvider).not.toHaveBeenCalled();
		expect(core.initBanto).not.toHaveBeenCalled();
		expect(core.connectEvents).not.toHaveBeenCalled();
		expect(purge.purgeLoopbackRememberedToken).not.toHaveBeenCalled();
	});
});

describe('bantoReady: フラグの無いビルド', () => {
	it('掃除の後に既定のキーで作り、模擬は読み込まない', async () => {
		const setup = await loadSetup();
		await setup.bantoReady;

		expect(purge.purgeLoopbackRememberedToken).toHaveBeenCalledTimes(1);
		expect(core.createHttpAuthProvider).toHaveBeenCalledWith({});
		expect(demo.startDemo).not.toHaveBeenCalled();
		expect(core.initBanto).toHaveBeenCalledTimes(1);
	});
});
