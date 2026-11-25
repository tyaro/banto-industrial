/**
 * 試運転中に**アカウントでログインした admin** がロックダウンしても、そのアカウントの
 * まま続く、の固定（banto v3.0.0 追従の監査で出た後退の固定、2026-10-04 オーナー指示。
 * banto v8 追従で、create-admin を通さない試運転モード専用の Hub 上へ組み直した）。
 * `chromium-commissioning-account` プロジェクト専用（1 台を 1 本の spec が使い切る）。
 *
 * ロックダウンは 1 つの Hub につき一方向なので、ログインなしの grant 側の流れ
 * （`banto-hub-lockdown-by-grant.spec.ts`）とは別の Hub で走らせる。
 *
 * 守りたいこと:
 * - 試運転中はアカウントの admin にも、設定の「セキュリティ」カテゴリ（ロックダウンの欄）
 *   が見える（可視判定はセッションの種別ではなくサーバーの
 *   `GET /api/commissioning/status`、`commissioningState.svelte.ts`）。
 * - ロックダウンを実行すると、サーバーは試運転の grant を失効させるが本人のアカウントの
 *   トークンは有効なまま → 画面は `lockDownAndLeave` の `'stayed'`（「ログイン中の
 *   アカウントで続けます」）で、ログイン画面へは行かず、そのアカウントで `/users` も開ける。
 * - grant 側のセッション（別のブラウザコンテキスト）は失効して、次の遷移で `/login` へ行く。
 */
import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import { CSRF_HEADERS } from './banto-hub-auth';

// `admin` は組み込みの管理者が使う予約名（banto v8）なので別の名前にする。
const USERNAME = 'acctadmin';
const PASSWORD = 'AcctAdminPass1';
const DISPLAY_NAME = 'アカウント管理者';

test.describe
	.serial('banto-hub アカウントでログインしたままのロックダウン（試運転モード専用サーバー）', () => {
	let grantContext: BrowserContext;
	let grantPage: Page;
	let accountContext: BrowserContext;
	let accountPage: Page;

	test.beforeAll(async ({ browser }) => {
		// 1 つ目: ログインなしの試運転の grant（アカウントを作る役）。
		grantContext = await browser.newContext();
		grantPage = await grantContext.newPage();
		// 2 つ目: 作ったアカウントでログインする別のコンテキスト（セッションを混ぜない）。
		accountContext = await browser.newContext();
		accountPage = await accountContext.newPage();
	});

	test.afterAll(async () => {
		await grantContext.close();
		await accountContext.close();
	});

	test('1. 試運転の grant で、管理者のアカウントを作る', async () => {
		const status = await grantPage.request.get('/api/commissioning/status', {
			headers: CSRF_HEADERS
		});
		expect(((await status.json()) as { lockedDown?: boolean }).lockedDown).toBe(false);

		await grantPage.goto('/users');
		await expect(grantPage.getByRole('heading', { level: 2, name: 'ユーザー管理' })).toBeVisible();

		const form = grantPage.locator('section.create');
		await form.getByLabel('ユーザー名').fill(USERNAME);
		await form.getByLabel('パスワード（8文字以上）').fill(PASSWORD);
		await form.getByLabel('表示名').fill(DISPLAY_NAME);
		await form.getByLabel('ロール').selectOption('admin');
		await form.getByRole('button', { name: '作成' }).click();

		await expect(grantPage.getByText('作成しました')).toBeVisible();
	});

	test('2. 試運転中はアカウントの admin にもロックダウンの欄が見え、実行するとそのアカウントのまま続く', async () => {
		await accountPage.goto('/login');
		await accountPage.getByLabel('ユーザー名').fill(USERNAME);
		await accountPage.getByLabel('パスワード').fill(PASSWORD);
		await accountPage.getByRole('button', { name: 'ログイン' }).click();
		await expect(accountPage).not.toHaveURL(/\/login$/);

		// アカウントのセッション（`kind: 'account'`）で設定へ。セキュリティカテゴリは
		// サーバーが試運転中なので出る。
		await accountPage.goto('/settings/security');
		await expect(accountPage).toHaveURL(/\/settings\/security$/);
		await expect(
			accountPage.getByRole('heading', { level: 2, name: '試運転モードのロックダウン' })
		).toBeVisible();
		await expect(accountPage.getByRole('link', { name: 'セキュリティ' })).toBeVisible();
		const identity = await accountPage.evaluate(async () => {
			const token = window.sessionStorage.getItem('banto.auth.token');
			const res = await fetch('/api/auth/identity', {
				headers: { 'X-Banto-Client': 'banto', Authorization: `Bearer ${token}` }
			});
			return (await res.json()) as { kind?: string };
		});
		expect(identity.kind, 'アカウントのセッションであること').toBe('account');

		accountPage.once('dialog', (dialog) => void dialog.accept());
		const button = accountPage.getByRole('button', { name: 'ロックダウンを実行' });
		await expect(button).toBeEnabled();
		await button.click();

		// 'stayed': ログイン画面へは行かず、そのアカウントで続ける。
		await expect(accountPage.getByText('ログイン中のアカウントで続けます')).toBeVisible();
		await expect(accountPage).not.toHaveURL(/\/login$/);
		// セキュリティカテゴリは消え、security のページは先頭の可視カテゴリへ弾かれる。
		await expect(accountPage).toHaveURL(/\/settings\/appearance$/);
		await expect(accountPage.getByRole('link', { name: 'セキュリティ' })).toHaveCount(0);

		const after = await accountPage.request.get('/api/commissioning/status', {
			headers: CSRF_HEADERS
		});
		expect(((await after.json()) as { lockedDown?: boolean }).lockedDown).toBe(true);

		// アカウントのトークンは生きている（ロックダウンが失効させるのは試運転の grant だけ）。
		await accountPage.goto('/users');
		await expect(accountPage).toHaveURL(/\/users$/);
		await expect(
			accountPage.getByRole('heading', { level: 2, name: 'ユーザー管理' })
		).toBeVisible();
	});

	test('3. grant 側のブラウザは失効していて、次の遷移でログイン画面へ行く', async () => {
		await grantPage.goto('/users');
		await expect(grantPage).toHaveURL(/\/login$/);
	});
});
