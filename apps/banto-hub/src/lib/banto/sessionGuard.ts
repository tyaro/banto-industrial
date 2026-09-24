/**
 * (app) ルートガードの判断（banto v1.7.0 #204）。`(app)/+layout.ts` と
 * vitest が共有する。
 *
 * `@banto/admin-core` の `resolveProtectedSession` は、セッションが無効だと
 * **確認できたとき**（トークン無し・`401`・`false`）だけ `'login'` を返し、
 * サーバーが照合できなかったとき（`500`・接続不能）は reject する。reject のときはトークン（Remember me を含む）に
 * 触れない。ここでは reject を `'unverified'` にまとめ、ガードはログイン画面
 * へ送らずにエラー画面（`routes/+error.svelte`、再試行付き）を出す。一時的な
 * DB エラーで全員がログアウトされる・ログイン画面へ飛ぶ、を起こさないため。
 *
 * banto-hub は常にサーバー配信（`setup.ts` の `getBantoMode()` は `'server'`
 * だけ）で、デスクトップの窓も同じ HTTP の UI を開くので、Tauri の
 * `auth_check` もデモ用プロバイダーへの切り替えも無い。
 */
import { resolveProtectedSession, type AuthProvider } from '@banto/admin-core';

export type ProtectedRouteDecision = 'session' | 'login' | 'unverified';

/** 照合できなかったときのエラー画面の本文。 */
export const SESSION_CHECK_FAILED_MESSAGE =
	'ログイン状態を確認できませんでした。サーバーがアカウントを確認できませんでした。ログイン状態はそのまま保たれています。しばらくしてから再試行してください。';

export async function decideProtectedRoute(auth: AuthProvider): Promise<ProtectedRouteDecision> {
	let outcome: Awaited<ReturnType<typeof resolveProtectedSession>>;
	try {
		outcome = await resolveProtectedSession(auth);
	} catch {
		return 'unverified';
	}
	// `'publicViewer'` は閲覧公開のあるアプリだけが返す（このアプリには無い）。
	// 入れたなら保護ルートに進めてよいので `'session'` と同じ扱い。
	return outcome === 'login' ? 'login' : 'session';
}
