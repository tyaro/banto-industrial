/**
 * banto-hub の Playwright/DOM smoke（T18-1、
 * docs/banto-hub-desktop-plan.md §16.3「banto-hub の Playwright/DOM テスト
 * 基盤を T18-1 の成果物へ前倒し」）。
 *
 * `e2e/tests/smoke.spec.ts`（ChronoGazer）と同じ設計: 実際の
 * `banto-hub`（`banto-hub-core`、`embed-ui` feature）バイナリを LAN/REST
 * モードで起動し、新規 SQLite DB に対して初回セットアップ画面が実際に出る
 * ことを含めて確認する。scenario 群は1つの `page` を
 * `test.describe.serial` + config 全体の `workers: 1` で共有し、直前の
 * scenario が作った状態（管理者アカウント等）に次の scenario が乗る -
 * 人が実際に一度だけ触る操作列を模している。
 *
 * ここで作る管理者アカウント（`banto-hub-auth.ts` の
 * `HUB_ADMIN_USERNAME`/`HUB_ADMIN_PASSWORD`）は、同じ `webServer`
 * インスタンスを共有する `banto-hub-tags-continuous.spec.ts` も認証に使う
 * - 詳細は `banto-hub-auth.ts` の doc comment 参照。
 */
import { expect, test, type Page } from '@playwright/test';
import { HUB_ADMIN_DISPLAY_NAME, HUB_ADMIN_PASSWORD, HUB_ADMIN_USERNAME } from './banto-hub-auth';

test.describe.serial('banto-hub smoke', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. first-run setup creates the admin account and lands on /status', async () => {
		await page.goto('/login');

		// 新規DB → AuthProvider.status() が initialized: false を返す →
		// login/+page.svelte がセットアップフォームを描画する（ログイン
		// フォームではない）。
		await expect(page.getByRole('heading', { name: 'banto-hub' })).toBeVisible();
		await expect(page.getByText('初回起動です')).toBeVisible();

		await page.getByLabel('表示名').fill(HUB_ADMIN_DISPLAY_NAME);
		await page.getByLabel('ユーザー名').fill(HUB_ADMIN_USERNAME);
		await page.getByLabel('パスワード（8文字以上）').fill(HUB_ADMIN_PASSWORD);
		await page.getByLabel('パスワード（確認）').fill(HUB_ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'アカウントを作成' }).click();

		// login/+page.svelte's submitSetup() は成功後 /status へ goto する
		// （banto-hub は relay-wright と違い、常に /status がログイン後の
		// 着地点 - navigation.ts の doc comment 参照）。
		await expect(page).toHaveURL(/\/status$/);
		// h2「サーバー状態」(status/+page.svelte 内の section 見出し) -
		// Header.svelte がページタイトルとして描く <h1>状態</h1> とは別物
		// なので、level 指定で strict mode の多重マッチを避ける。
		await expect(page.getByRole('heading', { level: 2, name: 'サーバー状態' })).toBeVisible();
	});

	test('2. logout returns to /login, then login restores the /status session', async () => {
		await page.getByRole('button', { name: 'ログアウト' }).click();
		await expect(page).toHaveURL(/\/login$/);

		await page.getByLabel('ユーザー名').fill(HUB_ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(HUB_ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();

		await expect(page).toHaveURL(/\/status$/);
		await expect(page.getByRole('heading', { level: 2, name: 'サーバー状態' })).toBeVisible();
	});

	test('3. sidebar navigation to タグ登録 renders the tags screen', async () => {
		await page.getByRole('link', { name: 'タグ登録' }).click();
		await expect(page).toHaveURL(/\/tags$/);
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
	});
});
