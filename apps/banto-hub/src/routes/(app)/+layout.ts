import { redirect } from '@sveltejs/kit';
import { getAuthProvider } from '@banto/admin-core';
import { bantoReady } from '$lib/banto/setup';
import {
	fetchCommissioningStatusOrNull,
	shouldBypassLoginForCommissioning
} from '$lib/banto/commissioning';
import { sessionStore } from '$lib/session.svelte';
import { settings } from '$lib/settings.svelte';

// relay-wright の同名ファイルから複製。(app) グループ全体の認証ガード
// （AuthProvider.check() ベース）+ sessionStore（identity/role）の初期化。
//
// 試運転モード対応（設計 §5.6・2026-08-30 オーナー決定）: 認証チェック
// （`getAuthProvider().check()`）より**前**に `GET /api/commissioning/status`
// （未認証で読める）を問い合わせ、試運転モード（未ロックダウン）だと
// 確認できた場合だけログインを丸ごと迂回する。バックエンドは未ロックダウン
// 中、認証ヘッダの有無に関わらず全リクエストを合成 admin identity として
// 受け付ける（`actor_identity`、`apps/banto-hub/core/src/commissioning.rs`）
// ので、ここでログインを要求してしまうと「認証なしで管理できる」はずの
// 試運転モードなのに実機でログイン画面に阻まれる（このタスクの発端になった
// 不具合）。
//
// 分岐は3つ、判定は `shouldBypassLoginForCommissioning`（純関数、
// `$lib/banto/commissioning.ts`・vitest と共有）に集約している:
//   1. 試運転モード確定 → ログイン迂回、`sessionStore.enterCommissioningMode()`
//   2. ロックダウン済み確定 → 従来どおり `check()` → だめなら `/login`
//   3. 状態取得に失敗（ネットワーク断など） → **安全側に倒し** 2. と同じ
//      扱い（`fetchCommissioningStatusOrNull` が失敗を `null` にまとめる - 実装指示
//      「取得に失敗した場合は安全側（ログインを要求する）に倒すこと」）。
export async function load() {
	await bantoReady;

	const commissioningStatus = await fetchCommissioningStatusOrNull();
	if (shouldBypassLoginForCommissioning(commissioningStatus)) {
		sessionStore.enterCommissioningMode();
	} else {
		if (!(await getAuthProvider().check())) {
			redirect(307, '/login');
		}
		await sessionStore.load();
	}

	// セッション確定後に UiSettingsProvider から設定を読み直す
	// （他クライアントで保存された値がこのタブの localStorage キャッシュに
	// 優先する）。fire-and-forget: ナビゲーションを待たせない/失敗させない。
	void settings.syncFromProvider();
}
