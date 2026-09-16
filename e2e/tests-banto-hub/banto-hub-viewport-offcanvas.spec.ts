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
 * オフキャンバスは `position: fixed` + `transform: translateX(-100%)` で画面外へ
 * 退避させる実装（Sidebar.svelte）。**当初は `transform` だけだった**ので
 * `toBeVisible()` では「画面外に置かれているだけで DOM 上は可視」な状態を検出
 * できず、bounding box の x 座標で判定していた。
 *
 * **2026-09-16（#381 レビュー対応12回目）**: 閉じたオフキャンバスに
 * `visibility: hidden` + `inert` を足した（`escLayering.ts` の層の約束・項目6 -
 * `transform` だけだと画面外のナビ項目がフォーカスを受けられてしまい、
 * 閉じたあとの「フォーカスの戻し先」判定にも生きた要素として残るため）。
 * これで「退避している」は Playwright から見て素直に **hidden** になったので、
 * 判定を `toBeHidden()`/`toBeVisible()` に改めた（bounding box の x 座標による
 * 判定は不要になった - `transform` のスライド中も `visibility` は最後まで
 * `visible` のままなので、`toBeHidden()` はアニメーション完了を待つ形になる）。
 */
import { expect, test, type Page } from '@playwright/test';
import { ensureLoggedIn } from './banto-hub-auth';

const NARROW_VIEWPORT = { width: 400, height: 800 };

/**
 * 退避している（＝ユーザーからも支援技術からも「無い」）こと。閉じたオフキャンバスは
 * `visibility: hidden` + `inert` なので、locator は hidden になる。閉じる
 * アニメーション（`transition: transform/visibility 0.2s`）の完了は
 * `toBeHidden()` のリトライが待つ。
 */
async function expectNavHidden(page: Page, locatorName: string): Promise<void> {
	await expect(page.getByRole('link', { name: locatorName })).toBeHidden();
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
		await expectNavHidden(page, 'タグ登録');
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
		await expectNavHidden(page, 'タグ登録');
	});

	test('4. バックドロップのクリックでもオフキャンバスが閉じる', async () => {
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(
			page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
		).toBeVisible();

		// バックドロップ（`button.nav-backdrop`、position:fixed;inset:0 で全画面
		// 400x800）はデフォルトの中心座標（≈200,400）だと、スライドイン
		// transition 完了後の `aside.offcanvas`（幅≈340px、左端固定、z-index
		// がバックドロップより上）に覆われて intercept される - 実機で常に
		// 緑だったのは transition 中の一瞬だけクリックできていたタイミング
		// 依存の偶然で、マシンが遅い/CPU 負荷が高いと aside が定位置に達して
		// 確実に落ちる（2026-09-04 実測回帰）。aside の右側、確実にバック
		// ドロップだけが見えている座標を明示してクリックする（ビューポート幅
		// に依存するため x は NARROW_VIEWPORT から導出）。
		await page
			.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
			.click({ position: { x: NARROW_VIEWPORT.width - 10, y: NARROW_VIEWPORT.height / 2 } });

		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expectNavHidden(page, 'タグ登録');
	});

	test('5. Escape キーでもオフキャンバスが閉じる', async () => {
		await page.getByRole('button', { name: 'メニューを開く' }).click();
		await expect(
			page.getByRole('button', { name: '背景をクリックしてメニューを閉じる' })
		).toBeVisible();

		await page.keyboard.press('Escape');

		await expect(page.getByRole('button', { name: 'メニューを開く' })).toBeVisible();
		await expectNavHidden(page, 'タグ登録');
	});
});
