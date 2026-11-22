/**
 * デモの公開の関門（docs/demo-pages-design.md §3.4・§6 の 2b-2、2026-11-23 オーナー指摘）。
 *
 * 組み立てた成果物で、公開してよいかを確かめる。**この spec が走らなかったら配信しない**
 * （\`.github/workflows/deploy-demo.yml\` が名指しで走らせ、「テストが無い」も失敗にする）。
 *
 * 1. メニュー（メニューバー・サイドバー・ドック）の全リンクが対象画面だけを指し、押すと
 *    アプリの中で開き、その URL の読み直しでも 404 にならない。
 * 2. 対象外の画面へアプリの中から着いたら（画面内のリンクを SvelteKit のルーターが拾う）、
 *    その画面の中身ではなく「この画面はデモでは使えません」の案内が出て、案内のリンクで
 *    対象の画面へ移れる。
 * 3. ログアウトは無く「デモを最初から」があり、押すと入口が出る（\`/login\` ではない）。
 * 4. その間、本物の \`/api\`・WebSocket・ほかのオリジンへの送信が 0 件。
 */
import { expect, test, type Page } from '@playwright/test';
import { BANNER, recordTraffic, SITE } from './helpers';

const APPS = [
	{
		name: 'Hub',
		base: `${SITE}/hub`,
		entry: '/tags',
		targets: [{ path: '/tags', heading: 'タグ登録' }]
	},
	{
		name: 'ChronoGazer',
		base: `${SITE}/chronogazer`,
		entry: '/monitor',
		targets: [
			{ path: '/monitor', heading: '監視' },
			{ path: '/tags', heading: '記録' }
		]
	}
] as const;

/** メニューの面（メニューバー・サイドバー・ドック・ヘッダー）のリンクの href（重複なし）。 */
async function menuHrefs(page: Page): Promise<string[]> {
	const hrefs = await page
		.locator('header a[href], nav a[href], aside a[href]')
		.evaluateAll((links) => links.map((link) => link.getAttribute('href') ?? ''));
	return [...new Set(hrefs)];
}

/** 画面内のリンクと同じ（アプリの中のリンクは SvelteKit のルーターが拾う）。注入したリンクは押す前に消し直す。 */
async function clickOutOfScopeLink(page: Page, href: string): Promise<void> {
	await page.evaluate((target) => {
		document.getElementById('demo-out-of-scope-link')?.remove();
		const a = document.createElement('a');
		a.href = target;
		a.id = 'demo-out-of-scope-link';
		a.textContent = '対象外へ';
		(document.querySelector('main') ?? document.body).appendChild(a);
	}, href);
	await page.locator('#demo-out-of-scope-link').click();
}

for (const app of APPS) {
	const heading = (path: string) => app.targets.find((t) => t.path === path)!.heading;

	test.describe(app.name, () => {
		test('メニューは対象画面だけで、押すとアプリの中で開き、読み直しても出る', async ({
			page,
			baseURL
		}) => {
			const traffic = recordTraffic(page, new URL(baseURL!).origin);
			await page.goto(`${app.base}${app.entry}`);
			await expect(page.getByText(BANNER)).toBeVisible();

			await expect(
				page.getByRole('heading', { name: heading(app.entry), exact: true })
			).toBeVisible();
			// メニューの面は描画の後で出る（メニューバーは幅を測ってから並べる）。
			await expect.poll(async () => (await menuHrefs(page)).length).toBeGreaterThan(0);
			const hrefs = await menuHrefs(page);
			expect(hrefs.length).toBeGreaterThan(0);
			const allowed = app.targets.map((t) => `${app.base}${t.path}`);
			for (const href of hrefs) {
				const pathname = new URL(href, `${baseURL}${app.base}/`).pathname.replace(/\/$/, '');
				expect(allowed, `メニューに対象外のリンク: ${href}`).toContain(pathname);
			}
			// 全対象画面へ行ける。
			for (const target of app.targets) {
				expect(hrefs.some((h) => h.startsWith(`${app.base}${target.path}`))).toBe(true);
			}

			for (const href of hrefs) {
				await page
					.locator(`header a[href="${href}"], nav a[href="${href}"], aside a[href="${href}"]`)
					.first()
					.click();
				const pathname = new URL(href, `${baseURL}${app.base}/`).pathname.replace(/\/$/, '');
				const target = app.targets.find((t) => `${app.base}${t.path}` === pathname)!;
				await expect.poll(() => new URL(page.url()).pathname.replace(/\/$/, '')).toBe(pathname);
				await expect(
					page.getByRole('heading', { name: target.heading, exact: true })
				).toBeVisible();

				// その URL を読み直しても 404 にならない（静的な入口がある）。
				await page.reload();
				await expect(page.getByText(BANNER)).toBeVisible();
				await expect(
					page.getByRole('heading', { name: target.heading, exact: true })
				).toBeVisible();
			}

			// コマンドパレットにも対象外の画面・ログアウトは出ない。
			await page.keyboard.press('Control+k');
			const palette = page.getByRole('dialog');
			await expect(palette).toBeVisible();
			await expect(palette.getByText('テーマ').first()).toBeVisible();
			for (const outOfScope of ['ユーザー管理', 'ログアウト', '構成パッケージ', '設定']) {
				await expect(palette.getByText(outOfScope)).toHaveCount(0);
			}
			await page.keyboard.press('Escape');

			await page.waitForTimeout(1000);
			expect(traffic.forbidden()).toEqual([]);
		});

		test('対象外の画面へアプリの中から着いたら、案内を出して中身は描かない', async ({
			page,
			baseURL
		}) => {
			const traffic = recordTraffic(page, new URL(baseURL!).origin);
			await page.goto(`${app.base}${app.entry}`);
			await expect(
				page.getByRole('heading', { name: heading(app.entry), exact: true })
			).toBeVisible();

			// 画面内のリンクと同じ（アプリの中のリンクは SvelteKit のルーターが拾う）。
			await clickOutOfScopeLink(page, `${app.base}/users`);

			await expect(
				page.getByRole('heading', { name: 'この画面はデモでは使えません', exact: true })
			).toBeVisible();
			await expect(page).toHaveURL(new RegExp(`${app.base}/users/?$`));
			// 案内には対象画面のリンクだけ。押すと対象画面が開く。
			const notice = page.locator('section.demo-out-of-scope');
			const links = notice.getByRole('link');
			await expect(links).toHaveCount(app.targets.length);
			for (const target of app.targets) {
				await page.locator('section.demo-out-of-scope a', { hasText: target.path }).click();
				await expect(
					page.getByRole('heading', { name: target.heading, exact: true })
				).toBeVisible();
				// 案内を出し直す。
				await clickOutOfScopeLink(page, `${app.base}/users`);
				await expect(
					page.getByRole('heading', { name: 'この画面はデモでは使えません', exact: true })
				).toBeVisible();
			}

			await page.waitForTimeout(1000);
			// 対象外の画面の中身は描かれない = そこが呼ぶ \`/api\` も出ない（本物へも）。
			expect(traffic.forbidden()).toEqual([]);
		});

		test('ログアウトは無く、「デモを最初から」で入口へ戻る', async ({ page, baseURL }) => {
			const traffic = recordTraffic(page, new URL(baseURL!).origin);
			await page.goto(`${app.base}${app.entry}`);
			await expect(page.getByText(BANNER)).toBeVisible();
			await expect(
				page.getByRole('heading', { name: heading(app.entry), exact: true })
			).toBeVisible();

			await expect(page.getByRole('button', { name: 'ログアウト' })).toHaveCount(0);
			await expect(page.getByRole('link', { name: 'ログアウト' })).toHaveCount(0);

			// 別の画面へ動いてから最初に戻れること（ChronoGazer は 2 画面ある。Hub は同じ画面の再読み込み）。
			const other = app.targets.find((t) => t.path !== app.entry);
			if (other) {
				await page
					.locator(
						`header a[href^="${app.base}${other.path}"], nav a[href^="${app.base}${other.path}"]`
					)
					.first()
					.click();
				await expect(page.getByRole('heading', { name: other.heading, exact: true })).toBeVisible();
			}

			await page.getByRole('button', { name: 'デモを最初から', exact: true }).click();
			await expect(page).toHaveURL(new RegExp(`${app.base}${app.entry}/?$`));
			await expect(page).not.toHaveURL(/\/login/);
			await expect(page.getByText(BANNER)).toBeVisible();
			await expect(
				page.getByRole('heading', { name: heading(app.entry), exact: true })
			).toBeVisible();

			await page.waitForTimeout(1000);
			expect(traffic.forbidden()).toEqual([]);
		});
	});
}

test('Hub のルート（/）へアプリの中から行くと、対象画面（/tags）へ入る', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(`${SITE}/hub/tags`);
	await expect(page.getByRole('heading', { name: 'タグ登録', exact: true })).toBeVisible();
	// `routes/+page.ts` の redirect（製品は /status、デモのビルドは /tags）。
	await clickOutOfScopeLink(page, `${SITE}/hub/`);
	await expect.poll(() => new URL(page.url()).pathname.replace(/\/$/, '')).toBe(`${SITE}/hub/tags`);
	await expect(page.getByRole('heading', { name: 'タグ登録', exact: true })).toBeVisible();
	await expect(
		page.getByRole('heading', { name: 'この画面はデモでは使えません', exact: true })
	).toHaveCount(0);
	await page.waitForTimeout(1000);
	expect(traffic.forbidden()).toEqual([]);
});
