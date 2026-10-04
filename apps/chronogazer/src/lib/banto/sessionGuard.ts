/**
 * (app) ルートガードが使う小物（banto v1.7.0 #204）。
 *
 * banto v2.0.0（#260）でセッションの確定は SessionController の役目になり、
 * v1 の `resolveProtectedSession` を包んでいた `decideProtectedRoute` は
 * 削除した。ガード本体は `routes/(app)/+layout.ts`（`resolveSettled` →
 * `unverified` は 503、確定した `none` は閲覧公開の `grantFallback` を経て、
 * 入れなければ /login - I2b）。ここに残るのは:
 *
 * - 照合できなかったとき（`unverified`: 照合の `500`・到達不能、Tauri の
 *   `auth_resolve` の DB エラー、10 秒の期限切れ）のエラー画面の本文。
 *   トークン（Remember me を含む）は消さず、`routes/+error.svelte` の
 *   「再試行」でガードをもう一度走らせる。一時的な DB エラーで全員が
 *   ログアウトされる・ログイン画面へ飛ぶ、を起こさないため。
 *
 * 起動時の「組み込みサーバーが配信しているタブか」の判定に使っていた
 * `isBantoAuthCheckResponse` は、banto v3.0.0（#286、I2c）で admin-template の
 * `environment.ts` の `probeBackend`（3 値: server / none / unreachable）に
 * 置き換えて削除した（`200`/`401`・Banto の JSON エラー本文を server と数える
 * 読み方は `probeBackend` がそのまま持っている）。
 */

/** 照合できなかったときのエラー画面の本文。 */
export const SESSION_CHECK_FAILED_MESSAGE =
	'ログイン状態を確認できませんでした。サーバーがアカウントを確認できませんでした。ログイン状態はそのまま保たれています。しばらくしてから再試行してください。';
