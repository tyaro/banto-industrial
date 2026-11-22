/**
 * デモの E2E の共有（`demo-pages.spec.ts`・`public-gate.spec.ts`）。配信の場所・帯の文言・
 * 「本物へ送らない」の記録。
 */
import type { Page } from '@playwright/test';

export const SITE = '/banto-industrial';
export const BANNER = 'デモ版 — 機器にはつながりません。読み直すと元に戻ります';

/** ページの全部の要求と WebSocket を記録する。 */
export function recordTraffic(page: Page, origin: string) {
	const requests: string[] = [];
	const sockets: string[] = [];
	const documents: string[] = [];
	page.on('request', (request) => {
		requests.push(request.url());
		if (request.resourceType() === 'document') documents.push(request.url());
	});
	page.on('websocket', (ws) => sockets.push(ws.url()));
	const unsupported: string[] = [];
	page.on('console', (message) => {
		if (message.text().startsWith('demo: 模擬に口がありません')) unsupported.push(message.text());
	});
	return {
		documents,
		/** 模擬に口が無く 501 を返した `/api`（`src/lib/demo/` の警告）。 */
		unsupported,
		/** 本物の `/api`・ほかのオリジン・WebSocket（あってはならないもの）。 */
		forbidden(): string[] {
			const bad = requests.filter((raw) => {
				const url = new URL(raw);
				if (url.protocol === 'data:' || url.protocol === 'blob:') return false;
				return url.origin !== origin || url.pathname === '/api' || url.pathname.includes('/api/');
			});
			return [...bad, ...sockets.map((s) => `WebSocket ${s}`)];
		}
	};
}

/**
 * ナビの置き場所を、ページの読み込みの前に決めておく（`shell.navLayout` の UI 設定の GET へ
 * 答える）。デモの localStorage・UI 設定はメモリだけで、設定の画面は対象外なので、画面からは
 * 切り替えられない。デモの模擬は `window.fetch` へ代入して差し込まれるので、その代入を
 * 受けて、模擬の前に挟む。`addInitScript` なので読み直しの後も同じ置き場所になる。
 */
export async function seedNavLayout(page: Page, layout: 'sidebar' | 'launcher'): Promise<void> {
	await page.addInitScript((value) => {
		const native = window.fetch.bind(window);
		let demo: typeof fetch | null = null;
		const seeded = (input: RequestInfo | URL, init?: RequestInit): Response | null => {
			const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
			const method = (
				init?.method ?? (input instanceof Request ? input.method : 'GET')
			).toUpperCase();
			if (method === 'GET' && /\/api\/ui-settings\/shell\.navLayout(\?|$)/.test(url)) {
				return new Response(JSON.stringify({ value }), {
					status: 200,
					headers: { 'content-type': 'application/json' }
				});
			}
			return null;
		};
		const beforeDemo: typeof fetch = async (input, init) =>
			seeded(input, init) ?? native(input, init);
		const afterDemo: typeof fetch = async (input, init) =>
			seeded(input, init) ?? demo!(input, init);
		Object.defineProperty(window, 'fetch', {
			configurable: true,
			get: () => (demo ? afterDemo : beforeDemo),
			set: (next: typeof fetch) => {
				demo = next;
			}
		});
	}, layout);
}
