/**
 * #714（2026-10-20 オーナー指示）: サイドバーの折り畳みボタンの置き場所。
 *
 * - 広幅・開いている: ロゴ行（サイドバー側）の「サイドバーを折りたたむ」だけが出る。
 *   ヘッダーには ☰ を出さない（トグルが 2 つ同時に見えない）。
 * - 折り畳み中: ヘッダー左端の「サイドバーを開く」が出て、押すと開く。
 * - 狭幅（≤900px）: ヘッダーの「メニューを開く」（オフキャンバス）。
 * - どの遷移でもフォーカスは見えているトグルへ移る / 残る（`<body>` に落とさない）。
 *
 * ファイル名は `banto-hub-smoke` より辞書順で後ろ（`ensureLoggedIn` を使うため）。
 */
import { expect, test, type Page } from '@playwright/test';
import { ensureLoggedIn } from './banto-hub-auth';

const collapseBtn = (page: Page) => page.getByRole('button', { name: 'サイドバーを折りたたむ' });
const openBtn = (page: Page) => page.getByRole('button', { name: 'サイドバーを開く' });

test.describe.serial('banto-hub sidebar toggle placement (#714)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
		await page.goto('/login');
		await ensureLoggedIn(page);
		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'サーバー状態' })).toBeVisible();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 広幅・開いている: 折りたたむボタンはサイドバーのロゴ行にあり、ヘッダーには ☰ が無い', async () => {
		await expect(
			page.locator('aside').getByRole('button', { name: 'サイドバーを折りたたむ' })
		).toBeVisible();
		await expect(page.locator('header [data-nav-toggle]')).toHaveCount(0);
		await expect(page.locator('[data-nav-toggle]:visible')).toHaveCount(1);
		await expect(collapseBtn(page)).toHaveAttribute('aria-expanded', 'true');
		await expect(collapseBtn(page)).toHaveAttribute('aria-controls', 'app-sidebar');
	});

	test('2. 折りたたむと、ヘッダー左端の「開く」ボタンにフォーカスが移る', async () => {
		await collapseBtn(page).click();
		await expect(collapseBtn(page)).toHaveCount(0);
		await expect(
			page.locator('header').getByRole('button', { name: 'サイドバーを開く' })
		).toBeVisible();
		await expect(openBtn(page)).toBeFocused();
		await expect(openBtn(page)).toHaveAttribute('aria-expanded', 'false');
		await expect(page.locator('[data-nav-toggle]:visible')).toHaveCount(1);
	});

	test('3. 折りたたみ中にコマンドパレットを閉じても、フォーカスは見えているトグルへ戻る', async () => {
		await page.keyboard.press('Control+k');
		await expect(page.getByRole('dialog')).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(page.getByRole('dialog')).toBeHidden();
		await expect(openBtn(page)).toBeFocused();
	});

	test('4. 「開く」を押すと広がり、フォーカスはサイドバー側の折りたたむボタンへ移る', async () => {
		await openBtn(page).click();
		await expect(openBtn(page)).toHaveCount(0);
		await expect(collapseBtn(page)).toBeFocused();
		await expect(page.locator('header [data-nav-toggle]')).toHaveCount(0);
	});

	test('5. 開いている間にコマンドパレットを閉じても、フォーカスは見えているトグルへ戻る', async () => {
		await page.keyboard.press('Control+k');
		await expect(page.getByRole('dialog')).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(page.getByRole('dialog')).toBeHidden();
		await expect(collapseBtn(page)).toBeFocused();
	});

	test('6. 狭幅へ変えると、ヘッダーの「メニューを開く」だけが出て、押すとオフキャンバスが開く', async () => {
		await page.setViewportSize({ width: 400, height: 800 });
		await expect(collapseBtn(page)).toBeHidden();
		const menuBtn = page.locator('header').getByRole('button', { name: 'メニューを開く' });
		await expect(menuBtn).toBeVisible();
		await expect(menuBtn).toHaveAttribute('aria-controls', 'app-sidebar');
		await menuBtn.click();
		await expect(page.getByRole('link', { name: 'タグ登録' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();
		// ヘッダーの ☰ はバックドロップの下なので、背景の右側をクリックして閉じる。
		await page
			.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
			.click({ position: { x: 390, y: 400 } });
		await expect(page.getByRole('link', { name: 'タグ登録' })).toBeHidden();
		await expect(menuBtn).toBeVisible();
	});

	test('7. 狭幅でヘッダーの ☰ にフォーカスがある状態から広幅へ戻すと、サイドバー側の ☰ へフォーカスが移る（#909 レビュー）', async () => {
		const menuBtn = page.locator('header').getByRole('button', { name: 'メニューを開く' });
		await menuBtn.focus();
		await expect(menuBtn).toBeFocused();
		await page.setViewportSize({ width: 1280, height: 800 });
		await expect(page.locator('header [data-nav-toggle]')).toHaveCount(0);
		await expect(collapseBtn(page)).toBeFocused();
	});

	test('8. 広幅でサイドバー側の ☰ にフォーカスがある状態から狭幅にすると、ヘッダーの ☰ へフォーカスが移る', async () => {
		await collapseBtn(page).focus();
		await expect(collapseBtn(page)).toBeFocused();
		await page.setViewportSize({ width: 400, height: 800 });
		await expect(
			page.locator('header').getByRole('button', { name: 'メニューを開く' })
		).toBeFocused();
	});
});
