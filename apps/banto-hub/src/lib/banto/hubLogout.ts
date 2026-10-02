/**
 * banto-hub のログアウトの入口（Header・コマンドパレット）。banto v2.0.0 #260。
 *
 * - 通常: admin-template の `logoutAndLeave`（`logout()` → `resolveSettled()` →
 *   確定した `none` のときだけ /login。届かなければトースト）。
 * - **試運転モードの合成セッションの間**: `logoutAndLeave` は使えない。試運転の
 *   セッションは controller が `adopt` で確定しているので、adopt 中の
 *   `resolveSettled()` は provider に問い合わせず常に試運転のセッションを返し
 *   （S-45、設計 §5.1 の表「ログアウトの継続」）、`'stayed'`（「ログアウト
 *   できませんでした」）になる。v1 と同じく、保存しているアカウントのトークンを
 *   `logout()` で捨ててからログイン画面を見せる。試運転の合成セッションは
 *   終わらない（終わらせるのは設定画面のロックダウンだけ、
 *   `commissioningLockDown.ts`）ので、ログイン画面から保護画面へ戻れば、ガードは
 *   また試運転として通す（v1 と同じ）。`logout()` は adopt 中の資格情報の変化
 *   なので保留にならず generation も動かない（S-46）が、/login への遷移の間は
 *   念のため配線①を止める（`leaveForLogin`）。
 *   この挙動（試運転中のログアウトは保存しているログインを捨てるだけで、試運転は
 *   終わらない）は 2026-10-02 のオーナー決定（banto-hub-operations.md §19
 *   「試運転中のログアウト」）。
 */
import { getAuthProvider, getSessionController } from '@banto/admin-core';
import { isCommissioningSession } from './commissioningPolicy';
import { leaveForLogin, logoutAndLeave, type LogoutOutcome } from './logout.svelte';
import { notifyLogoutOutcome } from './logoutNotice';

export async function hubLogout(goToLogin: () => Promise<void>): Promise<LogoutOutcome> {
	const controller = getSessionController();
	if (isCommissioningSession(controller.snapshot)) {
		try {
			await getAuthProvider().logout();
		} catch {
			// トークンの破棄に失敗しても、試運転の画面からログイン画面へは移れる（v1 と同じ）。
		}
		await leaveForLogin(goToLogin);
		return 'left';
	}
	return logoutAndLeave(goToLogin, { notify: notifyLogoutOutcome, controller });
}
