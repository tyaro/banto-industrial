/**
 * ChronoGazer R1-A smoke E2E (docs/archive/r1-plan.md R1-A "CI に frontend/E2E
 * ジョブ追加").
 *
 * Runs against a real `banto-serve --features embed-ui` (chronogazer-core's
 * dev binary, LAN/REST mode - see playwright.config.ts's doc comment) with a
 * brand-new SQLite database whose administrator `banto-serve create-admin` made
 * before the server started (scenario 1 logs in as it). Scenarios share ONE browser page/session and run in file
 * order (`describe.serial` + `workers: 1`, config-wide), the same pattern as
 * banto's own e2e/tests/smoke.spec.ts.
 *
 * Deliberately scoped to what R1-A actually ships: first-run setup / login /
 * logout, and the three nav destinations that exist today (監視/
 * ヒストリカル/イベント). 監視 and ヒストリカル are still placeholders;
 * イベント was replaced by the real collect-events list in R1-C's C-3b
 * (#409), and scenario 4 was updated with it. There is no
 * items/dashboard equivalent in this app (R1-A's nav is 監視・ヒストリカル・
 * イベント, not banto admin-template's items demo), and real content behind
 * 監視/ヒストリカル/イベント lands in R1-B..R1-D - extend this suite as each
 * of those phases replaces a placeholder with real UI.
 */
import { expect, test, type Page } from '@playwright/test';
import { E2E_ADMIN } from '../first-admin';

const ADMIN_USERNAME = E2E_ADMIN.username;
const ADMIN_PASSWORD = E2E_ADMIN.password;

test.describe.serial('ChronoGazer R1-A smoke', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. the administrator made by create-admin logs in and lands on the monitor screen', async () => {
		await page.goto('/login');

		// banto v8 (ADR-0034 §5/§7): no first-run setup screen. The admin was made by
		// `banto-serve create-admin` before the server started (playwright.config.ts), so
		// the page is the login form from the first visit: no display name, no
		// password confirmation.
		await expect(page.getByRole('heading', { name: '🏮 ChronoGazer' })).toBeVisible();
		await expect(page.getByLabel('表示名')).toHaveCount(0);
		await expect(page.getByLabel('パスワード（確認）')).toHaveCount(0);

		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン', exact: true }).click();

		// login/+page.svelte redirects to /monitor (not
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

	test('3. historical nav renders the historical screen (R2, #1207)', async () => {
		await page.getByRole('link', { name: 'ヒストリカル' }).click();
		await expect(page).toHaveURL(/\/historical$/);
		await expect(page.getByRole('heading', { level: 2, name: 'ヒストリカル' })).toBeVisible();
		// R2 で画面を実装した。グループが 0 件なので空の表示。
		await expect(page.getByText('表示グループが未設定です')).toBeVisible();
	});

	// R1-C の C-3b でプレースホルダが実装に置き換わった（このファイルの doc
	// comment「extend this suite as each of those phases replaces a placeholder
	// with real UI」のとおり）。ここでは一覧が描かれることだけを見る - 0 件の
	// うちは**「読めなかった」ではなく「まだ1件も記録されていない」**と言い分け
	// られているかを見るのが要点で、収集の状態や操作は
	// `user-settings-collect.spec.ts` が受け持つ。
	test('4. events nav renders the collect-events list (R1-C C-3b)', async () => {
		await page.getByRole('link', { name: 'イベント' }).click();
		await expect(page).toHaveURL(/\/events$/);
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		// 起動時の自動開始は収集対象 0 件で終わっているので、収集エンジンは
		// 1 度も動いておらず `collect_events` は空。
		await expect(page.getByText('イベントはまだ1件も記録されていません。')).toBeVisible();
		// 0 件でも一覧そのものは描かれる（列が消えて「壊れている」ように
		// 見えない）。
		await expect(page.getByRole('columnheader', { name: '種類' })).toBeVisible();
	});
});
