/**
 * アカウントでログインしたままのロックダウン（banto v3.0.0 追従の監査で出た後退の
 * 固定、2026-10-04 オーナー指示）。`chromium-locked-down` プロジェクト専用で、
 * **このプロジェクトの spec の中で辞書順の先頭**（`lockdown-` < `settings-guard`）
 * なので、ロックダウン済み専用サーバーがまだ試運転モードのうちに走る（他の spec は
 * `beforeAll` で `POST /api/commissioning/lock-down` を叩き、冪等なので、ここで
 * ロックダウン済みになっていても壊れない）。
 *
 * 守りたいこと:
 * - 初回セットアップで作った admin は、そのままアカウントのセッション
 *   （`kind: 'account'`）になる。試運転中はそのアカウントでも設定の「セキュリティ」
 *   カテゴリ（ロックダウンの欄）が見える（可視判定はセッションの種別ではなく
 *   サーバーの `GET /api/commissioning/status`、`commissioningState.svelte.ts`）。
 * - ロックダウンを実行すると、サーバーは試運転の grant を失効させるが本人の
 *   アカウントのトークンは有効なまま → 画面は `lockDownAndLeave` の `'stayed'`
 *   （「ログイン中のアカウントで続けます」）で、ログイン画面へは行かず、
 *   「セキュリティ」カテゴリが消え、先頭の可視カテゴリへ移る。
 * - grant のセッションでロックダウンした場合（→ /login）は単体テスト
 *   `commissioningLockDown.test.ts` が固定する。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

test.describe
	.serial('banto-hub アカウントでログインしたままのロックダウン（ロックダウン済み専用サーバー）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 試運転中はアカウントの admin にもロックダウンの欄が見え、実行するとそのアカウントのまま続く', async () => {
		const before = await page.request.get('/api/commissioning/status', { headers: CSRF_HEADERS });
		const beforeBody = (await before.json()) as { lockedDown?: boolean };
		test.skip(
			beforeBody.lockedDown === true,
			'このサーバーは既にロックダウン済み（spec の実行順が変わった）。この spec は試運転中にだけ意味がある'
		);

		// アカウントのセッション（`kind: 'account'`）で設定へ。セキュリティ
		// カテゴリはサーバーが試運転中なので出る。
		await page.goto('/settings/security');
		await expect(page).toHaveURL(/\/settings\/security$/);
		await expect(
			page.getByRole('heading', { level: 2, name: '試運転モードのロックダウン' })
		).toBeVisible();
		await expect(page.getByRole('link', { name: 'セキュリティ' })).toBeVisible();
		const identity = await page.evaluate(async () => {
			const token = window.sessionStorage.getItem('banto.auth.token');
			const res = await fetch('/api/auth/identity', {
				headers: { 'X-Banto-Client': 'banto', Authorization: `Bearer ${token}` }
			});
			return (await res.json()) as { kind?: string };
		});
		expect(identity.kind, 'アカウントのセッションであること').toBe('account');

		page.once('dialog', (dialog) => void dialog.accept());
		const button = page.getByRole('button', { name: 'ロックダウンを実行' });
		await expect(button).toBeEnabled();
		await button.click();

		// 'stayed': ログイン画面へは行かず、そのアカウントで続ける。
		await expect(page.getByText('ログイン中のアカウントで続けます')).toBeVisible();
		await expect(page).not.toHaveURL(/\/login$/);
		// セキュリティカテゴリは消え、security のページは先頭の可視カテゴリへ弾かれる。
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('link', { name: 'セキュリティ' })).toHaveCount(0);

		const after = await page.request.get('/api/commissioning/status', { headers: CSRF_HEADERS });
		expect(((await after.json()) as { lockedDown?: boolean }).lockedDown).toBe(true);

		// アカウントのトークンは生きている（ロックダウンが失効させるのは試運転の grant だけ）。
		await page.goto('/users');
		await expect(page).toHaveURL(/\/users$/);
	});
});
