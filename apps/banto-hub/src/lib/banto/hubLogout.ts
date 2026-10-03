/**
 * banto-hub のログアウトの入口（Header・コマンドパレット）。banto v3.0.0。
 *
 * 常に admin-template の `logoutAndLeave`（`logout()` → `resolveSettled()` →
 * 確定した `none` のときだけ /login。届かなければトースト）に委ねる。
 *
 * 試運転モードの grant のセッション（kind `commissioning`）も同じ:
 * `logout()`（`POST /api/auth/logout`）が grant のトークンを無効にして provider が
 * 捨て、確認は `none` → /login（`'left'`）。試運転はロックダウンでしか終わらない
 * ので（2026-10-02 オーナー決定、banto-hub-operations.md §19「試運転中の
 * ログアウト」）、ログイン画面から保護画面へ戻れば、ガード（`(app)/+layout.ts`）の
 * `grantFallback` が `grants.commissioning` の true を見て grant を無言で再発行する。
 * v2 までの試運転専用の分岐（`adopt` した合成セッションは `resolveSettled` が
 * 常に `confirmed` を返すので `logoutAndLeave` が使えなかった）は不要になった。
 */
import { getSessionController } from '@banto/admin-core';
import { logoutAndLeave, type LogoutOutcome } from './logout.svelte';
import { notifyLogoutOutcome } from './logoutNotice';

export async function hubLogout(goToLogin: () => Promise<void>): Promise<LogoutOutcome> {
	return logoutAndLeave(goToLogin, {
		notify: notifyLogoutOutcome,
		controller: getSessionController()
	});
}
