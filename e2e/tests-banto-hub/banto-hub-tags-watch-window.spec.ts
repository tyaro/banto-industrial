/**
 * #1254: 値を見張る窓（`/tags` のドックの枠の追加パネル `watch`）の実 DOM の受け入れテスト
 * （docs/hub/banto-hub-ui-improvement-proposal.md §値を見張る窓）。
 *
 * 1. 属性に出している行を「見張りに足す」で足すと窓が開き、完全な外部名の行が出る。
 * 2. 再読み込みの後も一覧が残る（利用者ごとにサーバーへ保存。`hub.watch.v1`）。
 * 3. 行の右クリックのメニュー「見張りに足す」で足せる。同じタグを重ねて足さない。
 * 4. 行ごとの「外す」・「すべて外す」で外せ、保存も空になる。
 *
 * 最初に保存した見張りの一覧と配置を消し、最後も消す（後の spec のため）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-watch-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-watch-group-${RUN_ID}`;
const TAG_A = `e2e-watch-a-${RUN_ID}`;
const TAG_B = `e2e-watch-b-${RUN_ID}`;
const NAME_A = `${CONNECTION_NAME}.${GROUP_NAME}.${TAG_A}`;
const NAME_B = `${CONNECTION_NAME}.${GROUP_NAME}.${TAG_B}`;
const WATCH_SETTING = '/api/ui-settings/hub.watch.v1';
const TAGS_LAYOUT_SETTING = '/api/ui-settings/hub.dock.v2.tags';
const WIDE = { width: 1920, height: 1080 };

test.describe.serial('banto-hub 値を見張る窓 (#1254)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let connectionId: number;
	let groupId: number;
	const tagIds: number[] = [];

	const panel = () => page.getByTestId('watch-panel');
	const watchRows = () => panel().getByTestId('watch-row');

	async function clearSaved(): Promise<void> {
		for (const key of [WATCH_SETTING, TAGS_LAYOUT_SETTING]) {
			const res = await page.request.put(key, { headers: authedHeaders, data: { value: '' } });
			expect(res.ok(), await res.text()).toBe(true);
		}
		await page.evaluate(() => {
			for (const key of Object.keys(localStorage)) {
				if (key.startsWith('banto.dock.v2.') || key.startsWith('banto.watch.v')) {
					localStorage.removeItem(key);
				}
			}
		});
	}

	async function savedWatchIds(): Promise<number[]> {
		const res = await page.request.get(WATCH_SETTING, { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		const body = (await res.json()) as { value?: string | null };
		if (!body.value) return [];
		return (JSON.parse(body.value) as { tags: number[] }).tags;
	}

	async function openWatchFromViewMenu(): Promise<void> {
		await page.getByTestId('tag-view-menu').click();
		await page.getByRole('menuitemcheckbox', { name: '見張り' }).click();
		await expect(panel()).toBeVisible();
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await clearSaved();

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connectionRes.ok()).toBe(true);
		connectionId = ((await connectionRes.json()) as { id: number }).id;
		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connectionId, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;
		for (const [name, address] of [
			[TAG_A, '40001'],
			[TAG_B, '40002']
		]) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: groupId,
					address,
					dataType: 'i16',
					unit: null,
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
			tagIds.push(((await tagRes.json()) as { id: number }).id);
		}
	});

	test.afterAll(async () => {
		await clearSaved();
		for (const id of tagIds)
			await page.request.delete(`/api/tags/${id}`, { headers: authedHeaders });
		await page.request.delete(`/api/collection-groups/${groupId}`, { headers: authedHeaders });
		await page.request.delete(`/api/plc-connections/${connectionId}`, { headers: authedHeaders });
		await page.close();
	});

	test('1. 属性に出している行を足すと窓が開き、完全な外部名の行が出る', async () => {
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(`e2e-watch-`);
		// 対象が無い間はボタンが無効。
		await expect(page.getByTestId('tag-watch-add')).toBeDisabled();
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		await expect(page.getByTestId('tag-watch-add')).toBeEnabled();
		await page.getByTestId('tag-watch-add').click();
		await expect(panel()).toBeVisible();
		await expect(watchRows()).toHaveCount(1);
		await expect(watchRows().first()).toContainText(NAME_A);
	});

	test('2. 再読み込みの後も一覧が残る', async () => {
		await expect.poll(savedWatchIds).toEqual([tagIds[0]]);
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		if (!(await panel().isVisible())) await openWatchFromViewMenu();
		await expect(watchRows()).toHaveCount(1);
		await expect(watchRows().first()).toContainText(NAME_A);
	});

	test('3. 行の右クリックのメニューで足せる。同じタグは重ねない', async () => {
		await page.getByPlaceholder('名前・アドレスで検索').fill(`e2e-watch-`);
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '見張りに足す' }).click();
		await expect(watchRows()).toHaveCount(2);
		await expect(panel()).toContainText(NAME_B);
		await expect.poll(savedWatchIds).toEqual([tagIds[0], tagIds[1]]);

		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '見張りに足す' }).click();
		await expect(watchRows()).toHaveCount(2);
	});

	test('4. 行ごとの「外す」と「すべて外す」で外せ、保存も空になる', async () => {
		await watchRows().filter({ hasText: NAME_A }).getByTestId('watch-remove').click();
		await expect(watchRows()).toHaveCount(1);
		await expect(panel()).not.toContainText(NAME_A);
		await expect.poll(savedWatchIds).toEqual([tagIds[1]]);

		await panel().getByTestId('watch-clear').click();
		await expect(watchRows()).toHaveCount(0);
		await expect(panel()).toContainText('見張っているタグはありません');
		await expect.poll(savedWatchIds).toEqual([]);
	});
});
