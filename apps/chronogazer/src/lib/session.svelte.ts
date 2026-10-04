// banto v3.0.0 の admin-template
// `apps/admin-template/src/lib/session.svelte.ts` からコピー（v3 移行）。
// chronogazer 固有の差: なし（I2b で閲覧公開を使うことになり、v3 移行時に
// 削った `publicViewer` を戻した。2026-10-04 オーナー決定）。
/**
 * Current session's identity/role (Svelte 5 runes), spec M10 RBAC.
 *
 * Issue #260 (v2.0.0, design §6.1, I-12): every field is DERIVED from the
 * default `SessionController`'s snapshot - the one writer of "who is signed
 * in". Nothing here is assigned from a `load` any more (the pre-v2 `load()`
 * wrote identity/role and read `authDisabled` separately after an `await`,
 * which could apply a stale answer - S-61). So every page/component under
 * the `(app)` route group reads the confirmed session reactively, all fields
 * from the SAME snapshot.
 *
 * Ordering note: SvelteKit does NOT guarantee a child route's `load()` waits
 * for an ancestor layout's `load()` to finish unless it calls `await
 * parent()` - so `routes/(app)/users/+page.ts` (and the other loads that
 * need `role`) do exactly that rather than reading `sessionStore.role`
 * optimistically: after `(app)/+layout.ts` resolved, the controller has
 * confirmed the session. Components render only after that load resolved.
 *
 * While the session is not confirmed (`unknown`, e.g. the hold after
 * another tab's login) or confirmed `none`, `identity` is `null` and `role`
 * is the least-privileged `viewer` (fail closed, `parseRole`).
 */
import { getSessionController, type Identity } from '@banto/admin-core';
import { parseRole, type Role } from './permissions';

class SessionStore {
	/** The confirmed identity, or `null` unless the session is `active`. */
	readonly identity: Identity | null = $derived.by(() => {
		const snapshot = getSessionController().snapshot;
		return snapshot.status === 'active' ? snapshot.identity : null;
	});

	readonly role: Role = $derived(parseRole(this.identity));

	/**
	 * Login-not-required mode (spec M11): the Tauri auth-disabled session,
	 * i.e. the provider answered `kind: 'local'` (design §5.3, S-47/S-61) -
	 * not a separate `auth_config_get` read. Always `false` outside the Tauri
	 * webview (that mode is desktop-only), and `false` while nothing is
	 * confirmed, so the UI it gates (hiding the logout button/password-change
	 * section) never disappears on a guess.
	 */
	readonly authDisabled: boolean = $derived(getSessionController().snapshot.kind === 'local');

	/**
	 * Is this the synthetic LAN "viewer-public" session (viewer-public-plan
	 * §2.2/§3.1-6, ADR-0012)? The issuer explicitly marks synthetic sessions
	 * with `identity.kind === 'publicViewer'` (ADR-0017; the controller keys them `publicViewer`);
	 * usernames (including `public`) and roles cannot distinguish them from
	 * ordinary accounts (Issue #209). Consumed by the nav allowlist
	 * (`navigation.ts`), `Header.svelte`'s login button, and
	 * `settings/AccountSection.svelte`'s account-UI guard.
	 */
	readonly publicViewer: boolean = $derived.by(() => {
		const snapshot = getSessionController().snapshot;
		return snapshot.status === 'active' && snapshot.kind === 'publicViewer';
	});
}

export const sessionStore = new SessionStore();
