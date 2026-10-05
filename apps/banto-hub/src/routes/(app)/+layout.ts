// banto v3.0.0（タグ v3.0.0）の admin-template
// `apps/admin-template/src/routes/(app)/+layout.ts` を写した（v3 移行）。
// banto-hub 固有の差:
// - 確定した `none` のあとの grant の kind が `commissioning`（試運転モード、
//   サーバーが未ロックダウンかつ loopback のときだけ発行する、ADR-0017）。
// - 閲覧公開（viewer-public）が無いので、閲覧公開のナビ制限も無い。
// - エラー画面の本文は i18n ではなく `SESSION_CHECK_FAILED_MESSAGE`（日本語）。
//   ロケールの同期は無い。`base` は使わない。
// banto v4.0.0（SvelteKit 3、#325）: `error(status, message)` の形にし、/login への
// redirect は `resolveAppPath()` を通す（上流と同じ）。上流で自動移行が壊した
// 閲覧公開の許可リスト（`url.pathname` と表の path を比べる所）は banto-hub には
// 無いので、比べ方の書き換えも無い。
import { error, redirect } from '@sveltejs/kit';
import {
	getAuthProvider,
	getSessionController,
	grantFallback,
	resolveSettled
} from '@banto/admin-core';
import { bantoReady } from '#lib/banto/setup.js';
import { SESSION_CHECK_FAILED_MESSAGE } from '#lib/banto/sessionGuard.js';
import { COMMISSIONING_KIND } from '#lib/banto/commissioning.js';
import { settings } from '#lib/settings.svelte.js';
import { resolveAppPath } from '#lib/navigation.js';

// (app) グループ全体の認証ガード（banto #260、設計 §6.1。v3.0.0 で grant 方式）。
// セッションの確定は SessionController（「誰がログインしているか」の唯一の
// 書き手、ADR-0016）。この load の副作用は controller の確認と、確定した
// `none` に対する grant の発行（ADR-0017）だけで、ストアには書かない
// （`sessionStore` は `controller.snapshot` からの導出）。
//
// 試運転モード（設計 §5.6・2026-08-30 オーナー決定）は v3.0.0 から、サーバーが
// 発行する grant で入る通常のセッション（kind `commissioning`）になった。
// 管理 REST・管理 WS は試運転中でも bearer が必須で、認証を迂回する経路は
// 無い。確定した `none` のとき `grantFallback(..., { kind: 'commissioning' })`
// が `GET /api/auth/status` の `grants.commissioning` を読み、true（未ロック
// ダウン かつ 接続元が loopback）なら `POST /api/auth/grant/commissioning` で
// トークンを受け取って確定する。ロックダウン済み・loopback でない・状態が読め
// ない・発行に失敗、のときは `none` のまま（`grantFallback` は何も足さない）
// なので /login へ送る。ロックダウンではサーバーが grant のトークンを全部失効
// させ、次の要求は 401 → `none` → ここで再び grant を試みて、今度は
// `grants.commissioning` が false なので /login になる。
//
// banto v1.7.0 #204 から: セッションが無効だと**確認できたとき**（確定した
// `none`）だけ `/login` へ送る。確認できなかったとき（照合の 500・到達不能・
// 10 秒の期限切れ・セッションが動き続けた）は、保存しているトークン
// （Remember me を含む）を消さずに、再試行付きのエラー画面
// （`routes/+error.svelte`）で止まる（S-36/S-60: 切り替えの後でも自動では
// 戻らない）。`grantFallback` の結果の `unverified` も同じ（S-66）。
export async function load() {
	await bantoReady;
	const controller = getSessionController();
	let result = await resolveSettled(controller, { cause: 'navigation' });
	if (result.outcome === 'unverified') sessionCheckFailed();
	if (result.snapshot.status === 'none') {
		result = await grantFallback(controller, getAuthProvider(), result.ticket, {
			kind: COMMISSIONING_KIND
		});
		if (result.outcome === 'unverified') sessionCheckFailed();
		if (result.snapshot.status !== 'active') redirect(307, resolveAppPath('/login'));
	}
	const snapshot = result.snapshot;

	// セッション確定後に UiSettingsProvider から設定を読み直す
	// （他クライアントで保存された値がこのタブの localStorage キャッシュに
	// 優先する）。fire-and-forget: ナビゲーションを待たせない/失敗させない。
	void settings.syncFromProvider();

	// この load が確認できた generation（I-16）。いまの `snapshot.generation` を
	// 読み直さない: 上の await の後に別のセッションが確定していることがあり、
	// この load のデータは前のセッションのもの。`+layout.svelte` はこれが
	// 生きている generation の間だけページを描き（世代ゲート）、違えば load を
	// 走らせ直す（配線①）。
	return { sessionGeneration: snapshot.generation };
}

/** 再試行付きのエラー画面（Issue #204）: ログイン状態を確認できなかった。 */
function sessionCheckFailed(): never {
	error(503, SESSION_CHECK_FAILED_MESSAGE);
}
