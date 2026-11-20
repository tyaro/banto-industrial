import { describe, expect, it, vi } from 'vitest';
import type { UiSettingsProvider } from '@banto/admin-core';
import { MemoryStorage } from './banto/testing/hubHttp';

// 既定の provider（`getUiSettings()`）は使わない（各テストが代役を渡す）。
vi.mock('./banto/setup', () => ({ getUiSettings: () => undefined }));

import { createDockLayoutStore } from './dockLayoutStore';
import { serializeDockLayout, type StoredDockLayout } from './dockLayout';

function layout(share: number): StoredDockLayout {
	return {
		schema: 1,
		panels: ['left', 'center'],
		layout: { version: 3, floating: [], docked: { share } }
	};
}

/** サーバーの代役。`get` は手で返せる（読み込みの前後の操作を作るため）。 */
function fakeProvider(initial?: string) {
	const values = new Map<string, string>();
	if (initial !== undefined) values.set('hub.dock.monitor', initial);
	let gate: Promise<void> | null = null;
	let failSet = false;
	const set = vi.fn(async (key: string, value: string) => {
		if (failSet) throw new Error('offline');
		values.set(key, value);
	});
	const provider = {
		get: vi.fn(async (key: string) => {
			if (gate) await gate;
			return values.get(key) ?? null;
		}),
		set
	} as unknown as UiSettingsProvider;
	return {
		provider,
		values,
		set,
		hold() {
			let release!: () => void;
			gate = new Promise<void>((resolve) => (release = resolve));
			return () => {
				gate = null;
				release();
			};
		},
		failWrites(fail: boolean) {
			failSet = fail;
		}
	};
}

async function settle(): Promise<void> {
	for (let i = 0; i < 5; i++) await Promise.resolve();
}

describe('createDockLayoutStore', () => {
	it('最初の描画には localStorage の写しを使う（壊れていれば使わない）', () => {
		const storage = new MemoryStorage();
		storage.setItem('banto.dock.monitor.account%3A1', serializeDockLayout(layout(0.3)));
		const server = fakeProvider();
		expect(
			createDockLayoutStore('monitor', {
				provider: server.provider,
				storage: () => storage,
				owner: () => 'account:1'
			}).initial
		).toEqual(layout(0.3));
		storage.setItem('banto.dock.monitor.account%3A1', '{broken');
		expect(
			createDockLayoutStore('monitor', {
				provider: server.provider,
				storage: () => storage,
				owner: () => 'account:1'
			}).initial
		).toBeUndefined();
	});

	it('マウント時の配置（最初の save）は書かずに基準にする', async () => {
		const storage = new MemoryStorage();
		const server = fakeProvider();
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		store.save(layout(0.2));
		expect(await store.loadSaved()).toBeUndefined();
		await settle();
		expect(storage.getItem('banto.dock.monitor.account%3A1')).toBeNull();
		expect(server.set).not.toHaveBeenCalled();
	});

	it('サーバーの保存値を返し（当てるのは呼び出し側）、当てた後の save は送り直さない', async () => {
		const storage = new MemoryStorage();
		const server = fakeProvider(serializeDockLayout(layout(0.4)));
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		store.save(layout(0.2));
		expect(await store.loadSaved()).toEqual(layout(0.4));
		// 呼び出し側が restoreLayout した結果の save。写しは更新し、サーバーへは送らない。
		store.save(layout(0.4));
		await settle();
		expect(storage.getItem('banto.dock.monitor.account%3A1')).toBe(
			serializeDockLayout(layout(0.4))
		);
		expect(server.set).not.toHaveBeenCalled();
	});

	it('サーバーの値が今の配置と同じなら返さない', async () => {
		const server = fakeProvider(serializeDockLayout(layout(0.2)));
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		store.save(layout(0.2));
		expect(await store.loadSaved()).toBeUndefined();
	});

	it('読み終わる前に利用者が変えたら、サーバーの値は返さずに最新の変更を送る', async () => {
		const storage = new MemoryStorage();
		const server = fakeProvider(serializeDockLayout(layout(0.4)));
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		const release = server.hold();
		store.save(layout(0.2));
		const loading = store.loadSaved();
		store.save(layout(0.25));
		store.save(layout(0.3));
		// 写しは同期で書く。サーバーへは読み終わるまで送らない。
		expect(storage.getItem('banto.dock.monitor.account%3A1')).toBe(
			serializeDockLayout(layout(0.3))
		);
		expect(server.set).not.toHaveBeenCalled();
		release();
		expect(await loading).toBeUndefined();
		await settle();
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(server.values.get('hub.dock.monitor')).toBe(serializeDockLayout(layout(0.3)));
	});

	it('読み終えた後の変更は送る。同じ配置は送らない', async () => {
		const server = fakeProvider();
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		store.save(layout(0.2));
		await store.loadSaved();
		store.save(layout(0.3));
		await settle();
		store.save(layout(0.3));
		await settle();
		expect(server.set).toHaveBeenCalledTimes(1);
		expect(server.values.get('hub.dock.monitor')).toBe(serializeDockLayout(layout(0.3)));
	});

	it('送信に失敗した値は保存済みにせず、次の変更で最新を送り直す', async () => {
		const server = fakeProvider();
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => new MemoryStorage(),
			owner: () => 'account:1'
		});
		store.save(layout(0.2));
		await store.loadSaved();
		server.failWrites(true);
		store.save(layout(0.3));
		await settle();
		expect(server.values.has('hub.dock.monitor')).toBe(false);
		server.failWrites(false);
		store.save(layout(0.35));
		await settle();
		expect(server.values.get('hub.dock.monitor')).toBe(serializeDockLayout(layout(0.35)));
	});

	it('localStorage が書けなくても続ける', async () => {
		const broken = {
			getItem: () => null,
			setItem: () => {
				throw new Error('quota');
			}
		} as Pick<Storage, 'getItem' | 'setItem'>;
		const server = fakeProvider();
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => broken,
			owner: () => 'account:1'
		});
		store.save(layout(0.2));
		await store.loadSaved();
		expect(() => store.save(layout(0.3))).not.toThrow();
		await settle();
		expect(server.values.get('hub.dock.monitor')).toBe(serializeDockLayout(layout(0.3)));
	});

	it('dispose の後は読み込みの結果を返さず、保存もしない', async () => {
		const storage = new MemoryStorage();
		const server = fakeProvider(serializeDockLayout(layout(0.4)));
		const store = createDockLayoutStore('monitor', {
			provider: server.provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		const release = server.hold();
		store.save(layout(0.2));
		const loading = store.loadSaved();
		store.dispose();
		release();
		expect(await loading).toBeUndefined();
		store.save(layout(0.3));
		await settle();
		expect(storage.getItem('banto.dock.monitor.account%3A1')).toBeNull();
		expect(server.set).not.toHaveBeenCalled();
	});
	it('写しはログインの利用者ごと: A の配置を B の最初の描画に使わない', () => {
		const storage = new MemoryStorage();
		const server = fakeProvider();
		const make = (owner: string | null) =>
			createDockLayoutStore('monitor', {
				provider: server.provider,
				storage: () => storage,
				owner: () => owner
			});
		const a = make('account:1');
		a.save(layout(0.5));
		a.save(layout(0.2));
		expect(storage.getItem('banto.dock.monitor.account%3A1')).toBe(
			serializeDockLayout(layout(0.2))
		);
		expect(make('account:2').initial).toBeUndefined();
		expect(make('account:1').initial).toEqual(layout(0.2));
	});

	it('前のページ名だけのキーは読まない', () => {
		const storage = new MemoryStorage();
		storage.setItem('banto.dock.monitor', serializeDockLayout(layout(0.3)));
		const store = createDockLayoutStore('monitor', {
			provider: fakeProvider().provider,
			storage: () => storage,
			owner: () => 'account:1'
		});
		expect(store.initial).toBeUndefined();
	});

	it('利用者が分からないときは写しを読まず、書きもしない', () => {
		const storage = new MemoryStorage();
		storage.setItem('banto.dock.monitor', serializeDockLayout(layout(0.3)));
		const setItem = vi.spyOn(storage, 'setItem');
		const getItem = vi.spyOn(storage, 'getItem');
		const store = createDockLayoutStore('monitor', {
			provider: fakeProvider().provider,
			storage: () => storage,
			owner: () => null
		});
		expect(store.initial).toBeUndefined();
		store.save(layout(0.5));
		store.save(layout(0.2));
		expect(getItem).not.toHaveBeenCalled();
		expect(setItem).not.toHaveBeenCalled();
	});

	it('作った後に持ち主が替わったら写しを書かない', () => {
		const storage = new MemoryStorage();
		let owner: string | null = 'account:1';
		const store = createDockLayoutStore('monitor', {
			provider: fakeProvider().provider,
			storage: () => storage,
			owner: () => owner
		});
		store.save(layout(0.5));
		owner = 'account:2';
		store.save(layout(0.2));
		expect(storage.getItem('banto.dock.monitor.account%3A1')).toBeNull();
		expect(storage.getItem('banto.dock.monitor.account%3A2')).toBeNull();
	});
});
