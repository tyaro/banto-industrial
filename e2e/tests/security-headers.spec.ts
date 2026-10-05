/**
 * #500: 組み込みサーバーの応答に banto のセキュリティヘッダー
 * （`banto_server::with_security_headers`）が付き、CSP が UI を壊さないこと。
 *
 * 認証・初回セットアップに依存しない（静的 UI と未認証の `/api/*` の応答
 * ヘッダーだけを見る）ので、ファイル名の辞書順は実行順に影響しない。
 */
import { expect, test, type APIResponse } from '@playwright/test';

function expectSecurityHeaders(response: APIResponse): void {
	const headers = response.headers();
	expect(headers['x-content-type-options']).toBe('nosniff');
	expect(headers['x-frame-options']).toBe('DENY');
	expect(headers['referrer-policy']).toBe('same-origin');
	const csp = headers['content-security-policy'] ?? '';
	expect(csp).toContain("default-src 'self'");
	expect(csp).toContain("frame-ancestors 'none'");
}

test.describe('ChronoGazer のセキュリティヘッダー', () => {
	test('静的 UI と /api/* の応答にヘッダーが付く', async ({ request }) => {
		expectSecurityHeaders(await request.get('/'));
		// 未認証で読める API（`GET /api/auth/status`。CSRF 対策の `X-Banto-Client` は要る）の
		// JSON 応答にも付く。
		const api = await request.get('/api/auth/status', {
			headers: { 'X-Banto-Client': 'banto' }
		});
		expect(api.status()).toBe(200);
		expectSecurityHeaders(api);
	});

	test('CSP に違反せずに UI が描画される', async ({ page }) => {
		const violations: string[] = [];
		page.on('console', (message) => {
			if (message.text().includes('Content Security Policy')) violations.push(message.text());
		});
		await page.goto('/');
		await page.waitForLoadState('networkidle');
		expect(violations).toEqual([]);
	});
});
