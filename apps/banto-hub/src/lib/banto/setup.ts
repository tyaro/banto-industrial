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
 * - `createHttpUiSettings` は使わない: banto-hub バックエンドに
 *   `/api/ui-settings/*` ルートが存在しない（apps/banto-hub/core/src/rest.rs
 *   のルーター一覧参照）ため、`createLocalUiSettings()`（localStorage）に
 *   固定する。これは relay-wright の「プレーンブラウザデモモード」の
 *   フォールバックと同じ実装だが、banto-hub では唯一の選択肢である点が
 *   異なる。
 */
import {
	connectEvents,
	createHttpAuthProvider,
	createHttpDataProvider,
	createLocalUiSettings,
	createSseEventProvider,
	initBanto
} from '@banto/admin-core';
import type { Notifier, UiSettingsProvider } from '@banto/admin-core';
import { toastStore } from '$lib/toast.svelte';

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

/** UI 設定の永続化 - banto-hub には /api/ui-settings が無いため常に localStorage。 */
const uiSettings: UiSettingsProvider = createLocalUiSettings();
export function getUiSettings(): UiSettingsProvider {
	return uiSettings;
}

const notifier: Notifier = { notify: (kind, message) => toastStore.push(kind, message) };

/**
 * `initBanto()` 完了 + EventProvider 接続完了を待てる Promise。
 * `routes/+layout.svelte` はこれを `{#await}` してから子を描画し、
 * `(app)/+layout.ts` のガードもこれを待ってから `getAuthProvider()` を呼ぶ。
 */
export const bantoReady: Promise<void> = (async () => {
	const authProvider = createHttpAuthProvider();
	const dataProvider = createHttpDataProvider({ getToken: authProvider.getToken });
	initBanto({ dataProvider, authProvider, notifier, resources: [] });
	connectEvents(createSseEventProvider({ getToken: authProvider.getToken }));
})();
