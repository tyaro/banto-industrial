/**
 * (app) ルートガードの判断（banto v1.7.0 #204）。`(app)/+layout.ts` と
 * vitest が共有する。
 *
 * `@banto/admin-core` の `resolveProtectedSession` は、セッションが無効だと
 * **確認できたとき**（トークン無し・`401`・`false`）だけ `'login'` を返し、
 * サーバーが照合できなかったとき（`500`・接続不能、Tauri の `auth_check` の
 * DB エラー）は reject する。reject のときはトークン（Remember me を含む）に
 * 触れない。ここでは reject を `'unverified'` にまとめ、ガードはログイン画面
 * へ送らずにエラー画面（`routes/+error.svelte`、再試行付き）を出す。一時的な
 * DB エラーで全員がログアウトされる・ログイン画面へ飛ぶ、を起こさないため。
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

/**
 * 起動時の「組み込みサーバーが配信しているタブか」の判定（`setup.ts` の
 * `isEmbeddedServer`）に使う、`GET /api/auth/check` の応答の読み方。
 * `200`/`401` のほか、Banto の JSON エラー本文（`{ "kind": ... }`）を持つ
 * 応答（照合中の DB エラーの `500` など）も「このサーバー」と数える - banto
 * v1.7.0 #204 で、`500` をサーバー無しと判定してデモ用のプロバイダーへ
 * 落ちる不具合が見つかったため（admin-template の `environment.ts` と同じ）。
 */
export async function isBantoAuthCheckResponse(response: Response): Promise<boolean> {
	if (response.status === 200 || response.status === 401) return true;
	const body: unknown = await response.json().catch(() => null);
	return (
		typeof body === 'object' &&
		body !== null &&
		typeof (body as { kind?: unknown }).kind === 'string'
	);
}
