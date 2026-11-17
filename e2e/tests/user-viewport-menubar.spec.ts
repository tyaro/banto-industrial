/**
 * #1159: ChronoGazer の既定になったメニューバー（`@banto/ui` の `HeaderNav`、ヘッダー統合）の
 * 実 DOM 受け入れテスト。レビューで「自動テストで保証されていない」とされた点:
 *
 * 1. キーボード操作（Tab で届く、「管理」ボタンの開閉・矢印・Esc、項目の選択と現在地）。
 * 2. 900px をまたぐときのフォーカス（`Header.svelte` のバー⇄ドロワーの受け渡し）。
 * 3. 背の低い画面でドロワーの最後の項目（「設定」）までスクロールして押せる
 *    （`Sidebar.svelte` の `aside.offcanvas { overflow-y: auto }`）。
 * 4. 未保存の変更があるときのメニューバーからの移動（`beforeNavigate` の確認）。
 *
 * 反証: 3 は `aside.offcanvas` の `overflow-y: auto` を外すと落ちる。4 は
 * `#lib/unsavedChanges.ts` の `guardUnsavedChanges` を呼ぶ行（ユーザー管理）を外すと落ちる。
 *
 * ファイル名: 管理者アカウントは `smoke.spec.ts` が作るので辞書順でそれより後（`s` < `u` < `v`）。
 * 既定のナビ（メニューバー）を使うので `banto.navLayout` は設定しない。
 * `HeaderNav` の約束: 「管理」ボタンは Enter/Space で開閉するだけ（フォーカスはボタンに
 * 残る）、ArrowDown/ArrowUp で中の項目へ入る、Esc で閉じてボタンへ戻る。
 */
import { expect, test, type Dialog, type Locator, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CONFIRM_MESSAGE = '保存していない変更があります。変更を破棄してこの画面から移動しますか？';

const WIDE = { width: 1280, height: 800 };
const NARROW = { width: 800, height: 800 };
const SHORT = { width: 800, height: 280 };

function mainNav(page: Page): Locator {
	return page.getByRole('banner').getByRole('navigation', { name: '主要ナビゲーション' });
}

/** document.activeElement が body/null に落ちていない。 */
async function focusKept(page: Page): Promise<boolean> {
	return page.evaluate(
		() => document.activeElement !== null && document.activeElement !== document.body
	);
}

async function login(page: Page): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
	await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer メニューバー キーボードと 900px 境界 (#1159)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		await login(page);
		await expect(mainNav(page)).toBeVisible();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. Tab でメニューバーのリンクに届き、「管理」は Enter/Space で開閉する', async () => {
		await page.setViewportSize(WIDE);
		await page.goto('/monitor');
		const nav = mainNav(page);
		const monitor = nav.getByRole('link', { name: '監視' });
		await expect(monitor).toHaveAttribute('aria-current', 'page');

		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
		let reached = false;
		for (let i = 0; i < 12 && !reached; i++) {
			await page.keyboard.press('Tab');
			reached = await monitor.evaluate((el) => el === document.activeElement);
		}
		expect(reached, 'Tab で「監視」リンクに届くこと').toBe(true);

		const admin = nav.getByRole('button', { name: '管理' });
		const users = nav.getByRole('link', { name: 'ユーザー管理' });
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
		await admin.focus();
		await page.keyboard.press('Enter');
		await expect(admin).toHaveAttribute('aria-expanded', 'true');
		await expect(users).toBeVisible();
		await expect(nav.getByRole('link', { name: '監査ログ' })).toBeVisible();
		// Enter は開くだけ: フォーカスはボタンに残る。
		await expect(admin).toBeFocused();

		await page.keyboard.press('Enter');
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
		await page.keyboard.press('Space');
		await expect(admin).toHaveAttribute('aria-expanded', 'true');
		await expect(users).toBeVisible();
	});

	test('2. 矢印で項目へ入って移れる。Esc で閉じてフォーカスは「管理」へ戻る', async () => {
		const nav = mainNav(page);
		const admin = nav.getByRole('button', { name: '管理' });
		await admin.focus();
		await page.keyboard.press('ArrowDown');
		await expect(nav.getByRole('link', { name: 'ユーザー管理' })).toBeFocused();
		await page.keyboard.press('ArrowDown');
		await expect(nav.getByRole('link', { name: '監査ログ' })).toBeFocused();
		await page.keyboard.press('ArrowUp');
		await expect(nav.getByRole('link', { name: 'ユーザー管理' })).toBeFocused();
		await page.keyboard.press('Escape');
		await expect(nav.getByRole('link', { name: 'ユーザー管理' })).toBeHidden();
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
		await expect(admin).toBeFocused();
	});

	test('3. Enter で項目を選ぶとそのページへ移り、現在地が変わる', async () => {
		const nav = mainNav(page);
		const admin = nav.getByRole('button', { name: '管理' });
		await admin.focus();
		await page.keyboard.press('ArrowDown');
		await expect(nav.getByRole('link', { name: 'ユーザー管理' })).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(page).toHaveURL(/\/users$/);
		await expect(admin).toHaveClass(/active/);
		await expect(nav.getByRole('link', { name: '監視' })).not.toHaveAttribute(
			'aria-current',
			'page'
		);
		await admin.focus();
		await page.keyboard.press('Enter');
		await expect(nav.getByRole('link', { name: 'ユーザー管理' })).toHaveAttribute(
			'aria-current',
			'page'
		);
		await page.keyboard.press('Escape');
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
	});

	test('4. 広幅→800px: バーのリンクのフォーカスは ☰ へ渡る（body に落ちない）', async () => {
		await page.setViewportSize(WIDE);
		await page.goto('/monitor');
		const nav = mainNav(page);
		await expect(nav.getByRole('link', { name: '監視' })).toBeVisible();
		await nav.getByRole('link', { name: 'イベント' }).focus();
		await expect(nav.getByRole('link', { name: 'イベント' })).toBeFocused();

		await page.setViewportSize(NARROW);
		const hamburger = page.getByRole('button', { name: 'メニューを開く', exact: true });
		await expect(hamburger).toBeVisible();
		await expect(nav).toBeHidden();
		await expect
			.poll(() => focusKept(page), { message: 'フォーカスが body に落ちない' })
			.toBe(true);
		await expect(hamburger).toBeFocused();
	});

	test('5. ☰ でドロワーを開き Esc で閉じるとフォーカスは ☰ へ戻る', async () => {
		const hamburger = page.getByRole('button', { name: 'メニューを開く', exact: true });
		await hamburger.focus();
		await page.keyboard.press('Enter');
		await expect(
			page.getByRole('button', { name: 'メニューを閉じる', exact: true })
		).toHaveAttribute('aria-expanded', 'true');
		await expect(page.getByRole('link', { name: 'イベント' })).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(page.getByRole('link', { name: 'イベント' })).toBeHidden();
		await expect(page.getByRole('button', { name: 'メニューを開く', exact: true })).toBeFocused();
	});

	test('6. ドロワーを開いたまま広げるとドロワーが閉じ、メニューバーが戻る', async () => {
		await page.getByRole('button', { name: 'メニューを開く', exact: true }).click();
		await expect(page.getByRole('link', { name: 'イベント' })).toBeVisible();
		await page.setViewportSize(WIDE);
		const nav = mainNav(page);
		await expect(nav).toBeVisible();
		await expect(nav.getByRole('link', { name: 'イベント' })).toBeVisible();
		await expect(page.getByRole('button', { name: /メニューを(開く|閉じる)/ })).toHaveCount(0);
		await expect(page.getByRole('link', { name: 'イベント' })).toHaveCount(1);
		await expect.poll(() => focusKept(page)).toBe(true);
	});

	test('7. 背の低い画面: ドロワーを開いて最後の「設定」までスクロールして押せる', async () => {
		await page.setViewportSize(SHORT);
		await page.goto('/monitor');
		await page.getByRole('button', { name: 'メニューを開く', exact: true }).click();
		const drawer = page.locator('aside.offcanvas');
		await expect(drawer).toHaveClass(/open/);
		const settingsLink = drawer.getByRole('link', { name: /(^|\s)設定$/ });
		await expect(settingsLink).toBeAttached();
		// 縦にスクロールできる（内容が画面より高い）こと自体を固定する。
		const dims = await drawer.evaluate((el) => ({
			scrollHeight: el.scrollHeight,
			clientHeight: el.clientHeight,
			overflowY: getComputedStyle(el).overflowY
		}));
		expect(dims.overflowY).toBe('auto');
		expect(dims.scrollHeight).toBeGreaterThan(dims.clientHeight);
		await settingsLink.scrollIntoViewIfNeeded();
		await expect(settingsLink).toBeInViewport({ ratio: 1 });
		await settingsLink.click();
		await expect(page).toHaveURL(/\/settings\/appearance$/);
	});
});

test.describe.serial('chronogazer メニューバーからの移動と未保存の確認 (#1159)', () => {
	let page: Page;
	let dialogs: string[] = [];
	let answer: 'stay' | 'leave' = 'stay';

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		page.on('dialog', (dialog: Dialog) => {
			dialogs.push(dialog.message());
			void (answer === 'leave' ? dialog.accept() : dialog.dismiss());
		});
		await login(page);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test.beforeEach(async () => {
		// 前のテストが入力を残したまま終わると、読み直し（beforeunload）の確認が出るので
		// 「離れる」で開き直してから、確認の記録と応答を初期化する。
		answer = 'leave';
		await page.goto('/users');
		await expect(page.getByRole('heading', { level: 1, name: 'ユーザー管理' })).toBeVisible();
		dialogs = [];
		answer = 'stay';
		await expect(mainNav(page)).toBeVisible();
	});

	function createSection(): Locator {
		return page.locator('section', { has: page.getByRole('heading', { name: '新規作成' }) });
	}

	test('1. 未保存でメニューバーの「監視」→ 確認 → キャンセルで留まり入力が残る', async () => {
		const create = createSection();
		await create.getByLabel('ユーザー名').fill('menubar-probe');
		await expect(create.locator('.banto-unsaved')).toHaveText('未保存の変更があります');

		await mainNav(page).getByRole('link', { name: '監視' }).click();
		await expect.poll(() => dialogs).toEqual([CONFIRM_MESSAGE]);
		await expect(page).toHaveURL(/\/users$/);
		await expect(create.getByLabel('ユーザー名')).toHaveValue('menubar-probe');
	});

	test('2. 承認するとメニューバーのリンク先へ移動し、現在地が変わる', async () => {
		answer = 'leave';
		await createSection().getByLabel('ユーザー名').fill('menubar-probe');
		await mainNav(page).getByRole('link', { name: '監視' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
		expect(dialogs).toEqual([CONFIRM_MESSAGE]);
		await expect(mainNav(page).getByRole('link', { name: '監視' })).toHaveAttribute(
			'aria-current',
			'page'
		);
	});

	test('3. 変更なしなら確認なしでメニューバーから移動する', async () => {
		await mainNav(page).getByRole('link', { name: '監視' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
		expect(dialogs).toEqual([]);
	});
});
