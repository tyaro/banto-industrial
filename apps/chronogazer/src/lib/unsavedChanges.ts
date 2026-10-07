// banto v5.1.0 の admin-template `apps/admin-template/src/lib/unsavedChanges.ts` を写した
// （ChronoGazer #508、経路 B）。ChronoGazer 固有の差:
// - i18n（Paraglide）を持たないので、確認の文言は日本語の定数（`UNSAVED_CONFIRM_LEAVE`）。
// - ログイン画面の URL は `resolve('login')` ではなく `resolveAppPath('/login')`。
// - デスクトップのウィンドウを閉じるときの確認（上流の `windowCloseGuard.ts`）は写して
//   いない（#508 で「別に決める」とした。Tauri の close-requested を JS が握ると
//   ネイティブの閉じる動作が JS 任せになる影響が大きいため）。
/**
 * App-side wiring for `@banto/forms`'s unsaved-changes guard (spec §7,
 * banto issue #214). Pages call `guardUnsavedChanges({ isDirty, isSaving })`
 * during component init; this binds the parts the package cannot know:
 * SvelteKit's `beforeNavigate`, the prompt text and which navigations are
 * forced.
 *
 * Forced = the target is the login screen. Every path there means the
 * session is ending or already gone - logout (`logoutAndLeave` /
 * `leaveForLogin` in `#lib/banto/logout.svelte.ts` confirm `none` first, then
 * `goto('/login')`), `ownerChangePolicy: 'relogin'`
 * (`#lib/banto/ownerChange.ts`), or the `(app)` guard redirecting a session
 * it confirmed `none` after a `refreshAll()`. Holding the user on a page whose
 * session is gone would only strand them, so those never prompt. (A
 * redirect that happens INSIDE a navigation never reaches `beforeNavigate`
 * at all - SvelteKit skips it while navigating.)
 */
import { beforeNavigate } from '$app/navigation';
import {
	guardUnsavedChanges as guardWith,
	type LeaveNavigation,
	type UnsavedChangesGuard
} from '@banto/forms';
import { resolveAppPath } from '#lib/navigation.js';

/** 画面移動の確認文（`window.confirm`）。 */
export const UNSAVED_CONFIRM_LEAVE =
	'保存していない変更があります。変更を破棄してこの画面から移動しますか？';
/** 入力欄の脇に出す「未保存」の表示（`UnsavedChangesNotice`）。 */
export const UNSAVED_NOTICE = '未保存の変更があります';
/** 「変更を取り消す」ボタンの文言。 */
export const UNSAVED_DISCARD = '変更を取り消す';

/** True for navigations that must never be held back (see module doc comment). */
export function isForcedNavigation(navigation: LeaveNavigation): boolean {
	const pathname = navigation.to?.url.pathname;
	const login = resolveAppPath('/login');
	return pathname === login || pathname === `${login}/`;
}

export interface AppUnsavedChangesOptions {
	/** The draft differs from what was loaded or last saved. */
	isDirty: () => boolean;
	/** A save is in flight (leaving could drop it). */
	isSaving?: () => boolean;
}

/** Register the page's guard. Call during component initialisation. */
export function guardUnsavedChanges(options: AppUnsavedChangesOptions): UnsavedChangesGuard {
	return guardWith({
		...options,
		beforeNavigate,
		message: () => UNSAVED_CONFIRM_LEAVE,
		isForced: isForcedNavigation
	});
}
