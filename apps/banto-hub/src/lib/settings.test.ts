import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { MemoryStorage } from './banto/testing/hubHttp';

// サーバー側の UiSettingsProvider の代役。get は手で返せる（遅れて返った応答を作るため）。
const store = new Map<string, string>();
let gate: Promise<void> | null = null;
vi.mock('./banto/setup', () => ({
	getUiSettings: () => ({
		get: async (key: string) => {
			if (gate) await gate;
			return { value: store.get(key) ?? null, revision: '0' };
		},
		set: async (key: string, value: string) => {
			store.set(key, value);
			return { revision: '1' };
		}
	})
}));
vi.mock('@banto/theme', async (importOriginal) => ({
	...(await importOriginal<object>()),
	applyTheme: vi.fn(),
	applyPreset: vi.fn(),
	watchSystemTheme: vi.fn(() => () => {})
}));
vi.mock('./recentCommands', () => ({ syncRecentFromProvider: vi.fn(async () => {}) }));

import { settings } from './settings.svelte';
import { syncRecentFromProvider } from './recentCommands';

function hold() {
	let release!: () => void;
	gate = new Promise<void>((resolve) => (release = resolve));
	return () => {
		gate = null;
		release();
	};
}

beforeEach(() => {
	store.clear();
	gate = null;
	vi.stubGlobal('localStorage', new MemoryStorage());
	settings.setThemeMode('system');
	settings.setNavLayout('sidebar');
	store.clear();
	vi.mocked(syncRecentFromProvider).mockClear();
});
afterEach(() => {
	vi.unstubAllGlobals();
});

describe('syncFromProvider', () => {
	it('サーバーの値を当てる', async () => {
		store.set('theme.mode', 'dark');
		await settings.syncFromProvider();
		expect(settings.themeMode).toBe('dark');
	});

	it('古いセッションの応答は設定にも最近のコマンドにも当てない', async () => {
		store.set('theme.mode', 'dark');
		let current = true;
		const release = hold();
		const p = settings.syncFromProvider(() => current);
		current = false;
		release();
		await p;
		expect(settings.themeMode).toBe('system');
		expect(vi.mocked(syncRecentFromProvider).mock.calls[0]?.[0]?.()).toBe(false);
	});

	it('読み込みの開始後に編集したキーは、遅れて返った古い値で巻き戻さない', async () => {
		store.set('theme.mode', 'dark');
		store.set('shell.navLayout', 'launcher');
		const release = hold();
		const p = settings.syncFromProvider();
		settings.setThemeMode('light'); // 開始後の編集
		release();
		await p;
		expect(settings.themeMode).toBe('light');
		// 編集していないキーは当たる。
		expect(settings.navLayout).toBe('launcher');
	});
});
