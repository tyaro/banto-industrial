// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/providers/demo.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: なし（本文は無改変）。
/**
 * Demo AuthProvider (spec §3.3): fixed admin/admin credentials backed by
 * sessionStorage. Used in plain-browser dev only (spec §11.1 environment 3);
 * Tauri and embedded-server modes use the real `auth_*` Rust commands/REST
 * routes instead. Split out of setup.ts (improvement-plan-2026-07.md P3-4):
 * demo-mode plumbing app authors never touch when swapping resources.
 *
 * Issue #260 実装-3 (design §5.2, §7.1): a STANDARD provider (the v2
 * contract), not the compatibility adapter:
 * - `credentialRevision()` is an in-memory counter (opaque `${n}.0`) that
 *   advances when this provider changes the stored flag - a login or a
 *   logout that actually changed it - and `onCredentialChanged` listeners
 *   hear exactly those changes, in the operation's own continuation (I-19);
 * - `login`/`logout` write the flag only while the revision is still the one
 *   read when they started (compare-and-set, I-7; with no request in
 *   between here, the check is trivially met, but the shape matches the
 *   HTTP provider's);
 * - `resolve()` answers from the flag in one step; it never clears anything,
 *   so `checked === current`.
 * The flag lives in `sessionStorage`, which is per tab: there is no other
 * tab to hear from (no `storage` events for it).
 */
import type { AuthProvider, CredentialRevision, ResolvedAuth } from '@banto/admin-core';

const AUTH_KEY = 'banto.auth.demo';

/**
 * Spec M10 RBAC: the demo provider's one fixed account is always full
 * 'admin' - this is the only environment where usersAdmin.ts is
 * unconditionally unavailable anyway (see isUsersAdminAvailable()), so this
 * only matters for permissions.ts-gated UI elsewhere (nav, items page,
 * settings page), which should behave exactly as if a real admin were
 * logged in.
 */
const DEMO_IDENTITY = { id: 'admin', name: '管理者', role: 'admin' } as const;

function isSessionAuthed(): boolean {
	return typeof sessionStorage !== 'undefined' && sessionStorage.getItem(AUTH_KEY) === '1';
}

/** A fresh demo provider (its own revision counter and listeners). */
export function createDemoAuthProvider(): AuthProvider {
	let counter = 0;
	const listeners = new Set<() => void>();
	const revision = (): CredentialRevision => `${counter}.0` as CredentialRevision;

	/** Write the flag if the revision is still `expected`; advance and notify only when it changed. */
	function writeIfUnchanged(expected: CredentialRevision, authed: boolean): boolean {
		if (revision() !== expected) return false;
		if (isSessionAuthed() === authed) return true;
		if (authed) sessionStorage.setItem(AUTH_KEY, '1');
		else sessionStorage.removeItem(AUTH_KEY);
		counter += 1;
		for (const listener of [...listeners]) listener();
		return true;
	}

	return {
		async login(params) {
			const start = revision();
			const { username, password } = params as { username?: string; password?: string };
			if (username !== 'admin' || password !== 'admin') {
				return { success: false, error: 'ユーザー名またはパスワードが違います' };
			}
			if (!writeIfUnchanged(start, true)) {
				return {
					success: false,
					superseded: true,
					error: '別のセッションが先に確定したため、このログインは適用されませんでした'
				};
			}
			return { success: true };
		},
		async logout() {
			writeIfUnchanged(revision(), false);
		},
		async resolve(): Promise<ResolvedAuth> {
			const checked = revision();
			return isSessionAuthed()
				? { status: 'active', checked, current: checked, identity: { ...DEMO_IDENTITY } }
				: { status: 'none', checked, current: checked };
		},
		credentialRevision: revision,
		onCredentialChanged(listener) {
			listeners.add(listener);
			return () => {
				listeners.delete(listener);
			};
		},
		// Always "initialized": the demo provider's admin/admin account always
		// exists, so the login page never shows the first-run setup form here
		// (spec §8.2's setup flow only applies to Tauri/embedded-server modes).
		async status() {
			return { initialized: true };
		},
		// No account store to change a password on in pure-browser demo mode;
		// the settings page hides the password-change section when this
		// resolves to `success: false` (see its "note" fallback).
		async changePassword() {
			return { success: false, error: 'デモモードでは変更できません' };
		}
	};
}

/** The app's demo provider (`setup.ts`). */
export const demoAuthProvider: AuthProvider = createDemoAuthProvider();
