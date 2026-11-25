/**
 * banto-hub の Playwright/DOM smoke（T18-1、
 * docs/hub/banto-hub-desktop-plan.md §16.3「banto-hub の Playwright/DOM テスト
 * 基盤を T18-1 の成果物へ前倒し」）。
 *
 * `e2e/tests/smoke.spec.ts`（ChronoGazer）と同じ設計: 実際の
 * `banto-hub`（`banto-hub-host`、`embed-ui` feature）バイナリを LAN/REST
 * モードで起動し、新規 SQLite DB に `banto-hub create-admin` で作った管理者で
 * ログインできることを含めて確認する（banto v8 で初回セットアップ画面は無い）。scenario 群は1つの `page` を
 * `test.describe.serial` + config 全体の `workers: 1` で共有し、直前の
 * scenario が作った状態（ログイン等）に次の scenario が乗る -
 * 人が実際に一度だけ触る操作列を模している。
 *
 * 管理者アカウント（`banto-hub-auth.ts` の
 * `HUB_ADMIN_USERNAME`/`HUB_ADMIN_PASSWORD`）は、同じ `webServer`
 * インスタンスを共有する他の spec も認証に使う
 * - 詳細は `banto-hub-auth.ts` の doc comment 参照。
 */
import { expect, test, type Page } from '@playwright/test';
import { HUB_ADMIN_PASSWORD, HUB_ADMIN_USERNAME } from './banto-hub-auth';

/** ナビのリンク名（メニューバーは名前だけ、サイドバーは前にアイコンが付く）。 */
const NAV_TAGS = /^(?:\S+\s+)?タグ$/;

test.describe.serial('banto-hub smoke', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. the administrator made by create-admin logs in and lands on /status', async () => {
		await page.goto('/login');

		// banto v8（ADR-0034 §5/§7）: 初回セットアップの画面は無い。管理者は起動の前に
		// `banto-hub create-admin` が作ってあり（ロックダウン済み）、最初からログイン
		// フォームが出る（表示名・パスワード確認の欄は無い）。
		await expect(page.getByRole('heading', { name: 'banto-hub' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'アカウントを作成' })).toHaveCount(0);
		await expect(page.getByLabel('表示名')).toHaveCount(0);
		await expect(page.getByLabel('パスワード（確認）')).toHaveCount(0);

		await page.getByLabel('ユーザー名').fill(HUB_ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(HUB_ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();

		// login/+page.svelte は成功後 /status へ goto する（banto-hub は relay-wright と違い、
		// 常に /status がログイン後の着地点 - navigation.ts の doc comment 参照）。
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
		await page.getByRole('link', { name: NAV_TAGS }).click();
		await expect(page).toHaveURL(/\/tags$/);
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
	});
});
