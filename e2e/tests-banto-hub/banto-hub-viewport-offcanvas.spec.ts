/**
 * T19 S3-a（UX-43、docs/banto-hub-t19-design.md §8.2「案C・banto-hub のみ」）:
 * ≤900px オフキャンバスサイドバーの実 DOM 固定。
 *
 * ファイル名について: `banto-hub-auth.ts` の doc comment のとおり、
 * `fetchAuthToken`/`ensureLoggedIn` を使う新規 spec は smoke より辞書順で
 * 後になる名前にする必要がある（smoke の初回セットアップ DOM 検証を壊さない
 * ため）。`banto-hub-viewport-offcanvas` の先頭 `v` は `banto-hub-smoke` の
 * `s` より後なので条件を満たす。
 *
 * オフキャンバスは `position: fixed` + `transform: translateX(-100%)` で
 * 画面外へ退避させる実装（Sidebar.svelte）なので、Playwright の
 * `toBeVisible()` だけでは「画面外に置かれているだけで DOM 上は可視」な
 * 状態を検出できない（bounding box は空でないため）。そのため実際の
 * bounding box の x 座標で「画面外」かどうかを判定する。
 */
import { expect, test, type Page } from '@playwright/test';
import { ensureLoggedIn } from './banto-hub-auth';

const NARROW_VIEWPORT = { width: 400, height: 800 };

/** ビューポート左端より完全に外（右端も含めて左側）にあるかどうか。 */
async function isOffscreenLeft(page: Page, locatorName: string): Promise<boolean> {
	const box = await page.getByRole('link', { name: locatorName }).boundingBox();
	if (!box) return true; // display:none 等で box が取れない場合も「見えていない」扱い
	return box.x + box.width <= 0;
}

/**
 * オフキャンバスを閉じた直後は `transition: transform 0.2s` のスライド中
 * なので、bounding box を1回だけ見ると「まだ画面内」を拾ってしまう
 * ことがある。固定 sleep でアニメーション時間に依存するのではなく、
 * 画面外に収まるまでポーリングして待つ。
 */
async function expectOffscreenLeft(page: Page, locatorName: string): Promise<void> {
	await expect.poll(() => isOffscreenLeft(page, locatorName)).toBe(true);
}

test.describe.serial('banto-hub offcanvas sidebar (narrow viewport)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: NARROW_VIEWPORT });
		await page.goto('/login');
		await ensureLoggedIn(page);
		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'サーバー状態' })).toBeVisible();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 初期状態ではサイドバーのナビが画面外に退避している', async () => {
		expect(await isOffscreenLeft(page, 'タグ登録')).toBe(true);
		// バックドロップも出ていない（開いていないので背景オーバーレイは無い）。
		await expect(
			page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
		).not.toBeVisible();
	});

	test('2. ☰ を押すとオフキャンバスが開いてナビが見える', async () => {
		await page.getByRole('button', { name: 'メニューを開く' }).click();

		// `exact: true` が必要: バックドロップの aria-label
		// 「背景をクリックしてメニューを閉じる」が部分一致してしまうため
		// （strict mode 違反の実体験）。
		await expect(page.getByRole('button', { name: 'メニューを閉じる', exact: true })).toBeVisible();
		await expect(
			page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
		).toBeVisible();

		const tagsLink = page.getByRole('link', { name: 'タグ登録' });
		await expect(tagsLink).toBeVisible();
		await expect
			.poll(async () => {
				const box = await tagsLink.boundingBox();
				return box?.x ?? -1;
			})
			.toBeGreaterThanOrEqual(0);
	});

	test('3. リンクを押すと遷移してオフキャンバスが閉じる', async () => {
		await page.getByRole('link', { name: 'タグ登録' }).click();

		await expect(page).toHaveURL(/\/tags$/);
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();

		// 遷移により afterNavigate 経由でオフキャンバスが閉じ、☰ は「開く」に
		// 戻り、ナビは再び画面外へ退避する。
		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expectOffscreenLeft(page, 'タグ登録');
	});

	test('4. バックドロップのクリックでもオフキャンバスが閉じる', async () => {
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(
			page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
		).toBeVisible();

		await page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' }).click();

		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expectOffscreenLeft(page, 'タグ登録');
	});

	test('5. Escape キーでもオフキャンバスが閉じる', async () => {
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(
			page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
		).toBeVisible();

		await page.keyboard.press('Escape');

		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expectOffscreenLeft(page, 'タグ登録');
	});
});
