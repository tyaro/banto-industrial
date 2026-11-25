/**
 * 試運転モードの Hub で、アカウントを作ってからロックダウンし、そのアカウントで
 * ログインし直す流れの固定（banto v8 追従で書き直し。元は banto v3.0.0 追従の監査で出た
 * 後退の固定、2026-10-04 オーナー指示）。`chromium-commissioning` プロジェクト専用。
 *
 * banto v8 では初回セットアップ（`POST /api/auth/setup`）が無く、最初の管理者は
 * `banto-hub create-admin` が作る（作ると同時にロックダウンする）。ほかの E2E の Hub は
 * どれも create-admin 済みでロックダウン済みなので、この spec は **create-admin を通さず
 * に起動した 3 台目**（試運転モード・loopback だけ・ログインなしの admin の grant）で走る。
 *
 * 守りたいこと:
 * - 試運転モードでは、ログインなし（試運転の grant）で管理画面に入れ、`/users` から
 *   管理者のアカウントを作れる。
 * - 設定の「セキュリティ」カテゴリのロックダウンを実行すると、サーバーは試運転の grant
 *   を失効させる。grant のセッションだったブラウザは `/login` へ行く
 *   （`commissioningLockDown.ts` の `lockDownAndLeave`）。
 * - いま作ったアカウントでログインでき、admin として `/users` を開ける。
 * - ロックダウンしたあとは試運転の口が閉じている（`/api/commissioning/status` が
 *   `lockedDown: true`、設定の「セキュリティ」カテゴリも出ない）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS } from './banto-hub-auth';

// `admin` は組み込みの管理者が使う予約名（banto v8）なので別の名前にする。
const USERNAME = 'lockadmin';
const PASSWORD = 'LockAdminPass1';
const DISPLAY_NAME = 'ロックダウン管理者';

test.describe
	.serial('banto-hub 試運転モードでアカウントを作ってロックダウンする（試運転モード専用サーバー）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. ログインなしの試運転の grant で、/users から管理者のアカウントを作れる', async () => {
		const status = await page.request.get('/api/commissioning/status', { headers: CSRF_HEADERS });
		expect(((await status.json()) as { lockedDown?: boolean }).lockedDown).toBe(false);

		await page.goto('/users');
		await expect(page).toHaveURL(/\/users$/);
		await expect(page.getByRole('heading', { level: 2, name: 'ユーザー管理' })).toBeVisible();

		const form = page.locator('section.create');
		await form.getByLabel('ユーザー名').fill(USERNAME);
		await form.getByLabel('パスワード（8文字以上）').fill(PASSWORD);
		await form.getByLabel('表示名').fill(DISPLAY_NAME);
		await form.getByLabel('ロール').selectOption('admin');
		await form.getByRole('button', { name: '作成' }).click();

		await expect(page.getByText('作成しました')).toBeVisible();
		await expect(page.getByText(USERNAME, { exact: true })).toBeVisible();
	});

	test('2. 設定のセキュリティからロックダウンすると、試運転の grant が失効してログイン画面へ行く', async () => {
		await page.goto('/settings/security');
		await expect(page).toHaveURL(/\/settings\/security$/);
		await expect(
			page.getByRole('heading', { level: 2, name: '試運転モードのロックダウン' })
		).toBeVisible();

		page.once('dialog', (dialog) => void dialog.accept());
		const button = page.getByRole('button', { name: 'ロックダウンを実行' });
		await expect(button).toBeEnabled();
		await button.click();

		await expect(page).toHaveURL(/\/login$/);

		const after = await page.request.get('/api/commissioning/status', { headers: CSRF_HEADERS });
		expect(((await after.json()) as { lockedDown?: boolean }).lockedDown).toBe(true);
	});

	test('3. 作ったアカウントでログインでき、admin として /users を開ける', async () => {
		await page.getByLabel('ユーザー名').fill(USERNAME);
		await page.getByLabel('パスワード').fill(PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/status$/);

		await page.goto('/users');
		await expect(page).toHaveURL(/\/users$/);
		await expect(page.getByRole('heading', { level: 2, name: 'ユーザー管理' })).toBeVisible();

		// ロックダウンしたので、設定の「セキュリティ」カテゴリ（試運転中だけ出る）は無い。
		await page.goto('/settings');
		await expect(page.getByRole('link', { name: 'セキュリティ' })).toHaveCount(0);
	});
});
