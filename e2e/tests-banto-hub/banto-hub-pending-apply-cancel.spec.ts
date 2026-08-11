/**
 * TAG-P0-3: pending apply/cancel 導線の実 DOM 固定。
 *
 * - 収集中に構成変更を送ると pending へ積まれる
 * - status 画面の Pending changes からキャンセル/適用できる
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-pending-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-pending-group-${RUN_ID}`;

test.describe.serial('banto-hub pending apply/cancel', () => {
	let page: Page | undefined;
	let token = '';
	let groupId = 0;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				host: '127.0.0.1',
				port: 15022
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: {
				name: GROUP_NAME,
				plcConnectionId: connection.id,
				periodMs: 100
			}
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };
		groupId = group.id;
	});

	test.afterAll(async () => {
		if (page) {
			await page.close();
		}
	});

	async function queueTagWhileRunning(tagName: string): Promise<number> {
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		const startRes = await page.request.post('/api/collection/start-all-simulation', {
			headers: authedHeaders
		});
		expect(startRes.ok()).toBe(true);
		const startStatus = (await startRes.json()) as {
			state?: string;
			mode?: string;
		};
		expect(startStatus.state).toBe('running');
		expect(startStatus.mode).toBe('all_simulation');

		const queuedRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: tagName,
				collectionGroupId: groupId,
				address: '40001',
				dataType: 'i16'
			}
		});
		const queuedBody = await queuedRes.text();
		expect(
			queuedRes.status(),
			`running中のtags createは pending へ積まれる (body=${queuedBody})`
		).toBe(202);
		const queued = JSON.parse(queuedBody) as {
			pending: { id: number };
		};
		expect(typeof queued.pending.id).toBe('number');
		return queued.pending.id;
	}

	test('1. Pending changes 画面からキャンセルできる', async () => {
		const pendingId = await queueTagWhileRunning(`e2e-pending-cancel-${RUN_ID}`);

		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'Pending changes' })).toBeVisible();

		const row = page.locator('tbody tr', { hasText: `#${pendingId}` });
		await expect(row).toBeVisible();
		await row.getByRole('button', { name: 'キャンセル' }).click();

		await expect(row).toContainText('キャンセル済み');
		await expect(row.getByRole('button', { name: 'キャンセル' })).toHaveCount(0);

		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		const stopRes = await page.request.post('/api/collection/stop', { headers: authedHeaders });
		expect(stopRes.ok()).toBe(true);
	});

	test('2. Pending changes 画面から適用できる（停止中）', async () => {
		const pendingId = await queueTagWhileRunning(`e2e-pending-apply-${RUN_ID}`);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		const stopRes = await page.request.post('/api/collection/stop', { headers: authedHeaders });
		expect(stopRes.ok()).toBe(true);

		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'Pending changes' })).toBeVisible();

		const row = page.locator('tbody tr', { hasText: `#${pendingId}` });
		await expect(row).toBeVisible();
		await row.getByRole('button', { name: '適用' }).click();

		await expect(row).toContainText('適用済み');
		await expect(row.getByRole('button', { name: '適用' })).toHaveCount(0);

		const tagsRes = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(tagsRes.ok()).toBe(true);
		const tags = (await tagsRes.json()) as Array<{ name: string }>;
		expect(tags.some((tag) => tag.name === `e2e-pending-apply-${RUN_ID}`)).toBe(true);
	});
});
