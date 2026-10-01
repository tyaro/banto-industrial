// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/session.svelte.ts` を写した（v2 移行 PR1d）。
// banto-hub 固有の差:
// - `publicViewer` を削った（このアプリには閲覧公開（viewer-public）が無く、
//   `(app)/+layout.ts` も `publicViewerFallback` を使わない）。
// - `authDisabled` は `false` 固定（Tauri のログイン不要モードは無い。banto-hub
//   は headless の axum サーバーが配信する UI だけ。v1 から同じ）。
// - `commissioningMode` を足した（試運転モード、設計 §5.6・2026-08-30 オーナー
//   決定）。controller が `adopt(..., 'commissioning', ticket)` で確定した合成
//   セッションかどうか。確定させるのは `$lib/banto/commissioningPolicy.ts` の
//   policy runner だけ（v1 の `enterCommissioningMode()` の直接代入は廃止）。
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
	 * 試運転モード（未ロックダウン）の合成セッションが確定しているか
	 * （`kind === 'commissioning'` かつ `active`）。設定画面のロックダウン
	 * セクションの表示条件（`settings/+layout.ts`・`SecuritySection.svelte`）、
	 * `status/+page.svelte`「サーバー状態」の表示、タグストリームの接続先
	 * （`tagMonitorAdmin.ts`）、再接続の失敗後の確認（`sessionRecheck.ts`）が
	 * 読む。
	 *
	 * サーバーは試運転モード中、認証の有無に関わらず全リクエストを合成 admin
	 * として受け付ける（`actor_identity`）が、`/api/auth/identity` はその合成
	 * identity を返さない（`$lib/banto/commissioning.ts` の
	 * `COMMISSIONING_IDENTITY` の doc）。そこで provider には問い合わせず、
	 * policy runner が `adopt()` で確定する（設計 §6.2、S-44）。
	 */
	readonly commissioningMode: boolean = $derived.by(() => {
		const snapshot = getSessionController().snapshot;
		return snapshot.status === 'active' && snapshot.kind === 'commissioning';
	});
}

export const sessionStore = new SessionStore();
