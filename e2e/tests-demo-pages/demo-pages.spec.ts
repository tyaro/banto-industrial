/**
 * GitHub Pages のデモの受け入れ（docs/demo-pages-design.md §3.5・§6 の 2）。
 *
 * 組み立てた成果物（`pnpm demo:build`）を fallback の無い静的なサーバーで配り、
 *
 * 1. 対象の 3 つの URL を直に開き、読み直しても画面（デモの帯とその画面の見出し）が出ること。
 * 2. その間、本物の `/api` への要求・WebSocket の接続・ほかのオリジンへの要求が 0 件である
 *    こと（模擬は `fetch` の中で答えるのでネットワークに出ない。出たら振り分けの制約が
 *    破れている）。
 * 3. 模擬の初期化を失敗させたら（デモのビルドにだけある `?demo-fail-init=1`）、
 *    「デモを始められませんでした」で止まり、通常の provider が動かないこと（模擬を入れる
 *    前に失敗させるので、provider が動けば本物の `/api` への要求が見える）。
 */
import { expect, test, type Page } from '@playwright/test';

const SITE = '/banto-industrial';
const BANNER = 'デモ版 — 機器にはつながりません。読み直すと元に戻ります';

const TARGETS = [
	{ name: 'Hub /tags', path: `${SITE}/hub/tags`, heading: 'タグ登録' },
	{ name: 'ChronoGazer /tags', path: `${SITE}/chronogazer/tags`, heading: '記録' },
	{ name: 'ChronoGazer /monitor', path: `${SITE}/chronogazer/monitor`, heading: '監視' }
] as const;

/** ページの全部の要求と WebSocket を記録する。 */
function recordTraffic(page: Page, origin: string) {
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

for (const target of TARGETS) {
	test(`${target.name}: 直に開く・読み直すと画面が出て、本物の API へは送らない`, async ({
		page,
		baseURL
	}) => {
		const traffic = recordTraffic(page, new URL(baseURL!).origin);

		const response = await page.goto(target.path);
		expect(response?.status()).toBe(200);
		await expect(page.getByText(BANNER)).toBeVisible();
		await expect(page.getByRole('heading', { name: target.heading, exact: true })).toBeVisible();
		// 末尾 `/` の転送（静的なサーバー）と SvelteKit の `trailingSlash` が行き来しない:
		// 開くのは 301 の 1 回と入口の 1 回だけ。
		expect(traffic.documents).toEqual([`${baseURL}${target.path}`, `${baseURL}${target.path}/`]);

		await page.reload();
		await expect(page.getByText(BANNER)).toBeVisible();
		await expect(page.getByRole('heading', { name: target.heading, exact: true })).toBeVisible();

		// 起動の後の購読・再接続の試みも拾えるよう、少し置いてから数える。
		await page.waitForTimeout(1500);
		expect(traffic.forbidden()).toEqual([]);
		// 起動で呼ばれる口は全部模擬にある（API の口が変わったら、模擬も同じ PR で直す）。
		expect(traffic.unsupported).toEqual([]);
	});
}

for (const target of [TARGETS[0], TARGETS[1]]) {
	test(`${target.name}: 模擬の初期化が失敗したら止まり、通常の provider は動かない`, async ({
		page,
		baseURL
	}) => {
		const traffic = recordTraffic(page, new URL(baseURL!).origin);

		await page.goto(`${target.path}/?demo-fail-init=1`);
		await expect(
			page.getByRole('alert').filter({ hasText: 'デモを始められませんでした' })
		).toBeVisible();
		await expect(page.getByRole('heading', { name: target.heading, exact: true })).toHaveCount(0);
		await expect(page.getByText(BANNER)).toHaveCount(0);

		await page.waitForTimeout(1500);
		expect(traffic.forbidden()).toEqual([]);
	});
}

test('入口と 404.html', async ({ page }) => {
	await page.goto(`${SITE}/`);
	await expect(page.getByRole('link', { name: '/tags' }).first()).toBeVisible();
	await expect(page.getByRole('link', { name: '/monitor' })).toBeVisible();

	// 対象外のパスは SPA として開かず、入口へのリンクだけを出す。
	const response = await page.goto(`${SITE}/hub/users`);
	expect(response?.status()).toBe(404);
	await expect(page.getByRole('link', { name: 'デモの入口へ' })).toHaveAttribute(
		'href',
		`${SITE}/`
	);
});
