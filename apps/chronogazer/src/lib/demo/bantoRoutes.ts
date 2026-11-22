/**
 * banto の標準の口（`/api/auth/*`・`/api/ui-settings/*`・`/api/events`）の模擬
 * （設計 docs/demo-pages-design.md §3.2・§3.3）。
 *
 * - 認証: いつもログイン済みの admin。トークンの中身では何も判断しない（どのトークンでも、
 *   トークンが無くても admin）。初回の管理者作成・試運転の grant は出さない
 *   （`status` は `initialized: true`・`grants: {}`）。
 * - UI 設定: このタブのメモリだけ（読み直すと消える）。
 * - SSE: つないだまま何も送らない。
 *
 * Hub の `src/lib/demo/bantoRoutes.ts` と同じ形（口は banto の標準で両アプリ共通）。
 */
import type { Identity } from '@banto/admin-core';
import {
	idleEventStream,
	json,
	noContent,
	type DemoHttpRoute
} from '@banto-industrial/demo-backend';

/** デモの利用者。 */
export const DEMO_IDENTITY: Identity = { id: 'demo-admin', name: 'デモ', role: 'admin' };

export function createBantoRoutes(identity: Identity = DEMO_IDENTITY): DemoHttpRoute[] {
	const uiSettings = new Map<string, string>();
	return [
		{ method: 'GET', path: '/api/auth/check', handle: () => json(true) },
		{ method: 'GET', path: '/api/auth/identity', handle: () => json(identity) },
		{
			method: 'GET',
			path: '/api/auth/status',
			handle: () => json({ initialized: true, grants: {} })
		},
		{ method: 'POST', path: '/api/auth/logout', handle: () => noContent() },
		{
			method: 'GET',
			path: '/api/ui-settings/:key',
			handle: (req) => json({ value: uiSettings.get(req.params.key) ?? null })
		},
		{
			method: 'PUT',
			path: '/api/ui-settings/:key',
			handle: async (req) => {
				const body = await req.json<{ value?: unknown }>();
				if (typeof body.value === 'string') uiSettings.set(req.params.key, body.value);
				return noContent();
			}
		},
		{ method: 'GET', path: '/api/events', handle: (req) => idleEventStream(req.signal) }
	];
}

/**
 * デモ専用のトークンのキーを、起動のたびに両方の storage から消してから、このタブの
 * sessionStorage に置き直す（banto#838 の「メモリだけのトークン」が入るまでの回り道、
 * 設計 §3.2）。`createHttpAuthProvider` はトークンが無いと `resolve()` が `none` になり
 * ログイン画面へ送るので、置いておく。中身は模擬が見ないので固定の文字列。
 * 既定のキー `banto.auth.token` は読みも書きもしない。
 */
export function resetDemoToken(storageKey: string): void {
	localStorage.removeItem(storageKey);
	sessionStorage.removeItem(storageKey);
	sessionStorage.setItem(storageKey, 'demo');
}
