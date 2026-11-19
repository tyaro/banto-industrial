/**
 * @banto/admin-core を banto-hub 用に配線する（root layout から副作用
 * import される）。
 *
 * relay-wright の同名ファイルから複製した上で大きく単純化している:
 * banto-hub は Tauri シェルを持たない headless axum サーバー専用のブラウザ
 * 配信 UI なので、relay-wright の3環境分岐（Tauri / 組み込みサーバー越し
 * ブラウザ / プレーン `vite dev` デモ）のうち **HTTP 一択**になる
 * （実装指示: 「Tauri 分岐...全削除。HTTP プロバイダ一択」）。
 *
 * - `createHttpAuthProvider` → `createHttpDataProvider` →
 *   `connectEvents(createSseEventProvider(...))` の順で配線する
 *   （`banto_server::auth_routes`/`sse_route` はどのアプリでも同型）。
 * - `getBantoMode()` は 'server' 固定で残す — `tagRegistryAdmin.ts` 等
 *   コピー元のクライアントコードが `getBantoMode() === 'tauri'` 分岐を
 *   含んだまま無改変で動くようにするため（実装指示: 「コピーしたクライ
 *   アントが無改変で動くように」）。
 * - UI 設定（#1147）: `createHttpUiSettings`（`/api/ui-settings/{key}`、利用者ごと）を
 *   `bantoReady` の中で差し替える。Hub は Tauri のコマンド経路を持たず（デスクトップ版の
 *   画面も HTTP で読み込む）、`createTauriUiSettings` は使わない。差し替え前（モジュールの
 *   読み込み直後）は localStorage 版で、`settings.svelte.ts` の同期書き込みと FOUC 写しが
 *   そのまま動く。
 */
import {
	connectEvents,
	createHttpAuthProvider,
	createHttpDataProvider,
	createHttpUiSettings,
	createLocalUiSettings,
	createSseEventProvider,
	initBanto
} from '@banto/admin-core';
import type { Notifier, UiSettingsProvider } from '@banto/admin-core';
import { toastStore } from '#lib/toast.svelte.js';
import { purgeLoopbackRememberedToken } from './loopbackTokenPurge';

/** usersAdmin.ts 等の CSRF ヘッダー（管理系 REST 全体で共通）。 */
export const CSRF_HEADER = { 'X-Banto-Client': 'banto' } as const;

/**
 * relay-wright の3環境判定の名残 — banto-hub は常に 'server'
 * （embed-ui 配信の axum が相手）。コピー元クライアント
 * （tagRegistryAdmin.ts 等）の `getBantoMode() === 'tauri'` 分岐は
 * 到達しないコードとして残る。
 */
export type BantoMode = 'server';
export function getBantoMode(): BantoMode {
	return 'server';
}

/**
 * UI 設定の永続化（#1147）。`bantoReady` が解決するまでは localStorage 版（写しだけ）で、
 * 解決後は Hub のサーバー（`/api/ui-settings/{key}`、利用者ごと）へ差し替わる。未ログインの
 * 書き込みはサーバーで 401 になるが、呼び出し側（`settings.svelte.ts`）が握りつぶす
 * （localStorage の写しは常に書く）。
 */
let uiSettings: UiSettingsProvider = createLocalUiSettings();
export function getUiSettings(): UiSettingsProvider {
	return uiSettings;
}

const notifier: Notifier = { notify: (kind, message) => toastStore.push(kind, message) };

/**
 * `initBanto()` 完了 + EventProvider 接続完了を待てる Promise。
 * `routes/+layout.svelte` はこれを `{#await}` してから子を描画し、
 * `(app)/+layout.ts` のガードもこれを待ってから `getAuthProvider()` を呼ぶ。
 *
 * #644 PR5: loopback の origin に残った Remember me のトークンは、provider が読む**前**に
 * サーバーで失効させて `localStorage` から消す（ガードが古いトークンを読まないよう、
 * この Promise が掃除の完了を待つ）。
 */
export const bantoReady: Promise<void> = (async () => {
	await purgeLoopbackRememberedToken();
	const authProvider = createHttpAuthProvider();
	const dataProvider = createHttpDataProvider({ getToken: authProvider.getToken });
	uiSettings = createHttpUiSettings({ getToken: authProvider.getToken });
	initBanto({ dataProvider, authProvider, notifier, resources: [] });
	connectEvents(createSseEventProvider({ getToken: authProvider.getToken }));
})();
