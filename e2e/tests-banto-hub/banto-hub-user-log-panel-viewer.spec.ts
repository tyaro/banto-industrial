/**
 * #1253: 下のログのパネルの見せる範囲（ロックダウン済み専用サーバー、`chromium-locked-down`）。
 *
 * 管理者でない利用者（閲覧者）には、口（`GET /api/log-feed`）が接続の行しか返さない
 * （サーバーが絞る。画面で隠すのではない）。画面は届いた種類に合わせてトグルを出すので、
 * 操作・書き込みのトグルも行も出ない。同じサーバーの管理者には操作の監査の行が出る
 * （比べるため）。試運転中は閲覧者でも権限の差が出ないので、ロックダウン済みで見る
 * （`banto-hub-user-session-ended.spec.ts` と同じ手順。lock_down() は冪等）。
 */
import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const USER_PREFIX = 'e2e-log-viewer-';
const VIEWER_PASSWORD = 'E2eLogViewer1';
const FEED_PATH = '/api/log-feed';
const ONE_INTERVAL_MS = 12_000;

interface UserSummary {
	id: number;
	username: string;
}

async function deleteLeftoverUsers(page: Page, headers: Record<string, string>): Promise<void> {
	const res = await page.request.get('/api/users', { headers });
	expect(res.ok(), await res.text()).toBe(true);
	const users = (await res.json()) as UserSummary[];
	for (const user of users.filter((u) => u.username.startsWith(USER_PREFIX))) {
		await page.request.delete(`/api/users/${user.id}`, { headers });
	}
}

test.describe
	.serial('banto-hub 下のログのパネルの見せる範囲（ロックダウン済み専用サーバー）', () => {
	let adminPage: Page;
	let adminHeaders: Record<string, string> = {};
	let viewerContext: BrowserContext | null = null;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage({ viewport: { width: 1280, height: 800 } });
		await adminPage.goto('/login');
		let token = await fetchAuthToken(adminPage.request);
		const lockDownRes = await adminPage.request.post('/api/commissioning/lock-down', {
			headers: { ...CSRF_HEADERS, Authorization: `Bearer ${token}` }
		});
		expect(lockDownRes.ok(), await lockDownRes.text()).toBe(true);
		token = await fetchAuthToken(adminPage.request);
		await injectAuthToken(adminPage, token);
		adminHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await deleteLeftoverUsers(adminPage, adminHeaders);
	});

	test.afterAll(async () => {
		await viewerContext?.close();
		await deleteLeftoverUsers(adminPage, adminHeaders);
		await adminPage.close();
	});

	test('1. 管理者には操作の監査の行が出る', async () => {
		await adminPage.goto('/monitor');
		const panel = adminPage.getByTestId('log-panel');
		await expect(panel).toBeVisible();
		await expect(
			panel.locator('[data-testid="log-row"][data-source="operation"]').first()
		).toBeVisible({ timeout: ONE_INTERVAL_MS });
		await expect(panel.getByTestId('log-filter-operation')).toBeVisible();
	});

	test('2. 閲覧者には操作・書き込みの行もトグルも出ない（口が接続の行だけを返す）', async ({
		browser
	}) => {
		const username = `${USER_PREFIX}${Date.now()}`;
		const createRes = await adminPage.request.post('/api/users', {
			headers: adminHeaders,
			data: { username, password: VIEWER_PASSWORD, displayName: 'E2E ログの閲覧者', role: 'viewer' }
		});
		expect(createRes.ok(), await createRes.text()).toBe(true);

		viewerContext = await browser.newContext({ viewport: { width: 1280, height: 800 } });
		const viewerPage = await viewerContext.newPage();
		await viewerPage.goto('/login');
		const loginRes = await viewerPage.request.post('/api/auth/login', {
			headers: CSRF_HEADERS,
			data: { username, password: VIEWER_PASSWORD }
		});
		const login = (await loginRes.json()) as { success: boolean; token?: string };
		expect(login.success && login.token, JSON.stringify(login)).toBeTruthy();
		await injectAuthToken(viewerPage, login.token as string);

		const feed = viewerPage.waitForResponse(
			(r) => new URL(r.url()).pathname === FEED_PATH && r.request().method() === 'GET'
		);
		await viewerPage.goto('/monitor');
		const response = await feed;
		expect(response.status()).toBe(200);
		const body = (await response.json()) as { entries: { source: string }[] };
		expect(body.entries.filter((e) => e.source !== 'connection')).toEqual([]);

		const panel = viewerPage.getByTestId('log-panel');
		await expect(panel).toBeVisible();
		await expect(panel.locator('[data-source="operation"], [data-source="write"]')).toHaveCount(0);
		await expect(panel.getByTestId('log-filter-operation')).toHaveCount(0);
		await expect(panel.getByTestId('log-filter-write')).toHaveCount(0);
	});
});
