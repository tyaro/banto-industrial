/**
 * ChronoGazer public-viewer (閲覧公開) E2E (#507; mirrors upstream banto's
 * e2e/tests-public-viewer/public-viewer.spec.ts, ADR-0012 / I2b #499).
 *
 * Runs against the `banto-serve` that `public-viewer.playwright.config.ts`
 * starts with `BANTO_VIEWER_PUBLIC=1` (own port 8804, own fresh DB). One
 * shared page in file order: scenarios 1-3 run against a DB with ZERO users;
 * scenario 4 creates the first admin and later state builds on it.
 *
 * What it proves end to end on the real REST path:
 *   1. without logging in, the allowed screens (監視・ヒストリカル・イベント)
 *      open as the synthetic viewer, with no redirect loop (the guard in
 *      `apps/chronogazer/src/routes/(app)/+layout.ts`; SvelteKit 3's
 *      `resolve('')` trap, #506);
 *   2. the other screens (タグ設定・ユーザー管理・監査ログ・設定) are not
 *      opened - the guard sends them to the first allowed screen (監視). They
 *      are not redirected to /login: the viewer session is already confirmed,
 *      and only an unconfirmed session goes to /login;
 *   3. the sidebar lists only the allowed items;
 *   4. "ログイン" leads to the setup/login screen and signing in switches to a
 *      normal session (full nav, ログアウト instead of ログイン, and the
 *      restricted screens open).
 *
 * No `waitForTimeout`/`sleep`: every wait is a locator auto-retry.
 */
import { expect, test, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'pv-admin';
const ADMIN_PASSWORD = 'E2ePvAdminPass1';
const ADMIN_DISPLAY_NAME = 'E2E閲覧公開管理者';

const ALLOWED = [
	{ path: '/monitor', label: '監視' },
	{ path: '/historical', label: 'ヒストリカル' },
	{ path: '/events', label: 'イベント' }
] as const;

const RESTRICTED = [
	{ path: '/tags', label: 'タグ設定' },
	{ path: '/users', label: 'ユーザー管理' },
	{ path: '/audit-log', label: '監査ログ' },
	{ path: '/settings', label: '設定' }
] as const;

/** The header's "ログイン" button that replaces "ログアウト" for the synthetic viewer. */
function loginButton(page: Page) {
	return page.getByRole('banner').getByRole('button', { name: 'ログイン' });
}

/** A sidebar link. Its accessible name is "<icon> <label>", and `設定` is a suffix of `タグ設定` - match the whole name. */
function navLink(page: Page, label: string) {
	return page
		.getByRole('navigation')
		.getByRole('link', { name: new RegExp(String.raw`^\S+ ${label}$`) });
}

test.describe.serial('ChronoGazer viewer-public mode', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page?.close();
	});

	test('1. the allowed screens open without logging in, with no redirect loop', async () => {
		for (const { path, label } of ALLOWED) {
			await page.goto(path);
			// A loop would never settle on the URL (the guard bounces every
			// path to the first entry); `toHaveURL` fails on the first miss.
			await expect(page).toHaveURL(new RegExp(`${path}$`));
			await expect(page.getByRole('heading', { level: 2, name: label })).toBeVisible();
			// Synthetic session: ログイン instead of ログアウト.
			await expect(loginButton(page)).toBeVisible();
			await expect(page.getByRole('button', { name: 'ログアウト' })).toHaveCount(0);
		}
		// The in-app path (sidebar click) is allowed too.
		await navLink(page, '監視').click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test('2. the restricted screens are not opened: the guard sends them to 監視', async () => {
		for (const { path } of RESTRICTED) {
			await page.goto(path);
			await expect(page).toHaveURL(/\/monitor$/);
			await expect(page.getByRole('heading', { level: 2, name: '監視' })).toBeVisible();
			await expect(loginButton(page)).toBeVisible();
		}
	});

	test('3. the sidebar shows only the allowed items', async () => {
		await page.goto('/monitor');
		for (const { label } of ALLOWED) {
			await expect(navLink(page, label)).toBeVisible();
		}
		for (const { label } of RESTRICTED) {
			await expect(navLink(page, label)).toHaveCount(0);
		}
		await expect(page.getByRole('navigation').getByRole('link')).toHaveCount(ALLOWED.length);
	});

	test('4. "ログイン" leads to the setup screen; signing in switches to a normal session', async () => {
		await loginButton(page).click();
		await expect(page).toHaveURL(/\/login$/);

		// Zero users -> setup form (same as smoke scenario 1). Playwright's CI
		// retry re-runs this whole serial group against the SAME server and DB
		// (the admin already exists after a first attempt got this far), so
		// the screen is a login form then: accept either, with the same
		// credentials. Scenarios 1-3 are unaffected by an existing account -
		// an anonymous visit is still the synthetic viewer while the flag is on.
		const displayName = page.getByLabel('表示名');
		const usernameField = page.getByLabel('ユーザー名');
		// Both forms have the username field; 表示名 exists only in the setup form.
		await expect(usernameField).toBeVisible();
		if (await displayName.isVisible()) {
			await displayName.fill(ADMIN_DISPLAY_NAME);
			await usernameField.fill(ADMIN_USERNAME);
			await page.getByLabel('パスワード（8文字以上）').fill(ADMIN_PASSWORD);
			await page.getByLabel('パスワード（確認）').fill(ADMIN_PASSWORD);
			await page.getByRole('button', { name: 'アカウントを作成' }).click();
		} else {
			await usernameField.fill(ADMIN_USERNAME);
			await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
			await page.getByRole('button', { name: 'ログイン', exact: true }).click();
		}

		await expect(page).toHaveURL(/\/monitor$/);
		await expect(page.getByRole('button', { name: 'ログアウト' })).toBeVisible();
		await expect(loginButton(page)).toHaveCount(0);
		// Full nav: every allowed and restricted item.
		for (const { label } of [...ALLOWED, ...RESTRICTED]) {
			await expect(navLink(page, label)).toBeVisible();
		}
		// A restricted screen now opens instead of bouncing, and survives a reload
		// (the restored session is the account, not a fresh viewer session).
		await page.goto('/users');
		await expect(page).toHaveURL(/\/users$/);
		await page.reload();
		await expect(page).toHaveURL(/\/users$/);
		await expect(page.getByRole('button', { name: 'ログアウト' })).toBeVisible();
	});
});
