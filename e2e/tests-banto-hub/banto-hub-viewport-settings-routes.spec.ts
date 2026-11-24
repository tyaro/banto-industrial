/**
 * #359 段階2（設定画面のカテゴリ別ルート化）の実 DOM 固定。
 *
 * ファイル名について: `banto-hub-viewport-offcanvas.spec.ts` と同じ `viewport-` prefix の
 * まま残している（レール/タブの viewport 切替を検証する spec としての名前）。
 *
 * **ログインして開く（banto v8 追従）**: 以前はこのメイン E2E サーバーが試運転モード
 * （ロックダウンを一度も呼ばない）で、トークンを注入せずに保護画面へ `page.goto` して
 * ガードの `grantFallback`（`POST /api/auth/grant/commissioning`）で試運転の grant に入って
 * いた。v8 では E2E の Hub は `banto-hub create-admin` で作る管理者を持ち（ロックダウンも
 * 同時に行われる）、どのサーバーも最初からロックダウン済みなので、試運転の grant の経路は
 * E2E では通らない。ここは管理者でログインして開き、`セキュリティ`（ロックダウンの欄）は
 * ロックダウン済みなので**出ない**ことを見る（`guardCategory` の弾きは
 * `banto-hub-settings-guard.spec.ts` がロックダウン済み専用サーバーで固定する）。
 */
import { expect, test, type Page } from '@playwright/test';
import { ensureLoggedIn } from './banto-hub-auth';

const WIDE_VIEWPORT = { width: 1280, height: 900 };
const NARROW_VIEWPORT = { width: 700, height: 900 };

test.describe.serial('banto-hub 設定画面のカテゴリ別ルート', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE_VIEWPORT });
		// オリジンへ一度遷移してから、ログインしたトークンを sessionStorage へ入れる。
		await page.goto('/login');
		await ensureLoggedIn(page);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. /settings を開くと /settings/appearance へ redirect される', async () => {
		await page.goto('/settings');
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});

	test('2. カテゴリナビから各カテゴリへ遷移でき、それぞれの見出しが見える', async () => {
		await page.goto('/settings/appearance');

		await page.getByRole('link', { name: 'アカウント' }).click();
		await expect(page).toHaveURL(/\/settings\/account$/);
		await expect(page.getByRole('heading', { level: 2, name: 'アカウント' })).toBeVisible();

		await page.getByRole('link', { name: '接続' }).click();
		await expect(page).toHaveURL(/\/settings\/connectivity$/);
		await expect(page.getByRole('heading', { level: 2, name: /MQTT 発行/ })).toBeVisible();

		await page.getByRole('link', { name: 'データ' }).click();
		await expect(page).toHaveURL(/\/settings\/data$/);
		await expect(page.getByRole('heading', { level: 2, name: 'データ保持' })).toBeVisible();

		// ロックダウン済み（create-admin）なので、`セキュリティ` カテゴリはナビに出ない
		// （上の doc comment 参照）。
		await expect(page.getByRole('link', { name: 'セキュリティ' })).toHaveCount(0);

		await page.getByRole('link', { name: '外観' }).click();
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});

	test('3. 1280px 幅ではカテゴリナビが左レール（コンテンツの左）に出る', async () => {
		await page.setViewportSize(WIDE_VIEWPORT);
		await page.goto('/settings/appearance');

		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		const content = page.getByRole('heading', { level: 2, name: 'テーマ' });
		const [navBox, contentBox] = await Promise.all([nav.boundingBox(), content.boundingBox()]);
		expect(navBox).not.toBeNull();
		expect(contentBox).not.toBeNull();
		// レール表示: ナビの右端がコンテンツの左端より左（横並び）。
		expect(navBox!.x + navBox!.width).toBeLessThanOrEqual(contentBox!.x);
	});

	test('4. 狭い幅ではカテゴリナビが横タブとしてコンテンツの上に出る', async () => {
		await page.setViewportSize(NARROW_VIEWPORT);
		await page.goto('/settings/appearance');

		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		const content = page.getByRole('heading', { level: 2, name: 'テーマ' });
		const [navBox, contentBox] = await Promise.all([nav.boundingBox(), content.boundingBox()]);
		expect(navBox).not.toBeNull();
		expect(contentBox).not.toBeNull();
		// タブ表示: ナビの下端がコンテンツの上端より上（縦並び）。
		expect(navBox!.y + navBox!.height).toBeLessThanOrEqual(contentBox!.y);

		// 元のビューポートに戻す（このページ内でこれ以上テストは無いが、
		// describe.serial 内の後続 spec への影響を避ける習慣に倣う）。
		await page.setViewportSize(WIDE_VIEWPORT);
	});
});
