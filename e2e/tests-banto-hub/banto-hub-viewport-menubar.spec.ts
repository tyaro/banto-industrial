/**
 * #1159: Hub の既定になったメニューバー（`@banto/ui` の `HeaderNav`、ヘッダー統合）の
 * 実 DOM 受け入れテスト。レビューで「自動テストで保証されていない」とされた 3 点:
 *
 * 1. キーボード操作（Tab で届く、「管理」ボタンの開閉・矢印・Esc、項目の選択と現在地）。
 * 2. 900px をまたぐときのフォーカス（`Header.svelte` のバー⇄ドロワーの受け渡し）。
 * 3. 未保存の変更があるときのメニューバーからの移動（`beforeNavigate` の確認）。
 *
 * ファイル名は `banto-hub-smoke` より辞書順で後（`banto-hub-auth.ts` の約束）。
 * `HeaderNav` の約束: 「管理」ボタンは Enter/Space で開くだけ（フォーカスはボタンに
 * 残る）、ArrowDown/ArrowUp で中の項目へ入る、Esc で閉じてボタンへ戻る。
 */
import { expect, test, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, ensureLoggedIn, fetchAuthToken, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

/** ナビのリンク名（メニューバーは名前だけ、サイドバーは前にアイコンが付く）。 */
const NAV_TAGS = /^(?:\S+\s+)?タグ$/;

const WIDE = { width: 1280, height: 800 };
const NARROW = { width: 800, height: 800 };

const CONNECTION_NAME = 'e2e-menubar-plc';
const GROUP_NAME = 'e2e-menubar-group';
const TAG_NAME = 'e2e-menubar-tag';

function mainNav(page: Page): Locator {
	return page.getByRole('banner').getByRole('navigation', { name: '主要ナビゲーション' });
}

/** document.activeElement が body/null に落ちていない。 */
async function focusKept(page: Page): Promise<boolean> {
	return page.evaluate(
		() => document.activeElement !== null && document.activeElement !== document.body
	);
}

test.describe.serial('banto-hub メニューバー キーボードと 900px 境界 (#1159)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		await page.goto('/login');
		await ensureLoggedIn(page);
		await page.goto('/status');
		await expect(mainNav(page)).toBeVisible();
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. Tab でメニューバーのリンクに届き、「管理」は Enter/Space で開く', async () => {
		await page.setViewportSize(WIDE);
		await page.goto('/status');
		const nav = mainNav(page);
		const status = nav.getByRole('link', { name: '状態' });
		await expect(status).toHaveAttribute('aria-current', 'page');

		// ページの先頭から Tab を進めて、バーの「状態」リンクに届く。
		await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
		let reached = false;
		for (let i = 0; i < 12 && !reached; i++) {
			await page.keyboard.press('Tab');
			reached = await status.evaluate((el) => el === document.activeElement);
		}
		expect(reached, 'Tab で「状態」リンクに届くこと').toBe(true);

		const admin = nav.getByRole('button', { name: '管理' });
		const apiKeys = nav.getByRole('link', { name: 'APIキー' });
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
		await admin.focus();
		await page.keyboard.press('Enter');
		await expect(admin).toHaveAttribute('aria-expanded', 'true');
		await expect(apiKeys).toBeVisible();
		for (const name of ['ユーザー管理', '監査ログ', '書き込み監査']) {
			await expect(nav.getByRole('link', { name })).toBeVisible();
		}
		// Enter は開くだけ: フォーカスはボタンに残る。
		await expect(admin).toBeFocused();

		// もう一度 Enter で閉じ、Space で開く。
		await page.keyboard.press('Enter');
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
		await page.keyboard.press('Space');
		await expect(admin).toHaveAttribute('aria-expanded', 'true');
		await expect(apiKeys).toBeVisible();
	});

	test('2. 矢印で項目へ入って移れる。Esc で閉じてフォーカスは「管理」へ戻る', async () => {
		const nav = mainNav(page);
		const admin = nav.getByRole('button', { name: '管理' });
		await admin.focus();
		await page.keyboard.press('ArrowDown');
		await expect(nav.getByRole('link', { name: 'APIキー' })).toBeFocused();
		await page.keyboard.press('ArrowDown');
		await expect(nav.getByRole('link', { name: 'ユーザー管理' })).toBeFocused();
		await page.keyboard.press('ArrowUp');
		await expect(nav.getByRole('link', { name: 'APIキー' })).toBeFocused();
		await page.keyboard.press('Escape');
		await expect(nav.getByRole('link', { name: 'APIキー' })).toBeHidden();
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
		await expect(admin).toBeFocused();
	});

	test('3. Enter で項目を選ぶとそのページへ移り、現在地が変わる', async () => {
		const nav = mainNav(page);
		const admin = nav.getByRole('button', { name: '管理' });
		await admin.focus();
		await page.keyboard.press('ArrowDown');
		await expect(nav.getByRole('link', { name: 'APIキー' })).toBeFocused();
		await page.keyboard.press('Enter');
		await expect(page).toHaveURL(/\/api-keys$/);
		// 現在地: 「管理」ボタンが active、「状態」リンクは現在地でなくなる。
		await expect(admin).toHaveClass(/active/);
		await expect(nav.getByRole('link', { name: '状態' })).not.toHaveAttribute(
			'aria-current',
			'page'
		);
		await admin.focus();
		await page.keyboard.press('Enter');
		await expect(nav.getByRole('link', { name: 'APIキー' })).toHaveAttribute(
			'aria-current',
			'page'
		);
		await page.keyboard.press('Escape');
		await expect(admin).toHaveAttribute('aria-expanded', 'false');
	});

	test('4. 広幅→800px: バーのリンクのフォーカスは ☰ へ渡る（body に落ちない）', async () => {
		await page.setViewportSize(WIDE);
		await page.goto('/status');
		const nav = mainNav(page);
		await expect(nav.getByRole('link', { name: '状態' })).toBeVisible();
		await nav.getByRole('link', { name: NAV_TAGS }).focus();
		await expect(nav.getByRole('link', { name: NAV_TAGS })).toBeFocused();

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
		await expect(page.getByRole('link', { name: NAV_TAGS })).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(page.getByRole('link', { name: NAV_TAGS })).toBeHidden();
		await expect(page.getByRole('button', { name: 'メニューを開く', exact: true })).toBeFocused();
	});

	test('6. ドロワーを開いたまま広げるとドロワーが閉じ、メニューバーが戻る', async () => {
		await page.getByRole('button', { name: 'メニューを開く', exact: true }).click();
		await expect(page.getByRole('link', { name: NAV_TAGS })).toBeVisible();
		await page.setViewportSize(WIDE);
		const nav = mainNav(page);
		await expect(nav).toBeVisible();
		await expect(nav.getByRole('link', { name: NAV_TAGS })).toBeVisible();
		// ☰ はもう無く、サイドバーのリンクも出ていない（バーの分だけ）。
		await expect(page.getByRole('button', { name: /メニューを(開く|閉じる)/ })).toHaveCount(0);
		await expect(page.getByRole('link', { name: NAV_TAGS })).toHaveCount(1);
		await expect.poll(() => focusKept(page)).toBe(true);
	});
});

test.describe.serial('banto-hub メニューバーからの移動と未保存の確認 (#1159)', () => {
	let page: Page;
	let headers: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await cleanupFixtures(page.request, headers, {
			groupNames: [GROUP_NAME],
			connectionNames: [CONNECTION_NAME]
		});

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };
		const groupRes = await page.request.post('/api/collection-groups', {
			headers,
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };
		const tagRes = await page.request.post('/api/tags', {
			headers,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await cleanupFixtures(page.request, headers, {
			groupNames: [GROUP_NAME],
			connectionNames: [CONNECTION_NAME]
		});
		await page.close();
	});

	test.beforeEach(async () => {
		page.removeAllListeners('dialog');
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_NAME);
		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		await expect(page.getByRole('complementary', { name: `${TAG_NAME} を編集` })).toBeVisible();
	});

	test('1. 未保存でメニューバーの「状態」→ 確認 → キャンセルで留まり編集が残る', async () => {
		const drawer = page.getByRole('complementary', { name: `${TAG_NAME} を編集` });
		const nameInput = drawer.getByLabel('名前');
		await nameInput.fill(`${TAG_NAME}-changed`);

		let message: string | null = null;
		page.once('dialog', (dialog) => {
			message = dialog.message();
			void dialog.dismiss();
		});
		await mainNav(page).getByRole('link', { name: '状態' }).click();
		await expect.poll(() => message).toBe('変更を破棄しますか？');

		await expect(page).toHaveURL(/\/tags$/);
		await expect(drawer).toBeVisible();
		await expect(nameInput).toHaveValue(`${TAG_NAME}-changed`);
	});

	test('2. 承認するとメニューバーのリンク先へ移動する', async () => {
		const drawer = page.getByRole('complementary', { name: `${TAG_NAME} を編集` });
		await drawer.getByLabel('名前').fill(`${TAG_NAME}-changed`);

		let message: string | null = null;
		page.once('dialog', (dialog) => {
			message = dialog.message();
			void dialog.accept();
		});
		await mainNav(page).getByRole('link', { name: '状態' }).click();
		await expect.poll(() => message).toBe('変更を破棄しますか？');
		await expect(page).toHaveURL(/\/status$/);
		await expect(mainNav(page).getByRole('link', { name: '状態' })).toHaveAttribute(
			'aria-current',
			'page'
		);
	});

	test('3. 変更なしなら確認なしでメニューバーから移動する', async () => {
		let dialogShown = false;
		page.on('dialog', (dialog) => {
			dialogShown = true;
			void dialog.dismiss();
		});
		await mainNav(page).getByRole('link', { name: '状態' }).click();
		await expect(page).toHaveURL(/\/status$/);
		expect(dialogShown).toBe(false);
	});
});
