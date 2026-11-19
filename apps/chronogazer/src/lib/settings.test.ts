import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

class MemoryStorage {
	private map = new Map<string, string>();
	getItem(key: string): string | null {
		return this.map.get(key) ?? null;
	}
	setItem(key: string, value: string): void {
		this.map.set(key, value);
	}
	removeItem(key: string): void {
		this.map.delete(key);
	}
	clear(): void {
		this.map.clear();
	}
}

// Stand-in for the server-side UiSettingsProvider. `get` can be held so a late response can be staged.
const store = new Map<string, string>();
let gate: Promise<void> | null = null;
let failGet = false;
vi.mock('./banto/setup', () => ({
	getUiSettings: () => ({
		get: async (key: string) => {
			if (gate) await gate;
			if (failGet) throw new Error('offline');
			return store.get(key) ?? null;
		},
		set: async (key: string, value: string) => {
			store.set(key, value);
		}
	})
}));
vi.mock('@banto/theme', async (importOriginal) => ({
	...(await importOriginal<object>()),
	applyTheme: vi.fn(),
	applyPreset: vi.fn(),
	watchSystemTheme: vi.fn(() => () => {})
}));

import { settings } from './settings.svelte';

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
	failGet = false;
	vi.stubGlobal('localStorage', new MemoryStorage());
	settings.setThemeMode('system');
	settings.setNavLayout('sidebar');
	store.clear();
});
afterEach(() => {
	vi.unstubAllGlobals();
});

describe('syncFromProvider', () => {
	it('applies the server value', async () => {
		store.set('theme.mode', 'dark');
		await settings.syncFromProvider();
		expect(settings.themeMode).toBe('dark');
		expect(localStorage.getItem('banto.theme')).toBe('dark');
	});

	it('drops the response of a session that is no longer current', async () => {
		store.set('theme.mode', 'dark');
		let current = true;
		const release = hold();
		const p = settings.syncFromProvider(() => current);
		current = false;
		release();
		await p;
		expect(settings.themeMode).toBe('system');
		expect(localStorage.getItem('banto.theme')).toBe('system');
	});

	it('does not roll back a key edited after the read began, but applies the others', async () => {
		store.set('theme.mode', 'dark');
		store.set('shell.navLayout', 'launcher');
		const release = hold();
		const p = settings.syncFromProvider();
		settings.setThemeMode('light');
		release();
		await p;
		expect(settings.themeMode).toBe('light');
		expect(localStorage.getItem('banto.theme')).toBe('light');
		expect(settings.navLayout).toBe('launcher');
	});

	it('keeps the current values when the read fails', async () => {
		store.set('theme.mode', 'dark');
		failGet = true;
		await settings.syncFromProvider();
		expect(settings.themeMode).toBe('system');
		expect(settings.navLayout).toBe('sidebar');
	});
});
