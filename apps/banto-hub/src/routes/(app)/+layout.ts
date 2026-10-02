// banto v2.0.0（タグ v2.0.0 = dc61fc1）の admin-template
// `apps/admin-template/src/routes/(app)/+layout.ts` を写した（v2 移行 PR1d）。
// banto-hub 固有の差:
// - 確定の入口は `resolveSettled` を直接ではなく、試運転の policy runner
//   （`$lib/banto/commissioningPolicy.ts`、`mode: 'guard'`）。試運転モード
//   （未ロックダウン）なら合成セッションを `adopt` で確定し、そうでなければ
//   通常の確認（`resolveSettled`）へ残りの期限で引き継ぐ（設計 §6.2）。
// - 閲覧公開（viewer-public）が無いので、確定した `none` は
//   `publicViewerFallback` を経ずにそのまま /login（公開閲覧のナビ制限も無い）。
// - エラー画面の本文は i18n ではなく `SESSION_CHECK_FAILED_MESSAGE`（日本語）。
//   ロケールの同期は無い。`base` は使わない。
import { error, redirect } from '@sveltejs/kit';
import { getSessionController } from '@banto/admin-core';
import { bantoReady } from '$lib/banto/setup';
import { SESSION_CHECK_FAILED_MESSAGE } from '$lib/banto/sessionGuard';
import { runCommissioningPolicy } from '$lib/banto/commissioningPolicy';
import { settings } from '$lib/settings.svelte';

// (app) グループ全体の認証ガード（banto #260、設計 §6.1・§6.2）。セッションの
// 確定は SessionController（「誰がログインしているか」の唯一の書き手、ADR-0016）。
// この load の副作用は controller の確定（と方針の `adopt`/`end`）だけで、ストアには
// 書かない（`sessionStore` は `controller.snapshot` からの導出）。
//
// 試運転モード（設計 §5.6・2026-08-30 オーナー決定）: 認証の確認より**前**に
// `GET /api/commissioning/status`（未認証で読める）を問い合わせ、試運転モード
// （未ロックダウン）だと確認できた場合だけログインを迂回する。バックエンドは
// 未ロックダウン中、認証ヘッダの有無に関わらず全リクエストを合成 admin として
// 受け付ける（`actor_identity`、`apps/banto-hub/core/src/commissioning.rs`）。
// 判断は policy runner（`mode: 'guard'`）:
//   1. 試運転モード確定 → `adopt(COMMISSIONING_IDENTITY, 'commissioning', ticket)`
//      （同じ C の再 adopt は generation 据え置き、S-44）
//   2. ロックダウン済み確定 → 試運転の合成セッションがあれば `end` してから、
//      通常の確認（`resolveSettled`）
//   3. 状態の取得に失敗（ネットワーク断など）→ **安全側に倒し** 2. と同じ扱い
//      （迂回しない。v1 から同じ）
//
// banto v1.7.0 #204 から: セッションが無効だと**確認できたとき**（確定した
// `none`）だけ `/login` へ送る。確認できなかったとき（照合の 500・到達不能・
// 10 秒の期限切れ・セッションが動き続けた・方針のやり直しの上限）は、保存して
// いるトークン（Remember me を含む）を消さずに、再試行付きのエラー画面
// （`routes/+error.svelte`）で止まる（S-36/S-60: 切り替えの後でも自動では
// 戻らない）。
export async function load() {
	await bantoReady;
	const controller = getSessionController();
	const result = await runCommissioningPolicy(controller, { mode: 'guard' });
	if (result.outcome === 'unverified') {
		error(503, { message: SESSION_CHECK_FAILED_MESSAGE });
	}
	if (result.snapshot.status !== 'active') redirect(307, '/login');

	// セッション確定後に UiSettingsProvider から設定を読み直す
	// （他クライアントで保存された値がこのタブの localStorage キャッシュに
	// 優先する）。fire-and-forget: ナビゲーションを待たせない/失敗させない。
	void settings.syncFromProvider();

	// この load が確認できた generation（I-16）。いまの `snapshot.generation` を
	// 読み直さない: 上の await の後に別のセッションが確定していることがあり、
	// この load のデータは前のセッションのもの。`+layout.svelte` はこれが
	// 生きている generation の間だけページを描き（世代ゲート）、違えば load を
	// 走らせ直す（配線①）。
	return { sessionGeneration: result.snapshot.generation };
}
