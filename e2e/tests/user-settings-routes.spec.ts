/**
 * #359 chronogazer 分（設定画面のカテゴリ別ルート化）の実 DOM 固定。
 *
 * banto-hub（#371）と同様、`/settings` を開くと先頭の可視カテゴリへ
 * redirect されること、カテゴリナビから各カテゴリへ遷移して期待する見出しが
 * 出ることを固定する。chronogazer には banto-hub のようなロックダウン専用
 * E2E プロジェクトが無いが、このメイン E2E サーバー（`banto-serve`、LAN
 * ブラウザ相当 = `isTauri()` は常に false）では `security` カテゴリが
 * `+layout.ts` の `tauri && canManageAuthMode()` ゲートで**常に非可視**に
 * なるため、「非可視カテゴリへの直接 URL は先頭の可視カテゴリへ弾かれる」
 * （`guardCategory`）という回帰ケースがこのサーバーだけで検証できる
 * （banto-hub は逆に、このメインサーバーでは全カテゴリ可視になってしまう
 * ため専用のロックダウン済みサーバーが必要だった - 対照的な事情）。
 *
 * ファイル名について: `smoke.spec.ts` の `test.beforeAll` が最初の1件目
 * として初回セットアップ（管理者アカウント作成）を実 DOM 経由で行う想定
 * （`playwright.config.ts` は `workers: 1`/`fullyParallel: false` で
 * `testDir: './tests'` 配下をファイル名の辞書順に実行する）。このファイルは
 * その後にログインするだけで管理者アカウントを再利用するため、辞書順で
 * `smoke.spec.ts` より後になる名前にする必要がある（`se` < `sm` なので
 * 単純に `settings-routes.spec.ts` にすると smoke より先に走ってしまい、
 * smoke 側の「初回セットアップ画面が出ること」の検証を壊す - banto-hub の
 * `banto-hub-auth.ts` の doc comment と同じ罠）。`user-settings-routes` は
 * `sm` より辞書順で後（`u` > `s`）になるよう選んだだけの名前。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント（同じ
// `banto-serve` プロセス・同じ一時 DB を共有するため、ここでは新規作成せず
// ログインするだけでよい）。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

test.describe.serial('chronogazer 設定画面のカテゴリ別ルート', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. /settings を開くと先頭の可視カテゴリ（外観）へ redirect される', async () => {
		await page.goto('/settings');
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});

	test('2. カテゴリナビには可視カテゴリ（外観/アカウント/接続/データ）だけが並び、security は無い', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await expect(nav.getByRole('link', { name: '外観' })).toBeVisible();
		await expect(nav.getByRole('link', { name: 'アカウント' })).toBeVisible();
		await expect(nav.getByRole('link', { name: '接続' })).toBeVisible();
		await expect(nav.getByRole('link', { name: 'データ' })).toBeVisible();
		// isTauri() が常に false のこの E2E サーバーでは、認証カテゴリの
		// ガード（`tauri && canManageAuthMode()`）が常に false になり
		// security は非可視になる（doc comment参照）。
		await expect(nav.getByRole('link', { name: 'セキュリティ' })).toHaveCount(0);
	});

	test('3. アカウントへ遷移すると期待する見出しが出る', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: 'アカウント' }).click();
		await expect(page).toHaveURL(/\/settings\/account$/);
		await expect(page.getByRole('heading', { level: 2, name: 'アカウント' })).toBeVisible();
	});

	test('4. 接続へ遷移すると期待する見出しが出る', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: '接続' }).click();
		await expect(page).toHaveURL(/\/settings\/connectivity$/);
		await expect(
			page.getByRole('heading', { level: 2, name: 'LANアクセス（組み込みWebサーバ）' })
		).toBeVisible();
	});

	test('5. データへ遷移すると期待する見出しが出る', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: 'データ' }).click();
		await expect(page).toHaveURL(/\/settings\/data$/);
		await expect(
			page.getByRole('heading', { level: 2, name: '監査ログの保持ポリシー' })
		).toBeVisible();
		await expect(
			page.getByRole('heading', { level: 2, name: 'バックアップ/リストア' })
		).toBeVisible();
	});

	test('6. 外観へ戻ると期待する見出しが出る（ナビの周回）', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: '外観' }).click();
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});

	test('7. 非可視カテゴリ（security）への直接遷移は先頭の可視カテゴリへ弾かれる（guardCategory 回帰固定）', async () => {
		await page.goto('/settings/security');
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});
});
