/**
 * ChronoGazer の閲覧公開（LAN の viewer-public セッション）でのメニューバー表示（#1159、
 * banto の `e2e/tests-public-viewer/public-viewer-menubar.spec.ts` と同じ形、
 * nav-menubar-plan.md §4.11）: ヘッダーのナビは `visibleNavItems()` が絞った許可リスト
 * （監視・ヒストリカル・イベント）だけを持ち、サイドバーも管理グループも出ない。合成の
 * 閲覧者に保存済みの置き場所は無いので、`GET /api/ui-settings/shell.navLayout` は route で
 * 答える（サーバーには何も残さない）。自前のコンテキストで、シナリオ側の
 * `public-viewer.spec.ts` には影響しない。
 */
import { expect, test } from '@playwright/test';

test('viewer-public: メニューバーは許可リストの項目だけで、管理・設定は出ない', async ({
	browser
}) => {
	const context = await browser.newContext({ reducedMotion: 'reduce' });
	const page = await context.newPage();
	try {
		await page.route('**/api/ui-settings/shell.navLayout', async (route) => {
			if (route.request().method() !== 'GET') return route.continue();
			await route.fulfill({ json: { value: 'menubar' } });
		});
		await page.addInitScript(() => window.localStorage.setItem('banto.navLayout', 'menubar'));

		await page.goto('/monitor');
		await expect(page.getByRole('heading', { level: 2, name: '監視' })).toBeVisible();
		await expect(page.getByRole('banner').getByRole('button', { name: 'ログイン' })).toBeVisible();

		const nav = page.getByRole('banner').getByRole('navigation', { name: '主要ナビゲーション' });
		await expect(nav.getByRole('link', { name: /監視/ })).toBeVisible();
		await expect(page.locator('aside')).toHaveCount(0);
		await expect(nav.getByRole('button', { name: /管理/ })).toHaveCount(0);

		// バーの中身すべて（「その他」が出ていれば開いて）: 許可リストのパスだけ。
		const more = nav.getByRole('button', { name: /その他/ });
		if (await more.isVisible()) await more.click();
		const paths = await nav
			.getByRole('link')
			.evaluateAll((links) =>
				links.map((link) => new URL((link as HTMLAnchorElement).href).pathname)
			);
		for (const allowed of ['/monitor', '/historical', '/events']) {
			expect(paths).toContain(allowed);
		}
		for (const hidden of ['/tags', '/groups', '/users', '/audit-log', '/settings/appearance']) {
			expect(paths).not.toContain(hidden);
		}
	} finally {
		await context.close();
	}
});
