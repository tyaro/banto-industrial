// banto v3.0.0 の admin-template
// `apps/admin-template/src/lib/session.svelte.ts` を写した（v3 移行）。
// banto-hub 固有の差:
// - `publicViewer` を削った（このアプリには閲覧公開（viewer-public）が無く、
//   `(app)/+layout.ts` の `grantFallback` の kind は `commissioning` だけ）。
// - `authDisabled` は `false` 固定（Tauri のログイン不要モードは無い。banto-hub
//   は headless の axum サーバーが配信する UI だけ。v1 から同じ）。
// - `commissioningGrant`（旧 `commissioningMode`）を足した（試運転モード、設計 §5.6・2026-08-30 オーナー
//   決定）。サーバーが発行した試運転の grant（kind `commissioning`）で確定した
//   セッションかどうか（v3.0.0 で policy runner の `adopt` は廃止）。
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

	/** banto-hub には Tauri のログイン不要モードが無いため常に false。 */
	readonly authDisabled: boolean = false;

	/**
	 * **このタブのセッションが試運転の grant か**（`kind === 'commissioning'` かつ
	 * `active`。kind はサーバーの `GET /api/auth/identity` の `kind` 由来で、
	 * アカウントのトークンなら `'account'`）。「誰として操作しているか」だけを表す。
	 *
	 * **「サーバーが試運転モード（未ロックダウン）か」ではない**（2026-10-04 オーナー
	 * 指示で軸を分けた）。ロックダウンの欄や試運転の表示の出し分けは
	 * `$lib/banto/commissioningState.svelte` の `serverCommissioning` を読むこと -
	 * アカウントでログインしたままでも、サーバーが試運転中ならロックダウンできる。
	 * いまこの値を読む画面は無い（監査・デバッグ用に残す）。確定させるのは
	 * `(app)/+layout.ts` の `grantFallback`（policy runner の `adopt` は廃止、banto v3.0.0）。
	 */
	readonly commissioningGrant: boolean = $derived.by(() => {
		const snapshot = getSessionController().snapshot;
		return snapshot.status === 'active' && snapshot.kind === 'commissioning';
	});
}

export const sessionStore = new SessionStore();
