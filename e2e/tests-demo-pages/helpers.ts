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
