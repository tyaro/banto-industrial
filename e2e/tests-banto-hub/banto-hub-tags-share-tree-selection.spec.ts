/**
 * #1179 PR3b（docs/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、段階 ①
 * 「2 つのページでツリーの選択を共有する」）の実 DOM 受け入れテスト:
 *
 * - タグモニタもツリーの選択を URL（`?target=`、タグ登録と同じ形）に持ち、
 *   再読み込み・戻る・進むで戻る。上下キーの移動は履歴の 1 項目にまとめる
 * - 存在しない id は「すべて」へ戻して URL を正規化し、案内する
 * - メニューバーでタグ登録 ⇔ タグモニタを行き来すると、直前の対象が引き継がれる
 *   （タグ登録の「設定」タブは持ち越さない - 戻ると「タグ一覧」）
 * - URL に `target` があれば、覚えている対象より URL が優先
 * - タグ登録の未保存の確認は、メニューバーでタグモニタへ移るときも効く
 *
 * `banto-hub-tags-*.spec.ts` と同じパターン（別 `describe.serial`、前提データは
 * REST で作る、`simulation: true`）。ファイル名は `smoke` より辞書順で後。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

const CONN = 'e2e-share-tree-conn';
const GROUP = 'e2e-share-tree-group';
const TAG = 'e2e-share-tree-tag';

test.describe.serial('banto-hub タグ登録・タグモニタ: ツリーの選択の共有 (#1179 PR3b)', () => {
	let page: Page;
	let headers: Record<string, string>;
	let connId: number;
	let groupId: number;

	const monitorConnUrl = (): RegExp => new RegExp(`/monitor\\?target=connection:${connId}$`);
	const monitorGroupUrl = (): RegExp => new RegExp(`/monitor\\?target=group:${groupId}$`);
	const tagsConnUrl = (): RegExp => new RegExp(`/tags\\?target=connection:${connId}$`);
	const tagsGroupUrl = (): RegExp => new RegExp(`/tags\\?target=group:${groupId}$`);

	/** 接続先の行（シミュレーションの接続は名前の後ろに「⚠ SIM」が付く）。 */
	const connNode = (): ReturnType<Page['getByRole']> =>
		page.getByRole('tree').getByRole('treeitem', { name: new RegExp(`^${CONN}( |$)`) });

	const mainNav = (): ReturnType<Page['getByRole']> =>
		page.getByRole('banner').getByRole('navigation', { name: '主要ナビゲーション' });

	const monitorSelection = (): ReturnType<Page['getByTestId']> =>
		page.getByTestId('monitor-tree-selection');

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await cleanupFixtures(page.request, headers, {
			groupNames: [GROUP],
			connectionNames: [CONN]
		});

		const connRes = await page.request.post('/api/plc-connections', {
			headers,
			data: {
				name: CONN,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connRes.ok()).toBe(true);
		connId = ((await connRes.json()) as { id: number }).id;

		const groupRes = await page.request.post('/api/collection-groups', {
			headers,
			data: { name: GROUP, plcConnectionId: connId, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;

		const tagRes = await page.request.post('/api/tags', {
			headers,
			data: {
				name: TAG,
				collectionGroupId: groupId,
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
			groupNames: [GROUP],
			connectionNames: [CONN]
		});
		await page.close();
	});

	test('1. タグモニタ: 選んだ対象が URL に載り、再読み込み・戻る・進むで戻る', async () => {
		await page.goto('/monitor');
		await expect(monitorSelection()).toHaveText('すべて');

		await groupNodeByName(page, GROUP).click();
		await expect(page).toHaveURL(monitorGroupUrl());
		await connNode().click();
		await expect(page).toHaveURL(monitorConnUrl());
		await expect(monitorSelection()).toHaveText(CONN);

		await page.reload();
		await expect(monitorSelection()).toHaveText(CONN);
		await expect(connNode()).toHaveAttribute('aria-selected', 'true');
		await expect(page).toHaveURL(monitorConnUrl());

		await page.goBack();
		await expect(page).toHaveURL(monitorGroupUrl());
		await expect(monitorSelection()).toHaveText(GROUP);
		await expect(groupNodeByName(page, GROUP)).toHaveAttribute('aria-selected', 'true');

		await page.goBack();
		await expect(page).toHaveURL(/\/monitor$/);
		await expect(monitorSelection()).toHaveText('すべて');

		await page.goForward();
		await expect(page).toHaveURL(monitorGroupUrl());
		await expect(monitorSelection()).toHaveText(GROUP);
	});

	test('2. タグモニタ: 上下キーの移動は履歴の 1 項目にまとまる', async () => {
		await page.goto('/monitor');
		await connNode().click();
		await expect(page).toHaveURL(monitorConnUrl());

		// 接続先の下は配下のグループ（展開済み）。2 回目の下で別の行へ移っても、
		// 移らなくても、キーで積んだ項目は 1 つだけ。
		await page.keyboard.press('ArrowDown');
		await expect(page).toHaveURL(monitorGroupUrl());
		await page.keyboard.press('ArrowDown');
		await page.keyboard.press('ArrowUp');
		await expect(page).toHaveURL(monitorGroupUrl());

		await page.goBack();
		await expect(page).toHaveURL(monitorConnUrl());
		await expect(monitorSelection()).toHaveText(CONN);
	});

	test('3. タグモニタ: 存在しない id は「すべて」へ戻して URL を直し、案内する', async () => {
		await page.goto('/monitor?target=group:2147483647');
		await expect(page).toHaveURL(/\/monitor$/);
		await expect(
			page.getByText('指定された接続先・収集グループが見つからないため', { exact: false })
		).toBeVisible();
		await expect(monitorSelection()).toHaveText('すべて');

		// 読めない値（初めから「すべて」と同じ表示になる）でも案内する。`page.goto` は
		// 文書の読み込み直しなので、上の案内は残っていない。
		await page.goto('/monitor?target=bogus');
		await expect(page).toHaveURL(/\/monitor$/);
		await expect(
			page.getByText('指定された接続先・収集グループが見つからないため', { exact: false })
		).toBeVisible();
		await expect(monitorSelection()).toHaveText('すべて');
	});

	test('4. タグ登録で選んだ対象が、メニューバーでタグモニタへ移っても同じ（逆も）', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP).click();
		await expect(page).toHaveURL(tagsGroupUrl());

		await mainNav().getByRole('link', { name: 'タグモニタ' }).click();
		await expect(page).toHaveURL(monitorGroupUrl());
		await expect(monitorSelection()).toHaveText(GROUP);
		await expect(groupNodeByName(page, GROUP)).toHaveAttribute('aria-selected', 'true');

		await connNode().click();
		await expect(page).toHaveURL(monitorConnUrl());
		await mainNav().getByRole('link', { name: 'タグ登録' }).click();
		await expect(page).toHaveURL(tagsConnUrl());
		await expect(page.getByTestId('tag-tree-selection')).toHaveText(CONN);

		// 戻るはタグモニタの接続先へ（ストアではなく URL が決める）。
		await page.goBack();
		await expect(page).toHaveURL(monitorConnUrl());
		await expect(monitorSelection()).toHaveText(CONN);
	});

	test('5. タグ登録の「設定」タブはタグモニタへ持ち越さず、戻ると「タグ一覧」', async () => {
		await page.goto(`/tags?target=group:${groupId}&tab=settings`);
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');

		await mainNav().getByRole('link', { name: 'タグモニタ' }).click();
		await expect(page).toHaveURL(monitorGroupUrl());

		await mainNav().getByRole('link', { name: 'タグ登録' }).click();
		await expect(page).toHaveURL(tagsGroupUrl());
		await expect(page.getByRole('tab', { name: 'タグ一覧' })).toHaveAttribute(
			'aria-selected',
			'true'
		);
	});

	test('6. URL に target があれば、覚えている対象より URL が優先', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP).click();
		await expect(page).toHaveURL(tagsGroupUrl());

		// アプリ内のリンク（クライアント側の遷移で、覚えている対象は残ったまま）。
		await page.evaluate((href) => {
			const link = document.createElement('a');
			link.href = href;
			link.id = 'e2e-share-tree-link';
			link.textContent = 'e2e';
			document.body.append(link);
		}, `/monitor?target=connection:${connId}`);
		await page.locator('#e2e-share-tree-link').click();
		await expect(page).toHaveURL(monitorConnUrl());
		await expect(monitorSelection()).toHaveText(CONN);

		// 素の URL で開いたら「すべて」（覚えている対象は URL を書き換えない）。
		await page.goto('/monitor');
		await expect(page).toHaveURL(/\/monitor$/);
		await expect(monitorSelection()).toHaveText('すべて');
	});

	test('7. タグ登録の未保存の確認は、メニューバーでタグモニタへ移るときも効く', async () => {
		await page.goto(`/tags?target=group:${groupId}&tab=settings`);
		const settingsPane = page.getByTestId('tag-settings-pane');
		const nameInput = settingsPane.getByLabel('名前', { exact: true });
		await expect(nameInput).toHaveValue(GROUP);
		await nameInput.fill(`${GROUP}-edited`);

		let message: string | null = null;
		page.once('dialog', (dialog) => {
			message = dialog.message();
			void dialog.dismiss();
		});
		await mainNav().getByRole('link', { name: 'タグモニタ' }).click();
		await expect.poll(() => message).toBe('変更を破棄しますか？');
		await expect(page).toHaveURL(new RegExp(`/tags\\?target=group:${groupId}&tab=settings$`));
		await expect(nameInput).toHaveValue(`${GROUP}-edited`);

		page.once('dialog', (dialog) => void dialog.accept());
		await mainNav().getByRole('link', { name: 'タグモニタ' }).click();
		await expect(page).toHaveURL(monitorGroupUrl());
	});
});
