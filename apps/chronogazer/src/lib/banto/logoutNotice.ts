// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/lib/banto/logoutNotice.ts` からコピー（v2 移行 PR1c）。
// chronogazer 固有の差: 文言は Paraglide ではなく日本語の直書き（admin-template の
// ja の `auth.logoutStayed`・`auth.logoutUnverified` と同じ文）。
/**
 * The toast for a logout that did not reach /login (Issue #260 実装-3,
 * independent audit P2-2): `logoutAndLeave`'s `notify` for `Header.svelte`
 * and the command palette. Kept apart from `logout.svelte.ts` so that module
 * (and its tests) needs no UI strings.
 */
import { notify } from '@banto/admin-core';
import type { LogoutOutcome } from './logout.svelte';

export function notifyLogoutOutcome(outcome: Exclude<LogoutOutcome, 'left'>): void {
	notify(
		'error',
		outcome === 'stayed'
			? 'ログアウトできませんでした。ログインしたままです。'
			: 'ログイン状態を確認できませんでした。しばらくしてから再試行してください。'
	);
}
