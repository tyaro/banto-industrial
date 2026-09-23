/**
 * #383 段階2b / R1-C の C-3b（収集の画面）の実 DOM 固定。
 *
 * ここで固定するのは指示どおりの 2 点:
 * - `/settings/collect` で**収集の状態が出る**こと（5 状態のうち実サーバーが
 *   実際に取る状態がそのまま出て、接続ごとの状態も「走っていないから無い」と
 *   「0 件」を混同しない形で出ること）。
 * - **viewer には操作（開始・停止・再起動）が出ない**こと。`disabled` では
 *   なく**出さない**（`tags.spec.ts` の「viewer に新規作成・削除を出さない」
 *   と同じ方針）。
 *
 * **`page.route` は使っていない**。`banto-serve` は起動時に収集を自動開始し
 * （`CollectorService::autostart()`）、そのときレジストリは空なので状態は
 * `noTargets` = 「収集対象がありません」で確定する。**レジストリの CRUD では
 * 自動再起動しない**のが C-2 で決めた設計なので、先に走る `tags.spec.ts` が
 * PLC接続・収集グループ・タグを作っても収集の状態は変わらない（反映は明示的な
 * 「収集を再起動」だけ）。つまり実サーバーのままで状態表示を確かめられる。
 *
 * **このスペックは収集の操作を実行しない**。押すと状態が変わり、以後の
 * スペックと「起動時の状態」という前提を共有できなくなるうえ、実在しない PLC
 * への接続を試みて実行時間が伸びる。操作の結末（`pending`・打ち切り・未受付を
 * 混ぜない言い分け）は `collectAdmin.test.ts` が純関数として総当たりで固定して
 * いる。シミュレータ相手の一巡（開始 → ファイル生成 → イベント記録）は
 * C-4（`apps/chronogazer/core/tests/collect_roundtrip.rs` と
 * `e2e/tests/user-simulator-roundtrip.spec.ts`）が確かめている。
 *
 * ファイル名について: `smoke.spec.ts` の最初のテストが初回セットアップ
 * （管理者アカウント作成）を実 DOM で行う（`playwright.config.ts` は
 * `workers: 1`/`fullyParallel: false` でファイル名の辞書順に実行する）ため、
 * このファイルはそれより後に来る名前でなければならない（`user-settings-hub
 * .spec.ts` の doc comment にある罠と同じ）。`user-settings-collect` は
 * `smoke`/`tags` より後、`user-settings-hub` より前（`c` < `h`）。閲覧者
 * アカウントは**このスペック自身で作る**ので、他のスペックが作るアカウントに
 * 依存しない。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// このスペックが作る閲覧者アカウント（操作が出ないことを確かめるため）。
const VIEWER_USERNAME = 'e2e-collect-viewer';
const VIEWER_PASSWORD = 'E2eViewerPass1';

const ACTION_LABELS = ['収集を開始', '収集を停止', '収集を再起動'];

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer 収集の設定カテゴリ（R1-C の C-3b）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 収集カテゴリで状態・接続ごとの状態が出て、editor 以上には操作が並ぶ', async () => {
		await page.goto('/settings');
		const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
		await nav.getByRole('link', { name: '収集' }).click();
		await expect(page).toHaveURL(/\/settings\/collect$/);
		await expect(page.getByRole('heading', { level: 2, name: '収集' })).toBeVisible();

		// 起動時の自動開始が空のレジストリで終わった状態。「失敗」でも
		// 「0 件」でもない専用の状態として出る。
		await expect(page.getByText('状態: 収集対象がありません')).toBeVisible();
		await expect(page.getByText('失敗ではありません')).toBeVisible();

		// 接続ごとの状態は「走っていないから無い」。読めなかった・0 件と
		// 混同しない文言であること。
		await expect(page.getByRole('heading', { level: 3, name: '接続ごとの状態' })).toBeVisible();
		await expect(
			page.getByText('収集が動いていないため、接続ごとの状態はありません。')
		).toBeVisible();
		// 走っていないときに接続の表（見出し）は出さない。
		await expect(page.getByRole('columnheader', { name: '状態' })).toHaveCount(0);

		for (const label of ACTION_LABELS) {
			await expect(page.getByRole('button', { name: label })).toBeVisible();
		}
	});

	test('2. 閲覧者アカウントを作成する（次のテストの前提）', async () => {
		await page.goto('/users');
		// 入力欄は「新規作成」セクションに限定して取る（一覧のグリッドに同名の
		// 列見出しがあり、ページ全体を対象にすると strict mode 違反になる）。
		const createForm = page.locator('section.create');
		await expect(createForm.getByRole('heading', { level: 3, name: '新規作成' })).toBeVisible();
		await createForm.getByLabel('ユーザー名').fill(VIEWER_USERNAME);
		await createForm.getByLabel('パスワード（8文字以上）').fill(VIEWER_PASSWORD);
		await createForm.getByLabel('表示名').fill('E2E収集閲覧者');
		await createForm.getByLabel('ロール').selectOption('viewer');
		await createForm.getByRole('button', { name: '作成' }).click();
		await expect(page.locator('section.list').getByText(VIEWER_USERNAME).first()).toBeVisible();
	});

	test('3. viewer にも状態は見えるが、操作は1つも出ない', async ({ browser }) => {
		const viewerPage = await browser.newPage();
		try {
			await login(viewerPage, VIEWER_USERNAME, VIEWER_PASSWORD);

			// 収集カテゴリは admin 限定ではない（状態の読み取りは viewer 以上）。
			await viewerPage.goto('/settings');
			const nav = viewerPage.getByRole('navigation', { name: '設定のカテゴリ' });
			await expect(nav.getByRole('link', { name: '収集' })).toBeVisible();
			// Hub接続（admin 限定）は見えない - 可視性が「全部見える」に倒れて
			// いないことの対照。
			await expect(nav.getByRole('link', { name: 'Hub接続' })).toHaveCount(0);

			await viewerPage.goto('/settings/collect');
			await expect(viewerPage).toHaveURL(/\/settings\/collect$/);
			await expect(viewerPage.getByText('状態: 収集対象がありません')).toBeVisible();

			for (const label of ACTION_LABELS) {
				await expect(viewerPage.getByRole('button', { name: label })).toHaveCount(0);
			}
			await expect(
				viewerPage.getByText('収集の開始・停止・再起動は、編集者以上のアカウントで行えます。')
			).toBeVisible();
		} finally {
			await viewerPage.close();
		}
	});
});
