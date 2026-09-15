/**
 * #359 relay-wright 分（設定画面のカテゴリ別ルート化）の実 DOM 固定。
 *
 * banto-hub（#371）・chronogazer（#372）と同様、`/settings` を開くと先頭の
 * 可視カテゴリへ redirect されること、カテゴリナビから各カテゴリへ遷移して
 * 期待する見出しが出ることを固定する。
 *
 * chronogazer と異なり、relay-wright の `security` カテゴリの可視性は
 * `tauri && canManageAuthMode()` 単体ではなく、タグモニタ手動書き込み・
 * アーム時限失効（H10、いずれも `isAdmin` のみが条件で `tauri` 不問）との
 * OR になっている（`+layout.ts` の doc comment参照）。この E2E サーバー
 * （`relay-wright-serve`、LAN ブラウザ相当 = `isTauri()` は常に false）では
 * 「認証」サブセクションは非可視のままだが、admin セッションでは
 * タグモニタ/アーム時限失効のいずれも可視になるため、`security`
 * カテゴリ自体は5カテゴリすべてと同様にこのサーバーだけで実 DOM 固定できる
 * （chronogazer は逆に、このメインサーバーでは security が丸ごと非可視に
 * なるため「非可視カテゴリへの直接遷移」の回帰をここで拾えたが、
 * relay-wright では admin セッションで再現できないため、その分岐は
 * `categories.test.ts` の単体テストで代える - そちらの doc comment参照）。
 *
 * ファイル名について: `relay-wright-smoke.spec.ts` がこの E2E サーバーの
 * 唯一の管理者アカウントを初回セットアップで作成する（`relay-wright.
 * playwright.config.ts` は `workers: 1`/`fullyParallel: false` で
 * `testDir: './tests-relay-wright'` 配下を `testMatch: 'relay-wright-*.spec.ts'`
 * にマッチするファイル名の辞書順で実行する）。このファイルはその後に
 * ログインするだけで管理者アカウントを再利用するため、辞書順で
 * `relay-wright-smoke.spec.ts` より後になる名前にする必要がある
 * （`relay-wright-se` < `relay-wright-sm` なので単純に
 * `relay-wright-settings-routes.spec.ts` にすると smoke より先に走ってしまい、
 * smoke 側の「初回セットアップ画面が出ること」の検証を壊す -
 * chronogazer の `user-settings-routes.spec.ts` の doc comment と同じ罠）。
 * `relay-wright-user-settings-routes` は辞書順で `relay-wright-smoke` より
 * 後（`u` > `s`）になるよう選んだだけの名前。
 */
import { expect, test, type Page } from '@playwright/test';

// relay-wright-smoke.spec.ts が初回セットアップで作成する唯一の管理者
// アカウント（同じ `relay-wright-serve` プロセス・同じ一時 DB を共有する
// ため、ここでは新規作成せずログインするだけでよい）。
const ADMIN_USERNAME = 'e2e-relay-wright-admin';
const ADMIN_PASSWORD = 'E2eRelayWrightPass1';

test.describe.serial('relay-wright 設定画面のカテゴリ別ルート', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		// submitLogin() の goto('/settings') は #359 で /settings/appearance へ
		// 307 redirect される（relay-wright-smoke.spec.ts のテスト2 と同じ着地点）。
		await expect(page).toHaveURL(/\/settings\/appearance$/);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. /settings を開くと先頭の可視カテゴリ（外観）へ redirect される', async () => {
		await page.goto('/settings');
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});

	test('2. カテゴリナビには5カテゴリすべて（外観/アカウント/接続/データ/セキュリティ）が並ぶ', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await expect(nav.getByRole('link', { name: '外観' })).toBeVisible();
		await expect(nav.getByRole('link', { name: 'アカウント' })).toBeVisible();
		await expect(nav.getByRole('link', { name: '接続' })).toBeVisible();
		await expect(nav.getByRole('link', { name: 'データ' })).toBeVisible();
		// isTauri() が常に false のこの E2E サーバーでは「認証」サブセクション
		// 自体は非可視だが、admin セッションではタグモニタ手動書き込み/アーム
		// 時限失効（いずれも isAdmin のみが条件）が可視になるため、security
		// カテゴリ自体は表示される（doc comment参照）。
		await expect(nav.getByRole('link', { name: 'セキュリティ' })).toBeVisible();
	});

	test('3. アカウントへ遷移すると期待する見出しが出る', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: 'アカウント' }).click();
		await expect(page).toHaveURL(/\/settings\/account$/);
		// isTauri() が false のこのサーバーでは「自動ログイン」は非表示 -
		// 常に表示される「アカウント」（パスワード変更）の見出しで固定する。
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

	test('6. セキュリティへ遷移すると期待する見出しが出る（タグモニタ手動書き込み・アーム時限失効）', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: 'セキュリティ' }).click();
		await expect(page).toHaveURL(/\/settings\/security$/);
		// isTauri() が false のこのサーバーでは「認証」は非表示 - relay-wright
		// 固有の2セクション（H2/H10）の見出しで固定する。
		await expect(
			page.getByRole('heading', { level: 2, name: 'タグモニタ 手動書き込み' })
		).toBeVisible();
		await expect(
			page.getByRole('heading', { level: 2, name: 'アーム時限失効（H10）' })
		).toBeVisible();
	});

	test('7. 外観へ戻ると期待する見出しが出る（ナビの周回）', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: '外観' }).click();
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
	});
});
