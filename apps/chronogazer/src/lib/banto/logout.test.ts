// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/logout.test.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Issue #260 実装-3 (design §6.1, I-10/I-18): the logout
 * (`logoutAndLeave`) is `logout()` -> `resolveSettled()` -> `/login` only for
 * a confirmed `none`. Another session can be confirmed while the logout
 * request is in flight (another tab's login); the logout must then leave it
 * alone (S-51/S-17). And the login screen must not appear before the logout
 * and its confirmation finished (CI of #265: a login submitted there lost
 * the compare-and-set to the pending logout).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	adaptLegacyAuthProvider,
	currentSessionScope,
	getSessionController,
	initBanto,
	loadListViewState,
	resolveSettled,
	saveListViewState,
	type AuthProvider,
	type CredentialRevision,
	type DataProvider,
	type Identity,
	type ResolvedAuth
} from '@banto/admin-core';
import { isLeavingForLogin, leaveForLogin, logoutAndLeave } from './logout.svelte';

const goToLogin = vi.fn(async () => {});
const notify = vi.fn();
const logout = () => logoutAndLeave(goToLogin, { notify });

function deferred<T>() {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((res, rej) => {
		resolve = res;
		reject = rej;
	});
	return { promise, resolve, reject };
}

function flush(): Promise<void> {
	return new Promise((resolve) => setTimeout(resolve, 0));
}

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

const ALICE: Identity = { id: 'alice', name: 'Alice' };
const BOB: Identity = { id: 'bob', name: 'Bob' };

/**
 * A standard provider: every `resolve()` answer and the logout response are
 * settled by the test. `logout` clears the credential (revision + 1,
 * reported) when its response arrives, unless `keepOnLogout` (the
 * provider's compare-and-set found another login's token, S-21).
 */
function standardProvider() {
	let revision = 1;
	const listeners = new Set<() => void>();
	const probes: {
		checked: CredentialRevision;
		answer: ReturnType<typeof deferred<ResolvedAuth>>;
	}[] = [];
	const logoutGate = deferred<void>();
	const state = { keepOnLogout: false, rejectLogout: false, refuseLogout: false };
	const rev = () => `${revision}.0` as CredentialRevision;
	const change = () => {
		revision += 1;
		for (const listener of [...listeners]) listener();
	};
	const provider: AuthProvider = {
		login: async () => ({ success: true }),
		logout: vi.fn(async () => {
			await logoutGate.promise;
			if (state.rejectLogout) {
				// A Tauri invoke with no answer: the slot may have changed (local + 1, reported).
				change();
				throw new Error('no answer');
			}
			if (state.refuseLogout) throw { kind: 'forbidden', message: 'refused' };
			if (!state.keepOnLogout) change();
		}),
		resolve: () => {
			const answer = deferred<ResolvedAuth>();
			probes.push({ checked: rev(), answer });
			return answer.promise;
		},
		credentialRevision: rev,
		onCredentialChanged(listener) {
			listeners.add(listener);
			return () => listeners.delete(listener);
		}
	};
	const last = () => {
		const probe = probes[probes.length - 1];
		if (!probe) throw new Error('no probe');
		return probe;
	};
	return {
		provider,
		logoutGate,
		state,
		probes,
		active(identity: Identity) {
			const probe = last();
			probe.answer.resolve({
				status: 'active',
				checked: probe.checked,
				current: probe.checked,
				identity
			});
		},
		none() {
			const probe = last();
			probe.answer.resolve({ status: 'none', checked: probe.checked, current: probe.checked });
		},
		fail() {
			last().answer.reject(new Error('500'));
		},
		/** Another login stored its token (reported, I-19). */
		login: change
	};
}

async function signedInAsAlice() {
	const p = standardProvider();
	initBanto({ dataProvider: {} as DataProvider, authProvider: p.provider, resources: [] });
	const first = resolveSettled(getSessionController());
	p.active(ALICE);
	await first;
	return p;
}

beforeEach(() => {
	vi.stubGlobal('sessionStorage', memoryStorage());
	goToLogin.mockReset();
	notify.mockReset();
	goToLogin.mockImplementation(async () => {});
});
afterEach(() => {
	vi.unstubAllGlobals();
});

describe('logoutAndLeave (I-10, I-18)', () => {
	it('logout -> the credential is cleared -> resolveSettled confirms none -> /login, once, after all of it', async () => {
		const p = await signedInAsAlice();
		const seen: boolean[] = [];
		goToLogin.mockImplementationOnce(async () => {
			seen.push(isLeavingForLogin());
		});

		const done = logout();
		expect(isLeavingForLogin()).toBe(true);
		await flush();
		expect(goToLogin).not.toHaveBeenCalled(); // the logout request is still in flight
		p.logoutGate.resolve();
		await flush();
		expect(goToLogin).not.toHaveBeenCalled(); // the confirmation is still in flight
		expect(getSessionController().snapshot.status).toBe('unknown'); // the hold (I-5)
		p.none();
		await expect(done).resolves.toBe('left');
		expect(getSessionController().snapshot.status).toBe('none');
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(notify).not.toHaveBeenCalled();
		expect(seen).toEqual([true]);
		expect(isLeavingForLogin()).toBe(false);
	});

	it('S-51: a login confirmed while the logout is in flight is not ended by it (generation and saved state kept, no /login)', async () => {
		const p = await signedInAsAlice();
		const controller = getSessionController();

		const done = logout();
		// On another tab meanwhile: B logs in and this tab confirms B.
		p.login();
		p.active(BOB);
		await flush();
		expect(controller.snapshot.owner).toBe('account:bob');
		const scope = currentSessionScope();
		saveListViewState(scope, 'items:server', { sort: [], filters: [] });
		const generation = controller.snapshot.generation;

		// The late logout response: the provider's compare-and-set kept B's token.
		p.state.keepOnLogout = true;
		p.logoutGate.resolve();
		await flush();
		p.active(BOB); // the logout's own confirmation (a new probe, I-9)
		await expect(done).resolves.toBe('stayed');
		expect(notify).toHaveBeenCalledWith('stayed');

		expect(controller.snapshot).toMatchObject({
			status: 'active',
			owner: 'account:bob',
			generation
		});
		expect(loadListViewState(scope, 'items:server')).not.toBeNull();
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isLeavingForLogin()).toBe(false);
	});

	it('the confirmation is a signal: a probe started before the logout cannot answer it (I-9)', async () => {
		const p = await signedInAsAlice();
		const controller = getSessionController();
		// A load's confirmation is in flight when the logout starts.
		const load = resolveSettled(controller);
		const before = p.probes.length;
		p.state.keepOnLogout = true; // no reported change: only the signal forces a new probe
		const done = logout();
		p.logoutGate.resolve();
		await flush();
		expect(p.probes.length).toBe(before + 1);
		p.none();
		await expect(done).resolves.toBe('left');
		await load;
		expect(goToLogin).toHaveBeenCalledTimes(1);
	});

	it('the confirmation fails (unverified): stays, no /login, the user is told; A is not active again', async () => {
		const p = await signedInAsAlice();
		const done = logout();
		p.logoutGate.resolve();
		await flush();
		p.fail();
		await expect(done).resolves.toBe('unverified');
		expect(notify).toHaveBeenCalledTimes(1);
		expect(notify).toHaveBeenCalledWith('unverified');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(getSessionController().snapshot).toMatchObject({ status: 'unknown', owner: null });
		expect(isLeavingForLogin()).toBe(false);
	});

	it('a rejected logout that still ended the session goes to /login, nothing thrown or told', async () => {
		const p = await signedInAsAlice();
		p.state.rejectLogout = true;
		const done = logout();
		p.logoutGate.resolve();
		await flush();
		p.none();
		await expect(done).resolves.toBe('left');
		expect(goToLogin).toHaveBeenCalledTimes(1);
		expect(notify).not.toHaveBeenCalled();
	});

	it('a rejected logout (no answer) after which the session is still active: stayed, told, not thrown', async () => {
		const p = await signedInAsAlice();
		p.state.rejectLogout = true;
		const done = logout();
		p.logoutGate.resolve();
		await flush();
		p.active(ALICE);
		await expect(done).resolves.toBe('stayed');
		expect(notify).toHaveBeenCalledWith('stayed');
		expect(goToLogin).not.toHaveBeenCalled();
		expect(isLeavingForLogin()).toBe(false);
	});

	it('a logout refused with a structured error (nothing changed): stayed, told, not thrown (audit P2-2)', async () => {
		const p = await signedInAsAlice();
		p.state.refuseLogout = true;
		const done = logout();
		p.logoutGate.resolve();
		await flush();
		p.active(ALICE); // the signal's probe: still Alice
		await expect(done).resolves.toBe('stayed');
		expect(notify).toHaveBeenCalledTimes(1);
		expect(notify).toHaveBeenCalledWith('stayed');
		expect(getSessionController().snapshot.owner).toBe('account:alice');
		expect(goToLogin).not.toHaveBeenCalled();
	});

	it('S-17: a provider that cannot report the logout (compatibility adapter) still reaches /login', async () => {
		let valid = true;
		const legacy = adaptLegacyAuthProvider({
			login: async () => ({ success: true }),
			logout: async () => {
				valid = false;
			},
			check: async () => valid,
			getIdentity: async () => ALICE
		});
		initBanto({ dataProvider: {} as DataProvider, authProvider: legacy, resources: [] });
		const controller = getSessionController();
		await resolveSettled(controller);
		expect(controller.snapshot.status).toBe('active');

		await expect(logout()).resolves.toBe('left');
		expect(controller.snapshot.status).toBe('none');
		expect(goToLogin).toHaveBeenCalledTimes(1);
	});
});

describe('leaveForLogin', () => {
	it('holds isLeavingForLogin() while the navigation runs, also when it fails', async () => {
		const seen: boolean[] = [];
		await leaveForLogin(async () => {
			seen.push(isLeavingForLogin());
		});
		await expect(
			leaveForLogin(async () => {
				throw new Error('cancelled');
			})
		).rejects.toThrow('cancelled');
		expect(seen).toEqual([true]);
		expect(isLeavingForLogin()).toBe(false);
	});
});
