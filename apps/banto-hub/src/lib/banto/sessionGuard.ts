/**
 * (app) ルートガードが使う小物（banto v1.7.0 #204）。
 *
 * banto v2.0.0（#260）でセッションの確定は SessionController の役目になり、
 * v1 の `resolveProtectedSession` を包んでいた `decideProtectedRoute` は
 * 削除した。ガード本体は `routes/(app)/+layout.ts`（試運転の policy runner
 * `commissioningPolicy.ts` を `mode: 'guard'` で走らせ、`unverified` は 503、
 * 確定した `none` は /login）。ここに残るのは、照合できなかったとき
 * （`unverified`: 照合の `500`・到達不能・10 秒の期限切れ・セッションが
 * 動き続けた）のエラー画面の本文だけ。トークン（Remember me を含む）は
 * 消さず、`routes/+error.svelte` の「再試行」でガードをもう一度走らせる。
 * 一時的な DB エラーで全員がログアウトされる・ログイン画面へ飛ぶ、を
 * 起こさないため。
 */

/** 照合できなかったときのエラー画面の本文。 */
export const SESSION_CHECK_FAILED_MESSAGE =
	'ログイン状態を確認できませんでした。サーバーがアカウントを確認できませんでした。ログイン状態はそのまま保たれています。しばらくしてから再試行してください。';
