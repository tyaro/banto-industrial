/**
 * #1179 PR3b（docs/hub/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、段階 ①
 * 「2 つのページでツリーの選択を共有する」）の実 DOM 受け入れテスト。#1255 段階 ③で
 * タグモニタ（`/monitor`）を `/tags` に吸収したので、いまは「`/tags` で最後に選んだ対象を、
 * ナビのリンクが覚えていて、ほかのページから戻ったときに引き継ぐ」ことを見る
 * （`/tags` の URL の往復・存在しない id の扱いは `banto-hub-tags-view-tabs-url.spec.ts`）:
 *
 * - 別のページへ移ってからメニューバーの「タグ」で戻ると、直前の対象に戻る
 *   （「設定」タブは持ち越さない - 戻ると「タグ一覧」）
 * - URL に `target` があれば、覚えている対象より URL が優先
 * - 未保存の確認は、メニューバーで別のページへ移るときも効く
 *
 * `banto-hub-tags-*.spec.ts` と同じパターン（別 `describe.serial`、前提データは
 * REST で作る、`simulation: true`）。ファイル名は `smoke` より辞書順で後。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

/** ナビのリンク名（メニューバーは名前だけ、サイドバーは前にアイコンが付く）。 */
const NAV_TAGS = /^(?:\S+\s+)?タグ$/;
const NAV_STATUS = /^(?:\S+\s+)?状態$/;

const CONN = 'e2e-share-tree-conn';
const GROUP = 'e2e-share-tree-group';
const TAG = 'e2e-share-tree-tag';

test.describe.serial('banto-hub タグ: ナビによるツリーの選択の引き継ぎ (#1179 PR3b)', () => {
	let page: Page;
	let headers: Record<string, string>;
	let connId: number;
	let groupId: number;

	const tagsConnUrl = (): RegExp => new RegExp(`/tags\\?target=connection:${connId}$`);
	const tagsGroupUrl = (): RegExp => new RegExp(`/tags\\?target=group:${groupId}$`);

	const mainNav = (): ReturnType<Page['getByRole']> =>
		page.getByRole('banner').getByRole('navigation', { name: '主要ナビゲーション' });

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

	test('1. タグで選んだ対象は、別のページへ移ってからメニューバーの「タグ」で戻ると引き継がれる', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP).click();
		await expect(page).toHaveURL(tagsGroupUrl());

		await mainNav().getByRole('link', { name: NAV_STATUS }).click();
		await expect(page).toHaveURL(/\/status$/);

		await mainNav().getByRole('link', { name: NAV_TAGS }).click();
		await expect(page).toHaveURL(tagsGroupUrl());
		await expect(page.getByTestId('tag-tree-selection')).toHaveText(GROUP);
		await expect(groupNodeByName(page, GROUP)).toHaveAttribute('aria-selected', 'true');
	});

	test('2. 「設定」タブは持ち越さず、戻ると「タグ一覧」', async () => {
		await page.goto(`/tags?target=group:${groupId}&tab=settings`);
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');

		await mainNav().getByRole('link', { name: NAV_STATUS }).click();
		await expect(page).toHaveURL(/\/status$/);

		await mainNav().getByRole('link', { name: NAV_TAGS }).click();
		await expect(page).toHaveURL(tagsGroupUrl());
		await expect(page.getByRole('tab', { name: 'タグ一覧' })).toHaveAttribute(
			'aria-selected',
			'true'
		);
	});

	test('3. URL に target があれば、覚えている対象より URL が優先', async () => {
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
		}, `/tags?target=connection:${connId}`);
		await page.locator('#e2e-share-tree-link').click();
		await expect(page).toHaveURL(tagsConnUrl());
		await expect(page.getByTestId('tag-tree-selection')).toHaveText(CONN);

		// 素の URL で開いたら「すべて」（覚えている対象は URL を書き換えない）。
		await page.goto('/tags');
		await expect(page).toHaveURL(/\/tags$/);
		await expect(page.getByTestId('tag-tree-selection')).toHaveText('すべて');
	});

	test('4. 未保存の確認は、メニューバーで別のページへ移るときも効く', async () => {
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
		await mainNav().getByRole('link', { name: NAV_STATUS }).click();
		await expect.poll(() => message).toBe('変更を破棄しますか？');
		await expect(page).toHaveURL(new RegExp(`/tags\\?target=group:${groupId}&tab=settings$`));
		await expect(nameInput).toHaveValue(`${GROUP}-edited`);

		page.once('dialog', (dialog) => void dialog.accept());
		await mainNav().getByRole('link', { name: NAV_STATUS }).click();
		await expect(page).toHaveURL(/\/status$/);
	});
});
