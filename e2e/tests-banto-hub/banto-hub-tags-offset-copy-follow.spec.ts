/**
 * R2b-6d-2a の残課題（PR 本文）: オフセットコピーの衝突判定が、開いたまま別のクライアントが
 * 同じグループにタグを足す・消す変更に追従すること（`+page.svelte` の
 * `bulkOffsetCopyExistingKey` → `existingTagsKey`）を固定する。
 *
 * 画面を開いたまま、REST で直接（別のクライアントとして）コピー先のアドレスにタグを足すと、
 * 一覧の再取得の後に衝突のエラーが出て検証・実行が止まり、そのタグを消すと衝突が消えて検証できる。
 * 純関数の `existingTagsKey` は vitest にあるので、ここは画面が実際にそれへ追従する経路だけを見る。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-occ-follow-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-occ-follow-group-${RUN_ID}`;
const SOURCE_TAG = 'e2eocfsrc';

test.describe.serial('banto-hub オフセットコピーの衝突判定が別クライアントの変更に追従する', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupId: number;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'slmp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 5011 }
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };
		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;
		await createTag(SOURCE_TAG, 'D5000');
	});

	test.afterAll(async () => {
		await page.close();
	});

	async function createTag(name: string, address: string): Promise<number> {
		const res = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name,
				collectionGroupId: groupId,
				address,
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(res.ok()).toBe(true);
		return ((await res.json()) as { id: number }).id;
	}

	/**
	 * タグ一覧のページはタグを定期には取り直さず、画面の操作（保存など）の後の `reload()` で
	 * 取り直す。ここでは収集グループの再設定を変更なしで保存して、その `reload()` を起こす
	 * （別のクライアントの変更を画面が知る、実際の経路）。
	 */
	async function reloadRegistry(): Promise<void> {
		await groupNodeByName(page, GROUP_NAME).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '収集グループを再設定', exact: true }).click();
		const drawer = page.getByRole('dialog', { name: new RegExp(GROUP_NAME) });
		await expect(drawer).toBeVisible();
		const saved = page.waitForResponse(
			(r) => r.url().includes('/api/collection-groups/') && r.request().method() === 'PUT'
		);
		await drawer.getByRole('button', { name: '保存', exact: true }).click();
		expect((await saved).ok()).toBe(true);
		await page.keyboard.press('Escape');
		await expect(drawer).toBeHidden();
	}

	test('開いたまま別のクライアントがコピー先に同じアドレスのタグを足す・消すと、衝突の表示が変わる', async () => {
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		const toggle = page.getByTestId('tag-selection-mode-toggle');
		if ((await toggle.textContent())?.includes('複数選択を終了') !== true) {
			await toggle.click();
		}
		await page.getByRole('gridcell', { name: SOURCE_TAG, exact: true }).click();
		await expect(page.getByTestId('tag-bulk-bar')).toContainText('選択 1 件');
		await page.getByTestId('tag-bulk-offset-copy-open').click();
		await page.getByTestId('tag-bulk-offset-copy-words').fill('100');

		// D5000 +100 = D5100: 空いているので衝突なし、検証できる。
		const rows = page.locator('[data-testid="tag-bulk-offset-copy-preview-table"] tbody tr');
		await expect(rows).toHaveCount(1);
		await expect(rows.first()).toContainText('D5100');
		await expect(page.getByTestId('tag-bulk-offset-copy-errors')).toHaveCount(0);
		await expect(page.getByTestId('tag-bulk-offset-copy-validate')).toBeEnabled();

		// 別のクライアントが D5100 にタグを足す（画面は開いたまま）→ 衝突が出て検証・実行が止まる。
		const blockerId = await createTag('e2eocfblocker', 'D5100');
		await reloadRegistry();
		await expect(page.getByTestId('tag-bulk-offset-copy-errors')).toBeVisible();
		await expect(page.getByTestId('tag-bulk-offset-copy-validate')).toBeDisabled();
		await expect(page.getByTestId('tag-bulk-offset-copy-apply')).toBeDisabled();

		// 消すと衝突が消え、プレビューと検証が戻る。
		const del = await page.request.delete(`/api/tags/${blockerId}`, { headers: authedHeaders });
		expect(del.ok()).toBe(true);
		await reloadRegistry();
		await expect(page.getByTestId('tag-bulk-offset-copy-errors')).toHaveCount(0);
		await expect(rows).toHaveCount(1);
		await expect(page.getByTestId('tag-bulk-offset-copy-validate')).toBeEnabled();
	});
});
