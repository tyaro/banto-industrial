/**
 * #332 chronogazer 分（Hub 接続の設定カテゴリ）の実 DOM 固定。
 *
 * banto-hub との同時起動はこのリポジトリの E2E に前例が無く、今回は作って
 * いない。ここで固定するのは **Hub 無しで検証できる範囲**だけ:
 * - admin のナビに `Hub接続` が出て、初期状態が「未設定」であること。
 * - 到達不能な URL で「接続」すると **「Hubに到達できません」**という
 *   専用の状態になること（受入条件: エラーを「タグ0件」に潰さない）。
 * - そのとき設定は**保存されない**こと（「切断」が出ず、再読込で「未設定」
 *   に戻る）。
 * - **購読ブロック**（#383 段階1）が未設定でも「停止」を理由付きで出し、
 *   値の表を出さないこと。到達不能な Hub でも例外を出さないこと。
 *   `banto-serve` は OS キーリングを持てないので購読は常に張れない。
 * - 非 admin では `Hub接続` が見えず、`/settings/hub` への直接遷移が先頭の
 *   可視カテゴリへ弾かれること（`guardCategory`）。
 *
 * 「接続済み」「連携が必要」など Hub を実際に必要とする状態は
 * `crates/banto-hub-bootstrap` のモックサーバー付きテスト（27 本）が固定
 * している。
 *
 * ファイル名について: `smoke.spec.ts` が初回セットアップ（管理者アカウント
 * 作成）を実 DOM で行うため、このファイルは辞書順でそれより後でなければ
 * ならない（`playwright.config.ts` は `workers: 1`/`fullyParallel: false`
 * で `testDir` 配下をファイル名の辞書順に実行する。`user-settings-routes
 * .spec.ts` の doc comment にある罠と同じ）。`user-settings-hub` は
 * `smoke` より後、`user-settings-routes` より前に入る（`h` < `r`）。
 * 後続の `user-settings-routes.spec.ts` はここで作る閲覧者アカウントの
 * 有無に依存しない（admin でログインしてナビを見るだけ）。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// このスペックが作る閲覧者アカウント（非 admin の可視性を確かめるため）。
const VIEWER_USERNAME = 'e2e-hub-viewer';
const VIEWER_PASSWORD = 'E2eViewerPass1';

// ポート 1 は待ち受けが無く、接続が即座に拒否される - 「到達不能」を
// タイムアウト待ちなしで再現できる。
const UNREACHABLE_HUB = 'http://127.0.0.1:1';

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer Hub接続の設定カテゴリ', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. admin のカテゴリナビに Hub接続 が並ぶ', async () => {
		await page.goto('/settings');
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await expect(nav.getByRole('link', { name: 'Hub接続' })).toBeVisible();
	});

	test('2. Hub接続へ遷移すると見出しが出て、初期状態は「未設定」', async () => {
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: 'Hub接続' }).click();
		await expect(page).toHaveURL(/\/settings\/hub$/);
		await expect(page.getByRole('heading', { level: 2, name: 'Hub接続' })).toBeVisible();
		await expect(page.getByText('状態: 未設定')).toBeVisible();
		// 未設定のうちは「切断」を出さない（消すものが無い）。
		await expect(page.getByRole('button', { name: '切断' })).toHaveCount(0);
	});

	test('3. 到達不能なURLで「接続」すると「Hubに到達できません」になり、設定は保存されない', async () => {
		await page.getByLabel('接続先URL').fill(UNREACHABLE_HUB);
		await page.getByRole('button', { name: '接続' }).click();

		await expect(page.getByText('状態: Hubに到達できません')).toBeVisible();
		// 失敗した接続は記録を残さないので「切断」は出ない。
		await expect(page.getByRole('button', { name: '切断' })).toHaveCount(0);

		// 再読込すると保存済み設定が無いことがそのまま出る。
		await page.reload();
		await expect(page.getByText('状態: 未設定')).toBeVisible();
	});

	// #383 段階1: 購読ブロック。`banto-serve` は OS キーリングを持てない
	// （`UnavailableKeyStore`）ので購読は決して張れず、常に「停止」＋理由に
	// なる。ここで確かめたいのは「購読が張れなくても画面が壊れず、値の表を
	// 出さない」こと - 購読の失敗が接続設定の 6 状態を汚さないという規律を
	// 実 DOM 側から固定する。
	test('4. 未設定でも購読ブロックは「停止」を理由付きで出し、値の表は出さない', async () => {
		await page.goto('/settings/hub');
		await expect(page.getByText('状態: 未設定')).toBeVisible();
		await expect(page.getByRole('heading', { level: 3, name: '購読' })).toBeVisible();
		await expect(page.getByText('購読: 停止')).toBeVisible();
		await expect(page.getByText('値を受信していません。')).toBeVisible();
		// 値の表は Live のときだけ。停止中に古い値や空の表を出さない。
		await expect(page.getByRole('columnheader', { name: '品質' })).toHaveCount(0);
	});

	test('5. 到達不能なHubへ接続を試みても購読ブロックは例外を出さない', async () => {
		await page.getByLabel('接続先URL').fill(UNREACHABLE_HUB);
		await page.getByRole('button', { name: '接続' }).click();

		await expect(page.getByText('状態: Hubに到達できません')).toBeVisible();
		// 接続が失敗しても購読ブロックは「停止」のまま残る（消えない・
		// 例外で画面が落ちない）。
		await expect(page.getByText('購読: 停止')).toBeVisible();
		await expect(page.getByText('値を受信していません。')).toBeVisible();
	});

	test('6. 閲覧者アカウントを作成する（次のテストの前提）', async () => {
		await page.goto('/users');
		// 入力欄は「新規作成」セクションに限定して取る。一覧のグリッドには
		// 同名の列見出し（「ユーザー名の絞り込み」等）があり、ページ全体を
		// 対象にすると `getByLabel` が strict mode 違反になる。
		const createForm = page.locator('section.create');
		await expect(createForm.getByRole('heading', { level: 3, name: '新規作成' })).toBeVisible();
		await createForm.getByLabel('ユーザー名').fill(VIEWER_USERNAME);
		await createForm.getByLabel('パスワード（8文字以上）').fill(VIEWER_PASSWORD);
		await createForm.getByLabel('表示名').fill('E2E閲覧者');
		await createForm.getByLabel('ロール').selectOption('viewer');
		await createForm.getByRole('button', { name: '作成' }).click();
		await expect(page.locator('section.list').getByText(VIEWER_USERNAME).first()).toBeVisible();
	});

	test('7. 非 admin には Hub接続 が見えず、直接遷移は先頭の可視カテゴリへ弾かれる', async ({
		browser
	}) => {
		const viewerPage = await browser.newPage();
		try {
			await login(viewerPage, VIEWER_USERNAME, VIEWER_PASSWORD);

			await viewerPage.goto('/settings');
			const nav = viewerPage.getByRole('navigation', { name: '設定のカテゴリ' });
			await expect(nav.getByRole('link', { name: '外観' })).toBeVisible();
			await expect(nav.getByRole('link', { name: 'Hub接続' })).toHaveCount(0);

			await viewerPage.goto('/settings/hub');
			await expect(viewerPage).toHaveURL(/\/settings\/appearance$/);
			await expect(viewerPage.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();
		} finally {
			await viewerPage.close();
		}
	});
});
