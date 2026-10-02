// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/ownerChange.test.ts` からコピー（v2 移行 PR1d）。
// banto-hub 固有の差: なし（本文は無改変）。
/**
 * Issue #260 実装-3 (design §6.1 wiring ②, §8.1, I-17/I-24): a change of
 * user the controller recorded is reported by the protected layout - on
 * mount too - and handled once. Scenario numbers are design §4.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
	createSessionController,
	resolveSettled,
	type AuthProvider,
	type CredentialRevision,
	type Identity,
	type ResolvedAuth,
	type SessionController
} from '@banto/admin-core';
import { watchOwnerChanges, type OwnerChangePolicy } from './ownerChange';

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

const ALICE: Identity = { id: 'alice', name: 'Alice' };
const BOB: Identity = { id: 'bob', name: 'Bob' };
const CAROL: Identity = { id: 'carol', name: 'Carol' };

/** A standard provider whose answers the test settles; `change()` = another tab's login/logout (reported). */
function provider() {
	let revision = 1;
	const listeners = new Set<() => void>();
	const probes: {
		checked: CredentialRevision;
		answer: ReturnType<typeof deferred<ResolvedAuth>>;
	}[] = [];
	const rev = () => `${revision}.0` as CredentialRevision;
	const auth: AuthProvider = {
		login: async () => ({ success: true }),
		logout: async () => {},
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
	const last = () => probes[probes.length - 1]!;
	return {
		auth,
		change() {
			revision += 1;
			for (const listener of [...listeners]) listener();
		},
		active(identity: Identity) {
			const p = last();
			p.answer.resolve({ status: 'active', checked: p.checked, current: p.checked, identity });
		},
		none() {
			const p = last();
			p.answer.resolve({ status: 'none', checked: p.checked, current: p.checked });
		},
		fail() {
			last().answer.reject(new Error('500'));
		}
	};
}

function setup() {
	const p = provider();
	const controller = createSessionController(p.auth, { onNone: () => {}, onActive: () => {} });
	return { p, controller };
}

async function confirm(controller: SessionController, settle: () => void) {
	const request = resolveSettled(controller);
	settle();
	return request;
}

function handlers(policy: OwnerChangePolicy = 'rebuild') {
	return { policy, notify: vi.fn(), goToLogin: vi.fn(async () => {}) };
}

let stops: (() => void)[] = [];
beforeEach(() => {
	stops = [];
});
afterEach(() => {
	for (const stop of stops) stop();
});

describe('wiring ②: another tab’s login as a different user (#257, I-17, I-24)', () => {
	it('S-35: A -> hold -> B: notified once with { A -> B }, then acknowledged; the policy never touches the credential', async () => {
		const { p, controller } = setup();
		await confirm(controller, () => p.active(ALICE));
		const h = handlers();
		stops.push(watchOwnerChanges(controller, h));

		p.change(); // tab 2 logged in as B (storage event)
		expect(controller.snapshot).toMatchObject({ status: 'unknown', owner: null });
		expect(h.notify).not.toHaveBeenCalled(); // `unknown`: the record waits
		p.active(BOB); // the background confirmation (the hold kicked it)
		await flush();

		expect(h.notify).toHaveBeenCalledTimes(1);
		expect(h.notify).toHaveBeenCalledWith('rebuild', {
			from: 'account:alice',
			to: 'account:bob'
		});
		expect(h.goToLogin).not.toHaveBeenCalled();
		expect(controller.snapshot.pendingOwnerChange).toBeNull();
		expect(controller.snapshot.owner).toBe('account:bob');
	});

	it("S-37: 'relogin' notifies and goes to /login (the shared token is not cleared - the policy has no way to)", async () => {
		const { p, controller } = setup();
		await confirm(controller, () => p.active(ALICE));
		const logout = vi.spyOn(p.auth, 'logout');
		const h = handlers('relogin');
		stops.push(watchOwnerChanges(controller, h));

		p.change();
		p.active(BOB);
		await flush();

		expect(h.notify).toHaveBeenCalledWith('relogin', {
			from: 'account:alice',
			to: 'account:bob'
		});
		expect(h.goToLogin).toHaveBeenCalledTimes(1);
		expect(logout).not.toHaveBeenCalled();
		expect(controller.snapshot).toMatchObject({ status: 'active', owner: 'account:bob' });
	});

	it('S-59: the same user again (re-login in another tab): +2 generations, no notice', async () => {
		const { p, controller } = setup();
		await confirm(controller, () => p.active(ALICE));
		const generation = controller.snapshot.generation;
		const h = handlers();
		stops.push(watchOwnerChanges(controller, h));

		p.change();
		p.active({ ...ALICE });
		await flush();

		expect(controller.snapshot).toMatchObject({
			status: 'active',
			owner: 'account:alice',
			generation: generation + 2
		});
		expect(h.notify).not.toHaveBeenCalled();
	});

	it('S-81: a change confirmed while no layout is mounted (503 page) is reported when it mounts, once', async () => {
		const { p, controller } = setup();
		await confirm(controller, () => p.active(ALICE));

		// A -> B: the confirmation fails (500) -> the load's 503; the layout is unmounted.
		p.change();
		p.fail();
		await flush();
		expect(controller.snapshot.status).toBe('unknown');
		// The background confirmation then confirms B, and the guard re-confirms the same B (retry).
		await confirm(controller, () => p.active(BOB));
		await confirm(controller, () => p.active(BOB));
		expect(controller.snapshot.pendingOwnerChange).toEqual({
			from: 'account:alice',
			to: 'account:bob'
		});

		// The layout mounts again (the retry button, controller kept).
		const h = handlers();
		stops.push(watchOwnerChanges(controller, h));
		expect(h.notify).toHaveBeenCalledTimes(1);
		expect(controller.snapshot.pendingOwnerChange).toBeNull();

		// A later mount (another navigation) does not report it again.
		const again = handlers();
		stops.push(watchOwnerChanges(controller, again));
		expect(again.notify).not.toHaveBeenCalled();
	});

	it("S-83: `none` discards the unhandled change; C's explicit login then raises nothing ('relogin' does not fire)", async () => {
		const { p, controller } = setup();
		await confirm(controller, () => p.active(ALICE));
		p.change();
		p.fail();
		await flush();
		await confirm(controller, () => p.active(BOB)); // { A -> B } unhandled (503 page)
		expect(controller.snapshot.pendingOwnerChange).not.toBeNull();

		p.change(); // another tab logged out
		await confirm(controller, () => p.none());
		expect(controller.snapshot.pendingOwnerChange).toBeNull();

		p.change(); // this tab: C logs in explicitly on /login
		await confirm(controller, () => p.active(CAROL));
		const h = handlers('relogin');
		stops.push(watchOwnerChanges(controller, h));
		expect(h.notify).not.toHaveBeenCalled();
		expect(h.goToLogin).not.toHaveBeenCalled();
	});

	it('the subscription ends with the returned function (the layout unmounts)', async () => {
		const { p, controller } = setup();
		await confirm(controller, () => p.active(ALICE));
		const h = handlers();
		const stop = watchOwnerChanges(controller, h);
		stop();
		p.change();
		p.active(BOB);
		await flush();
		expect(h.notify).not.toHaveBeenCalled();
		expect(controller.snapshot.pendingOwnerChange).not.toBeNull(); // kept for the next mount
	});
});
