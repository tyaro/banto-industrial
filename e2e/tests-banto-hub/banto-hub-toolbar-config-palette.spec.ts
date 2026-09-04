/**
 * T19 S4（UX-42、docs/banto-hub-t19-design.md §8.1/§7.6）: 構成パッケージの
 * export/import をコマンドパレット（Ctrl+K）に登録した配線を固定する。
 * ロジック自体（`loadConfigPackage`/`serializeConfigPackage`/
 * `applyConfigPackage`）は既存のユニットテストで検証済みなので、ここでは
 * 「パレットから見つかる」「import はディープリンクで設定画面の該当
 * セクションへ誘導する」「export はダウンロードを発火する」という配線
 * だけを見る。
 *
 * ファイル名について: `banto-hub-auth.ts` の doc comment のとおり、
 * `ensureLoggedIn` を使う新規 spec は smoke より辞書順で後になる名前にする
 * 必要がある。`banto-hub-toolbar-config-palette` の先頭 `t` は
 * `banto-hub-smoke` の `s` より後なので条件を満たす（`banto-hub-viewport-
 * offcanvas.spec.ts` と同じ配慮）。
 */
import { expect, test, type Page } from '@playwright/test';
import { ensureLoggedIn } from './banto-hub-auth';

async function openPaletteAndSearch(page: Page, query: string): Promise<void> {
	await page.keyboard.press('Control+k');
	await expect(page.getByRole('dialog', { name: 'コマンドパレット' })).toBeVisible();
	// `getByRole('combobox')` だけだと strict mode violation になる -
	// タグ画面の CSV エクスポート範囲 `<select>` も ARIA 上は combobox として
	// 拾われるため（実測回帰）、パレット入力の accessible name で絞り込む。
	await page.getByRole('combobox', { name: 'コマンドを検索…' }).fill(query);
}

test.describe.serial('banto-hub コマンドパレットの構成パッケージ導線', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		await ensureLoggedIn(page);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 「構成」で検索すると export/import の両コマンドが見つかる（admin セッション）', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();

		await openPaletteAndSearch(page, '構成');

		await expect(page.getByRole('option', { name: '構成パッケージをダウンロード' })).toBeVisible();
		await expect(page.getByRole('option', { name: /^構成パッケージを取り込む/ })).toBeVisible();

		// キーワード検索（英語エイリアス）でも同じ2件が見つかることも確認する。
		await page.getByRole('combobox', { name: 'コマンドを検索…' }).fill('export');
		await expect(page.getByRole('option', { name: '構成パッケージをダウンロード' })).toBeVisible();

		await page.keyboard.press('Escape');
		await expect(page.getByRole('dialog', { name: 'コマンドパレット' })).not.toBeVisible();
	});

	test('2. import コマンドを実行すると /settings へ遷移し、構成セクションが見える', async () => {
		await page.goto('/tags');
		// Ctrl+K は (app)/+layout.svelte のグローバル keydown リスナーで拾う
		// ため、ハイドレーション完了前に押すとイベントが素通りしてパレットが
		// 開かない（実測回帰）。ページ固有の見出しが見えるまで待ってから押す。
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		await openPaletteAndSearch(page, '構成');
		await page.getByRole('option', { name: /^構成パッケージを取り込む/ }).click();

		await expect(page).toHaveURL(/\/settings#config-package$/);
		await expect(
			page.getByRole('heading', { level: 2, name: '構成の保存・読み込み（バックアップ）' })
		).toBeVisible();
	});

	test('3. export コマンドを実行するとダウンロードが発火し、成功トーストが出る', async () => {
		// 直前のテストで既に /settings にいるが、どのページからでも動くことも
		// 兼ねて別ページから開始する。テスト2と同じ理由でハイドレーション
		// 完了を待ってから Ctrl+K を押す。
		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'サーバー状態' })).toBeVisible();
		await openPaletteAndSearch(page, '構成');

		const downloadPromise = page.waitForEvent('download');
		await page.getByRole('option', { name: '構成パッケージをダウンロード' }).click();
		const download = await downloadPromise;

		expect(download.suggestedFilename()).toMatch(/^banto-hub-config-\d{4}-\d{2}-\d{2}\.json$/);
		// `download` イベントはダウンロード開始時点で発火し、実体の書き出しは
		// 非同期に続く。`path()` で完了を待たずに次のテストへ進むと、この
		// 直後に走る `banto-hub-viewport-offcanvas.spec.ts`（アニメーション
		// タイミング依存）がまれに巻き込まれて失敗する実測回帰があったため、
		// ここで完了を待ち切ってからテストを終える。
		await download.path();
		await expect(page.getByText('構成パッケージをダウンロードしました')).toBeVisible();
	});
});
