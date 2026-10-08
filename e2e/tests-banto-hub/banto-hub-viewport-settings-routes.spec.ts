/**
 * #359 段階2（設定画面のカテゴリ別ルート化）の実 DOM 固定。
 *
 * ファイル名について: このファイルはもう `ensureLoggedIn` を使わないが、
 * `banto-hub-viewport-offcanvas.spec.ts` と同じ `viewport-` prefix のまま
 * 残している（レール/タブの viewport 切替を検証する spec としての名前）。
 *
 * **ログインしない（banto v3.0.0、ADR-0017）**: このメイン E2E サーバー（port
 * 8805、`lock_down()` を一度も呼ばない）は未ロックダウン＝試運転モードで、
 * loopback から繋ぐので `GET /api/auth/status` の `grants.commissioning` が
 * true になる。この spec はトークンを注入せず（`ensureLoggedIn`/
 * `injectAuthToken` を呼ばず）に保護画面へ `page.goto` し、`(app)/+layout.ts`
 * のガードの `grantFallback`（`POST /api/auth/grant/commissioning`）で試運転の
 * grant に入る（E2E で grant の経路を 1 本通すため。`security` カテゴリの可視は
 * セッションの種別ではなくサーバーの `GET /api/commissioning/status` で決まるので、
 * アカウントのトークンを注入しても 5 カテゴリは揃う -
 * `banto-hub-lockdown-by-account.spec.ts` がそちらを固定する）。
 * ロックダウン済みサーバー（`chromium-locked-down` プロジェクト、port 8802）は
 * 当初 `banto-hub-status-pending-apply-cancel.spec.ts` 専用に `testMatch` で
 * 固定されていたため、「非可視カテゴリへの直接 URL は先頭の可視カテゴリへ
 * 弾かれる」ケース（`guardCategory`）はこの spec（メインサーバー）からは
 * 検証できなかった（`categories.ts` の `guardCategory` 自体は appearance/
 * account 側の `+page.ts` が「常に可視でも呼ぶ」形で経路自体は毎回通っている）。
 * この回帰ケースは PR #371 の Copilot レビュー是正で
 * `banto-hub-settings-guard.spec.ts` を新設し、`chromium-locked-down`
 * プロジェクトの `testMatch` にそちらも加える形で固定した（ロックダウン後は
 * サーバーの状態が `lockedDown: true` になり `security` が非可視になるため）。
 */
import { expect, test, type Page } from '@playwright/test';

const WIDE_VIEWPORT = { width: 1280, height: 900 };
const NARROW_VIEWPORT = { width: 700, height: 900 };

test.describe.serial('banto-hub 設定画面のカテゴリ別ルート', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		// ログインしない: 最初の保護画面への `page.goto` で、ガードが試運転の
		// grant に入る（上の doc comment）。
		page = await browser.newPage({ viewport: WIDE_VIEWPORT });
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

		// このメインサーバーは lock_down() されない（サーバーが試運転モード）ので、
		// `セキュリティ` カテゴリもナビに出る（上の doc comment 参照）。
		await page.getByRole('link', { name: 'セキュリティ' }).click();
		await expect(page).toHaveURL(/\/settings\/security$/);
		await expect(
			page.getByRole('heading', { level: 2, name: '試運転モードのロックダウン' })
		).toBeVisible();

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
