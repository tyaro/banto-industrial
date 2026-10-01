// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/session.test.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: `publicViewer` のケースを削った（このアプリには閲覧公開が無く、
// `session.svelte.ts` も `publicViewer` を持たない）。`authDisabled` の導出が壊れると
// キオスクの ESCAPE HATCH（`canManageAuthMode()`）が偽になるので、ここで固定する。
/**
 * Issue #260 実装-3 (design §6.1, I-12): `sessionStore` is derived from the
 * SessionController's snapshot - one snapshot, no separate reads.
 */
import { describe, expect, it } from 'vitest';
import {
	getSessionController,
	initBanto,
	resolveSettled,
	type AuthProvider,
	type CredentialRevision,
	type DataProvider,
	type ResolvedAuth
} from '@banto/admin-core';
import { sessionStore } from './session.svelte';

/** A provider that answers `answers` in order, one per `resolve()`; `change()` reports a credential change. */
function provider(answers: ((revision: CredentialRevision) => ResolvedAuth)[]) {
	let revision = 1;
	const listeners = new Set<() => void>();
	const rev = () => `${revision}.0` as CredentialRevision;
	const auth: AuthProvider = {
		login: async () => ({ success: true }),
		logout: async () => {},
		resolve: async () => {
			const next = answers.shift();
			if (!next) throw new Error('no more answers');
			return next(rev());
		},
		credentialRevision: rev,
		onCredentialChanged(listener) {
			listeners.add(listener);
			return () => listeners.delete(listener);
		}
	};
	return {
		auth,
		change() {
			revision += 1;
			for (const listener of [...listeners]) listener();
		}
	};
}

const account =
	(role: string) =>
	(r: CredentialRevision): ResolvedAuth => ({
		status: 'active',
		checked: r,
		current: r,
		identity: { id: 'alice', name: 'Alice', role },
		kind: 'account'
	});
const local = (r: CredentialRevision): ResolvedAuth => ({
	status: 'active',
	checked: r,
	current: r,
	identity: { id: '0', name: 'local', role: 'editor' },
	kind: 'local'
});

describe('sessionStore (derived from the controller snapshot)', () => {
	it('S-61: authDisabled follows the confirmed `kind` (`local`), with no separate settings read', async () => {
		const p = provider([account('admin'), local]);
		initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
		const controller = getSessionController();

		await resolveSettled(controller);
		expect(sessionStore.authDisabled).toBe(false);
		expect(sessionStore.role).toBe('admin');
		expect(sessionStore.identity?.id).toBe('alice');

		// Login-not-required mode switched on (the Rust slot re-bound): hold, then `local`.
		p.change();
		expect(sessionStore.identity).toBeNull(); // unknown: nothing confirmed
		expect(sessionStore.role).toBe('viewer'); // fail closed
		expect(sessionStore.authDisabled).toBe(false);
		await resolveSettled(controller);
		expect(sessionStore.authDisabled).toBe(true);
		expect(sessionStore.role).toBe('editor');
	});

	it('`none` leaves no identity and the least-privileged role', async () => {
		const p = provider([(r) => ({ status: 'none', checked: r, current: r })]);
		initBanto({ dataProvider: {} as DataProvider, authProvider: p.auth, resources: [] });
		await resolveSettled(getSessionController());
		expect(sessionStore.identity).toBeNull();
		expect(sessionStore.role).toBe('viewer');
	});
});
