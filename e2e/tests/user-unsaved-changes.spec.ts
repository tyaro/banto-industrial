/**
 * #508: 未保存の入力があるときの画面移動の確認（`beforeNavigate`）の実 DOM 固定。
 * 上流 admin-template（banto #214）の写し（`#lib/unsavedChanges.ts`）が、
 * ChronoGazer の画面で働くことを確認する。確認は `window.confirm`
 * （上流と同じ）で、Playwright では `dialog` イベントの `dismiss()` が「留まる」、
 * `accept()` が「離れる」。
 *
 * 固定したい受入条件:
 * 1. 入力 → 別の画面へ → 「留まる」を選ぶと、移動せず入力が残る（タグ設定・
 *    ユーザー管理・アカウントのパスワード欄）。
 * 2. 「離れる」を選ぶと移動する。
 * 3. 入力が無い（きれいな）ときは確認を出さない。
 * 4. セッションが終わる移動（ログアウト → /login）は、未保存の入力があっても
 *    確認を出さない（上流の `isForcedNavigation`）。
 * 反証: `#lib/unsavedChanges.ts` の `guardUnsavedChanges` を呼ぶ行（各画面）を
 * 外すと 1 が落ちる（確認が出ず移動してしまう）。
 *
 * ファイル名について: この E2E サーバーの管理者アカウントは `smoke.spec.ts`
 * が作るので、辞書順でそれより後になる名前にする（`user-settings-routes`
 * と同じ理由）。ログアウトで終わるテストを含むので、ファイル順で最後
 * （`user-u...`）に置いてある。
 */
import { expect, test, type Dialog, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

const CONFIRM_MESSAGE = '保存していない変更があります。変更を破棄してこの画面から移動しますか？';

test.describe.serial('chronogazer 未保存の入力の確認', () => {
	let page: Page;
	// 出た確認（window.confirm）のメッセージ。`answer` が応答（留まる/離れる）。
	let dialogs: string[] = [];
	let answer: 'stay' | 'leave' = 'stay';

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		page.on('dialog', (dialog: Dialog) => {
			dialogs.push(dialog.message());
			void (answer === 'leave' ? dialog.accept() : dialog.dismiss());
		});
		await page.goto('/login');
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test.beforeEach(() => {
		dialogs = [];
		answer = 'stay';
	});

	function sidebarLink(name: RegExp) {
		return page.locator('aside nav').getByRole('link', { name });
	}

	test('1. ユーザー管理: 入力 → 別の画面へ → 「留まる」で移動せず入力が残る', async () => {
		await page.goto('/users');
		const create = page.locator('section', {
			has: page.getByRole('heading', { name: '新規作成' })
		});
		await create.getByLabel('ユーザー名').fill('unsaved-probe');
		await expect(create.locator('.banto-unsaved')).toHaveText('未保存の変更があります');

		await sidebarLink(/監視/).click();
		await expect.poll(() => dialogs).toEqual([CONFIRM_MESSAGE]);
		await expect(page).toHaveURL(/\/users$/);
		await expect(create.getByLabel('ユーザー名')).toHaveValue('unsaved-probe');
	});

	test('2. 「離れる」を選ぶと移動する', async () => {
		// 1 の続き（/users に入力が残っている）。
		answer = 'leave';
		await sidebarLink(/監視/).click();
		await expect(page).toHaveURL(/\/monitor$/);
		expect(dialogs).toEqual([CONFIRM_MESSAGE]);
	});

	test('3. 入力が無いときは確認を出さずに移動する', async () => {
		await page.goto('/users');
		await expect(page.getByRole('heading', { level: 1, name: 'ユーザー管理' })).toBeVisible();
		await sidebarLink(/監視/).click();
		await expect(page).toHaveURL(/\/monitor$/);
		expect(dialogs).toEqual([]);
	});

	test('4. タグ設定: PLC接続の新規作成に入力 → 別の画面へ → 「留まる」で入力が残り、「離れる」で移動する', async () => {
		await page.goto('/tags');
		const form = page.locator('section.registry-section').nth(0).locator('div.create');
		await form.getByLabel('名前').fill('unsaved-plc');
		await expect(form.locator('.banto-unsaved')).toHaveText('未保存の変更があります');

		await sidebarLink(/イベント/).click();
		await expect.poll(() => dialogs).toEqual([CONFIRM_MESSAGE]);
		await expect(page).toHaveURL(/\/tags$/);
		await expect(form.getByLabel('名前')).toHaveValue('unsaved-plc');

		answer = 'leave';
		await sidebarLink(/イベント/).click();
		await expect(page).toHaveURL(/\/events$/);
	});

	test('5. 設定のカテゴリの間の移動も確認する（アカウントのパスワード欄）', async () => {
		await page.goto('/settings/account');
		await page.getByLabel('新しいパスワード（8文字以上）').fill('typed-not-saved');
		await expect(page.locator('.banto-unsaved')).toHaveText('未保存の変更があります');

		await page
			.getByRole('navigation', { name: '設定のカテゴリ' })
			.getByRole('link', { name: '外観' })
			.click();
		await expect.poll(() => dialogs).toEqual([CONFIRM_MESSAGE]);
		await expect(page).toHaveURL(/\/settings\/account$/);
		await expect(page.getByLabel('新しいパスワード（8文字以上）')).toHaveValue('typed-not-saved');

		// 「変更を取り消す」で入力が消えると、未保存の表示も確認も無くなる。
		await page.getByRole('button', { name: '変更を取り消す' }).click();
		await expect(page.locator('.banto-unsaved')).toHaveCount(0);
		dialogs = [];
		await page
			.getByRole('navigation', { name: '設定のカテゴリ' })
			.getByRole('link', { name: '外観' })
			.click();
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		expect(dialogs).toEqual([]);
	});

	test('5b. 設定の全カテゴリは、開いた直後（入力なし）は「未保存」でなく、離れても確認を出さない', async () => {
		const labels = ['外観', 'アカウント', '接続', 'データ', 'セキュリティ', 'Hub接続', '収集'];
		let checked = 0;
		for (const label of labels) {
			dialogs = [];
			await page.goto('/settings/appearance');
			const nav = page.getByRole('navigation', { name: '設定のカテゴリ' });
			// ナビが描画される（= カテゴリの一覧が確定する）まで待ってから数える。
			await expect(nav.getByRole('link', { name: '外観', exact: true })).toBeVisible();
			const link = nav.getByRole('link', { name: label, exact: true });
			// この環境で見えないカテゴリ（例: セキュリティは非表示）は飛ばす。
			if ((await link.count()) === 0) continue;
			checked += 1;
			await link.click();
			await expect(link).toHaveAttribute('aria-current', 'page');
			// 初期読み込み（非同期の取得）が反映されてから確かめる（購読の通信が続くので
			// networkidle は使えない）。
			await page.waitForTimeout(700);
			await expect(page.locator('.banto-unsaved'), `${label}: 開いた直後`).toHaveCount(0);

			await sidebarLink(/監視/).click();
			await expect(page, `${label}: 離れる`).toHaveURL(/\/monitor$/);
			expect(dialogs, `${label}: 確認が出ない`).toEqual([]);
		}
		// 外観・アカウント・接続・データ・Hub接続・収集のうち、少なくとも 5 つは見えている。
		expect(checked).toBeGreaterThanOrEqual(5);
	});

	test('6. ログアウト（/login への移動）は、未保存の入力があっても確認を出さない', async () => {
		await page.goto('/settings/account');
		await page.getByLabel('新しいパスワード（8文字以上）').fill('typed-not-saved');
		await expect(page.locator('.banto-unsaved')).toHaveText('未保存の変更があります');

		await page.getByRole('button', { name: 'ログアウト' }).click();
		await expect(page).toHaveURL(/\/login$/);
		expect(dialogs).toEqual([]);
	});
});
