// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/providers/demo.test.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Issue #260 実装-3 (design §5.2, §8.2): the demo provider is a standard
 * provider - `login`/`logout` advance the revision and notify (only when
 * the stored flag actually changed), and `resolve()` answers from the flag
 * in one step (`checked === current`).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createDemoAuthProvider } from './demo';

function memoryStorage(): Storage {
	const map = new Map<string, string>();
	return {
		getItem: (key) => map.get(key) ?? null,
		setItem: (key, value) => void map.set(key, value),
		removeItem: (key) => void map.delete(key),
		clear: () => map.clear(),
		key: (index) => Array.from(map.keys())[index] ?? null,
		get length() {
			return map.size;
		}
	} as Storage;
}

beforeEach(() => {
	vi.stubGlobal('sessionStorage', memoryStorage());
});
afterEach(() => {
	vi.unstubAllGlobals();
});

describe('demo AuthProvider (standard contract)', () => {
	it('login advances the revision and notifies once; resolve answers the admin in one step', async () => {
		const auth = createDemoAuthProvider();
		const changed = vi.fn();
		auth.onCredentialChanged(changed);
		const before = auth.credentialRevision();
		await expect(auth.resolve()).resolves.toMatchObject({
			status: 'none',
			checked: before,
			current: before
		});

		await expect(auth.login({ username: 'admin', password: 'admin' })).resolves.toEqual({
			success: true
		});
		const after = auth.credentialRevision();
		expect(after).not.toBe(before);
		expect(changed).toHaveBeenCalledTimes(1);
		await expect(auth.resolve()).resolves.toEqual({
			status: 'active',
			checked: after,
			current: after,
			identity: { id: 'admin', name: '管理者', role: 'admin' }
		});
	});

	it('wrong credentials change nothing', async () => {
		const auth = createDemoAuthProvider();
		const changed = vi.fn();
		auth.onCredentialChanged(changed);
		const before = auth.credentialRevision();
		await expect(auth.login({ username: 'admin', password: 'nope' })).resolves.toMatchObject({
			success: false
		});
		expect(auth.credentialRevision()).toBe(before);
		expect(changed).not.toHaveBeenCalled();
	});

	it('logout advances and notifies only when it cleared something; an unsubscribed listener is not called', async () => {
		const auth = createDemoAuthProvider();
		await auth.login({ username: 'admin', password: 'admin' });
		const changed = vi.fn();
		const off = auth.onCredentialChanged(changed);

		await auth.logout();
		expect(changed).toHaveBeenCalledTimes(1);
		await expect(auth.resolve()).resolves.toMatchObject({ status: 'none' });

		const revision = auth.credentialRevision();
		await auth.logout(); // nothing stored: no change
		expect(auth.credentialRevision()).toBe(revision);
		expect(changed).toHaveBeenCalledTimes(1);

		off();
		await auth.login({ username: 'admin', password: 'admin' });
		expect(changed).toHaveBeenCalledTimes(1);
	});
});
