/**
 * ChronoGazer R1-A smoke E2E (docs/r1-plan.md R1-A "CI に frontend/E2E
 * ジョブ追加").
 *
 * Runs against a real `banto-serve --features embed-ui` (chronogazer-core's
 * dev binary, LAN/REST mode - see playwright.config.ts's doc comment) with a
 * brand-new SQLite database, so scenario 1 legitimately hits the first-run
 * setup screen. Scenarios share ONE browser page/session and run in file
 * order (`describe.serial` + `workers: 1`, config-wide), the same pattern as
 * banto's own e2e/tests/smoke.spec.ts.
 *
 * Deliberately scoped to what R1-A actually ships: first-run setup / login /
 * logout, and the three nav destinations that exist today (監視/
 * ヒストリカル/イベント), all of which are still placeholders. There is no
 * items/dashboard equivalent in this app (R1-A's nav is 監視・ヒストリカル・
 * イベント, not banto admin-template's items demo), and real content behind
 * 監視/ヒストリカル/イベント lands in R1-B..R1-D - extend this suite as each
 * of those phases replaces a placeholder with real UI.
 */
import { expect, test, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const ADMIN_DISPLAY_NAME = 'E2E管理者';

test.describe.serial('ChronoGazer R1-A smoke', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. first-run setup creates the admin account and lands on the monitor screen', async () => {
		await page.goto('/login');

		// Fresh DB -> AuthProvider.status() reports uninitialized -> the login
		// page renders the setup form, not the login form (login/+page.svelte).
		await expect(page.getByRole('heading', { name: '🏮 ChronoGazer' })).toBeVisible();
		await expect(page.getByLabel('表示名')).toBeVisible();

		await page.getByLabel('表示名').fill(ADMIN_DISPLAY_NAME);
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード（8文字以上）').fill(ADMIN_PASSWORD);
		await page.getByLabel('パスワード（確認）').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'アカウントを作成' }).click();

		// login/+page.svelte's submitSetup() redirects to /monitor (not
		// /dashboard - this app has no dashboard route).
		await expect(page).toHaveURL(/\/monitor$/);
		// `level: 2` (the page body's own <h2>): Header.svelte also renders the
		// current page title as an <h1> with the same text, so a name-only
		// heading locator would match both and trip strict mode.
		await expect(page.getByRole('heading', { level: 2, name: '監視' })).toBeVisible();
		// routes/(app)/monitor/+page.svelte: R1-A ships this screen as a pure
		// empty state until R1-B adds 表示グループ CRUD.
		await expect(page.getByText('表示グループが未設定です')).toBeVisible();
	});

	test('2. logout returns to the login screen, then login restores the session', async () => {
		await page.getByRole('button', { name: 'ログアウト' }).click();
		await expect(page).toHaveURL(/\/login$/);

		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();

		await expect(page).toHaveURL(/\/monitor$/);
	});

	test('3. historical nav renders its R2 placeholder', async () => {
		await page.getByRole('link', { name: 'ヒストリカル' }).click();
		await expect(page).toHaveURL(/\/historical$/);
		await expect(page.getByRole('heading', { level: 2, name: 'ヒストリカル' })).toBeVisible();
		await expect(page.getByText('この画面は R2 フェーズで実装予定です')).toBeVisible();
	});

	test('4. events nav renders its R1-C placeholder', async () => {
		await page.getByRole('link', { name: 'イベント' }).click();
		await expect(page).toHaveURL(/\/events$/);
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		await expect(page.getByText('この画面は R1-C フェーズで実装予定です')).toBeVisible();
	});
});
