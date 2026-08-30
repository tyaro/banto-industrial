/**
 * relay-wright の Playwright/DOM smoke（H5 の残作業）。
 *
 * `relay-wright.playwright.config.ts` のファイル冒頭 doc comment のとおり、
 * `relay-wright-serve`（Tauri を使わない組み込みサーバー = spec §11.1の
 * **モード2**）を LAN/REST モードで起動し、新規 SQLite DB に対して以下を
 * 実 DOM 経由で確認する:
 *
 * - 初回セットアップ画面（管理者アカウント作成）→ ログアウト → ログイン
 *   （`e2e/tests-banto-hub/banto-hub-smoke.spec.ts` と同型の1テストケース
 *   ごとに前段の状態へ積み上げていく `test.describe.serial` 構成）。
 * - サイドバー導線で PLC接続画面（`(app)/plc-connections/+page.svelte`、
 *   R1-B）へ遷移し、そこで REST 経由の作成・編集・削除（カスケード削除の
 *   確認ダイアログ含む）が一通り動くこと。
 * - **モード2で動いていることの確認**: PLC接続を1件作成した直後に
 *   ブラウザをフルリロードし、それでもその行が残っていること。
 *   `$lib/banto/setup.ts` のモード3（`InMemoryDataProvider`）はページ読込
 *   毎に空のシードから作り直されるモジュール内メモリでしかないので、
 *   もしモード3で動いていたら作成した行はリロード後に消える。消えずに
 *   残ることが、実際には `HttpDataProvider`（SQLite裏付き
 *   `relay-wright-serve`）に対して読み書きしている証拠になる。
 *
 * Tauri webview 固有の経路（`invoke()` 分岐・`banto://event`・vibrancy 等、
 * spec §11.1のモード1）はこの spec の対象外 - config の doc comment に
 * 書いたとおり WebDriver が要る別課題として H5 のスコープから分離した。
 */
import { expect, test, type Page } from '@playwright/test';

const ADMIN_DISPLAY_NAME = 'E2E管理者';
const ADMIN_USERNAME = 'e2e-relay-wright-admin';
const ADMIN_PASSWORD = 'E2eRelayWrightPass1';

/** テスト4で作成する PLC 接続の初期値／編集後の値。 */
const CONNECTION_NAME = 'E2E生成PLC接続';
const CONNECTION_NAME_EDITED = 'E2E生成PLC接続（編集済み）';
const CONNECTION_HOST = '192.168.11.200';

test.describe.serial('relay-wright smoke (mode 2: embedded server)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. first-run setup creates the admin account and lands on /settings', async () => {
		await page.goto('/login');

		// 新規DB → AuthProvider.status() が initialized: false を返す →
		// login/+page.svelte がセットアップフォームを描画する（ログイン
		// フォームではない）。banto-hub と異なり見出しは
		// `🏮 {APP_NAME}`（$lib/appName.ts の APP_NAME = 'RelayWright'）。
		await expect(page.getByRole('heading', { name: /RelayWright/ })).toBeVisible();
		await expect(page.getByRole('button', { name: 'アカウントを作成' })).toBeVisible();

		await page.getByLabel('表示名').fill(ADMIN_DISPLAY_NAME);
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード（8文字以上）').fill(ADMIN_PASSWORD);
		await page.getByLabel('パスワード（確認）').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'アカウントを作成' }).click();

		// login/+page.svelte's submitSetup() は成功後 /settings へ goto する
		// （relay-wright は banto-hub と違い、常に /settings がログイン後の
		// 着地点 - navigation.ts の doc comment 参照）。
		await expect(page).toHaveURL(/\/settings$/);
		// Header.svelte の <h1>{pageTitle(...)}</h1> がページタイトルとして
		// 「設定」を描く（navigation.ts の navItems[{path:'/settings'}]）。
		await expect(page.getByRole('heading', { level: 1, name: '設定' })).toBeVisible();
	});

	test('2. logout returns to /login, then login restores the /settings session', async () => {
		await page.getByRole('button', { name: 'ログアウト' }).click();
		await expect(page).toHaveURL(/\/login$/);

		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();

		await expect(page).toHaveURL(/\/settings$/);
		await expect(page.getByRole('heading', { level: 1, name: '設定' })).toBeVisible();
	});

	test('3. sidebar navigation to PLC接続 renders the plc-connections screen', async () => {
		await page.getByRole('link', { name: 'PLC接続' }).click();
		await expect(page).toHaveURL(/\/plc-connections$/);
		// Header の <h1>PLC接続</h1>（ページタイトル）と、
		// plc-connections/+page.svelte 自身の <h2>PLC接続</h2>
		// （セクション見出し）は同一テキストなので level で区別する
		// （banto-hub-smoke.spec.ts の「サーバー状態」と同じ理由）。
		await expect(page.getByRole('heading', { level: 2, name: 'PLC接続' })).toBeVisible();
	});

	test('4. create/edit/delete a PLC connection over REST, surviving a full reload (mode 2 confirmation)', async () => {
		const gridWrap = page.locator('.grid-wrap');

		// --- create ---
		// 「新規作成」セクション: 名前・プロトコル(既定 slmp)・ホスト・
		// ポート(既定 5007)・ユニットID(既定 1)・有効(既定 true) のうち、
		// 名前とホスト以外はデフォルト値のまま作成する。
		await page.getByRole('heading', { name: '新規作成' }).waitFor();
		const createSection = page.locator('section.create');
		await createSection.getByLabel('名前').fill(CONNECTION_NAME);
		await createSection.getByLabel('ホスト').fill(CONNECTION_HOST);
		await createSection.getByRole('button', { name: '作成' }).click();

		await expect(gridWrap.getByText(CONNECTION_NAME, { exact: true })).toBeVisible();

		// --- モード2の確認: フルリロード後も残っていること ---
		// InMemoryDataProvider（モード3）ならページ読込のたびに空シードから
		// 作り直されるため、もしモード3で動いていればここで消える。
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'PLC接続' })).toBeVisible();
		await expect(gridWrap.getByText(CONNECTION_NAME, { exact: true })).toBeVisible();

		// --- edit ---
		// 行クリックで下に編集パネルが開く（plc-connections/+page.svelte の
		// selectConnection）。BantoGrid のセルは onpointerdown で
		// `selection.setActive()`（アクティブセルのハイライト）を先に走らせて
		// おり、その再描画がちょうど mousedown〜mouseup の間にセルの実際の
		// 位置をわずかに動かすことがある（調査で確認済み: 素の
		// `locator.click()` だと mousedown 時点では正しくセルに当たっている
		// のに、直後の mouseup 時点で同じ座標の `elementFromPoint` が
		// `.banto-grid` コンテナ自身に変わってしまい、結果として click イベント
		// が mousedown/mouseup 両ターゲットの最近共通祖先である `.banto-grid`
		// に発火してしまい、`onRowClick` が呼ばれず編集パネルが開かない）。
		// 実座標でのポインタ操作ではなくセル要素へ直接 `click` イベントを
		// dispatch することで、この BantoGrid 側の再描画タイミング競合を
		// 迂回する（アプリの行クリック挙動自体を検証したいのであって、生の
		// マウス座標移動の再現性を検証したいわけではないため許容する）。
		await gridWrap.getByText(CONNECTION_NAME, { exact: true }).dispatchEvent('click');
		const detailSection = page.locator('section.detail');
		await expect(
			detailSection.getByRole('heading', { name: `${CONNECTION_NAME} を編集` })
		).toBeVisible();
		await detailSection.getByLabel('名前').fill(CONNECTION_NAME_EDITED);
		await detailSection.getByRole('button', { name: '保存' }).click();

		await expect(gridWrap.getByText(CONNECTION_NAME_EDITED, { exact: true })).toBeVisible();
		await expect(gridWrap.getByText(CONNECTION_NAME, { exact: true })).toHaveCount(0);

		// --- delete ---
		// deleteConnection() はまずカスケードプレビューを REST で取得してから
		// window.confirm() を出す（plc-connections/+page.svelte）。子レコード
		// が無い接続なので巻き添え0件の確認文言になるが、ダイアログ自体は
		// 出るので accept する。
		page.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await page.locator('section.detail').getByRole('button', { name: '削除' }).click();

		await expect(gridWrap.getByText(CONNECTION_NAME_EDITED, { exact: true })).toHaveCount(0);
		await expect(page.locator('section.detail')).toHaveCount(0);
	});
});
