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
import { BANNER, recordTraffic, seedNavLayout, SITE } from './helpers';

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

function pathnameOf(href: string, baseURL: string, base: string): string {
	return new URL(href, `${baseURL}${base}/`).pathname.replace(/\/$/, '');
}

/** 1 つの面のリンクの href（重複なし）。 */
async function menuHrefs(page: Page, selector: string): Promise<string[]> {
	const hrefs = await page
		.locator(selector)
		.evaluateAll((links) => links.map((link) => link.getAttribute('href') ?? ''));
	return [...new Set(hrefs)];
}

/**
 * メニューの面。メニューバー（既定。ヘッダーの中）、サイドバー、ドック（`launcher`）、
 * 狭い幅（≤900px）のサイドバー（ハンバーガーで開くドロワー）。Hub・ChronoGazer のどちらにも
 * 4 つとも有る（`ShellMenubar`・`Sidebar`・`ShellLauncher` が両アプリに有る）。
 */
const SURFACES = [
	{ name: 'メニューバー', layout: null, root: 'header', width: 1280, drawer: false },
	{ name: 'サイドバー', layout: 'sidebar', root: 'aside', width: 1280, drawer: false },
	{ name: 'ドック', layout: 'launcher', root: 'nav', width: 1280, drawer: false },
	{ name: '狭い幅のドロワー', layout: 'sidebar', root: 'aside', width: 800, drawer: true }
] as const;

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
		for (const surface of SURFACES) {
			test(`メニュー（${surface.name}）は対象画面だけで、押すとアプリの中で開き、読み直しても出る`, async ({
				page,
				baseURL
			}) => {
				const traffic = recordTraffic(page, new URL(baseURL!).origin);
				await page.setViewportSize({ width: surface.width, height: 900 });
				if (surface.layout) await seedNavLayout(page, surface.layout);
				await page.goto(`${app.base}${app.entry}`);
				await expect(page.getByText(BANNER)).toBeVisible();
				await expect(
					page.getByRole('heading', { name: heading(app.entry), exact: true })
				).toBeVisible();

				const selector = `${surface.root} a[href]`;
				// ドロワーは閉じていると見えない。開いてから触る（開いていれば何もしない）。
				const open = async () => {
					if (surface.drawer && !(await page.locator(selector).first().isVisible())) {
						await page.locator('header button[aria-label*="メニュー"]').first().click();
					}
				};
				// 置き場所の設定は起動の後に届くので、その面が出るのを待つ。
				await expect.poll(async () => (await menuHrefs(page, selector)).length).toBeGreaterThan(0);
				await open();
				// 置き場所が効いている（既定のメニューバーはヘッダーの中の nav）。
				if (surface.layout) await expect(page.locator('header nav a[href]')).toHaveCount(0);
				const hrefs = await menuHrefs(page, selector);
				const allowed = app.targets.map((t) => `${app.base}${t.path}`);
				for (const href of hrefs) {
					expect(allowed, `${surface.name}に対象外のリンク: ${href}`).toContain(
						pathnameOf(href, baseURL!, app.base)
					);
				}
				// 全対象画面へ行ける。
				for (const target of app.targets) {
					expect(
						hrefs.some((h) => pathnameOf(h, baseURL!, app.base) === `${app.base}${target.path}`)
					).toBe(true);
				}

				for (const href of hrefs) {
					const pathname = pathnameOf(href, baseURL!, app.base);
					const target = app.targets.find((t) => `${app.base}${t.path}` === pathname)!;
					await open();
					const before = traffic.documents.length;
					await page.locator(`${surface.root} a[href="${href}"]`).first().click();
					await expect.poll(() => new URL(page.url()).pathname.replace(/\/$/, '')).toBe(pathname);
					await expect(
						page.getByRole('heading', { name: target.heading, exact: true })
					).toBeVisible();
					// アプリの中の移動（新しい文書の要求は無い）。
					expect(traffic.documents.length, `${href} を押して文書を読み直した`).toBe(before);

					// その URL を読み直しても 404 にならない（静的な入口がある）。
					await page.reload();
					await expect(page.getByText(BANNER)).toBeVisible();
					await expect(
						page.getByRole('heading', { name: target.heading, exact: true })
					).toBeVisible();
					expect(traffic.documents.length).toBeGreaterThan(before);
				}

				// コマンドパレットにも対象外の画面・ログアウトは出ない。
				if (!surface.layout) {
					await page.keyboard.press('Control+k');
					const palette = page.getByRole('dialog');
					await expect(palette).toBeVisible();
					await expect(palette.getByText('テーマ').first()).toBeVisible();
					for (const outOfScope of ['ユーザー管理', 'ログアウト', '構成パッケージ', '設定']) {
						await expect(palette.getByText(outOfScope)).toHaveCount(0);
					}
					await page.keyboard.press('Escape');
				}

				await page.waitForTimeout(1000);
				expect(traffic.forbidden()).toEqual([]);
			});
		}

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

		test('ログアウトは無く、「デモを最初から」で文書を読み直し、状態が最初に戻る', async ({
			page,
			baseURL
		}) => {
			const traffic = recordTraffic(page, new URL(baseURL!).origin);
			await page.goto(`${app.base}${app.entry}`);
			await expect(page.getByText(BANNER)).toBeVisible();
			await expect(
				page.getByRole('heading', { name: heading(app.entry), exact: true })
			).toBeVisible();

			await expect(page.getByRole('button', { name: 'ログアウト' })).toHaveCount(0);
			await expect(page.getByRole('link', { name: 'ログアウト' })).toHaveCount(0);

			// 別の画面へ動いてから最初に戻れること（ChronoGazer は 2 画面ある）。
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

			// メモリの状態を目に見える形で変える（コマンドパレットでダークテーマ。`<html data-theme>`）。
			const theme = () => page.evaluate(() => document.documentElement.dataset.theme);
			expect(await theme()).toBe('light');
			await page.keyboard.press('Control+k');
			await page.getByRole('dialog').getByText('ダークテーマにする').click();
			await expect.poll(theme).toBe('dark');

			// 押すと SvelteKit の goto ではなく、入口の文書を読み直す（メモリの状態が捨てられる）。
			const documentsBefore = traffic.documents.length;
			await page.getByRole('button', { name: 'デモを最初から', exact: true }).click();
			await expect.poll(() => traffic.documents.length).toBeGreaterThan(documentsBefore);
			expect(new URL(traffic.documents.at(-1)!).pathname.replace(/\/$/, '')).toBe(
				`${app.base}${app.entry}`
			);
			await expect(page).toHaveURL(new RegExp(`${app.base}${app.entry}/?$`));
			await expect(page).not.toHaveURL(/\/login/);
			await expect(page.getByText(BANNER)).toBeVisible();
			await expect(
				page.getByRole('heading', { name: heading(app.entry), exact: true })
			).toBeVisible();
			// 変えた状態が最初に戻っている。
			await expect.poll(theme).toBe('light');

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
