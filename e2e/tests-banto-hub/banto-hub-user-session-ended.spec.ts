/**
 * banto v1.7.2（tyaro/banto#241）: タグモニタ以外の画面を開いているときに
 * セッションが失効したら、画面はログイン画面へ移る。
 *
 * タグモニタは自分のストリーム（#441 の close `1008`、`banto-hub-stream-
 * revoked.spec.ts`）で失効に気づくが、ほかの画面にはそれが無い。気づくのは
 * `@banto/admin-core` の SSE（`/api/events`）で、`(app)/+layout.svelte` の
 * `onSessionEnded(() => void invalidateAll())` がルートガードを走らせ直す。
 * この購読が無いと、トークンは消えても画面は次の遷移まで残る。
 *
 * `chromium-locked-down` プロジェクト専用（試運転モードではトークンを
 * 使わないので、SSE は失効を知らせない）。
 *
 * 流れ: 管理者が閲覧者を作る → 閲覧者の別のブラウザコンテキストで
 * `/tags` を開き、SSE が繋がるのを待つ → 管理者が閲覧者のパスワードを
 * リセットする → banto-server の SSE の再検証（`REVALIDATE_INTERVAL` =
 * 15 秒）がストリームを閉じ、再接続が `401` → `check()` で確認（トークンを
 * 消す）→ `onSessionEnded` → `/login`。
 */
import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken, TOKEN_STORAGE_KEY } from './banto-hub-auth';

const USER_PREFIX = 'e2e-session-ended-';
const VIEWER_PASSWORD = 'E2eSessionEnded1';
/** SSE の再検証（15 秒）+ 再接続の待ち（3 秒）+ 確認と画面の移動の余裕。 */
const ONE_INTERVAL_WITH_MARGIN_MS = 30_000;
/** ログイン画面へ移った後、SSE が要求を送らないことを見る時間。 */
const QUIET_AFTER_LOGIN_MS = 5_000;

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
	.serial('banto-hub 画面を開いたままのセッション失効（ロックダウン済み専用サーバー）', () => {
	let adminPage: Page;
	let adminHeaders: Record<string, string> = {};
	let viewerContext: BrowserContext | null = null;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage();
		await adminPage.goto('/login');

		let token = await fetchAuthToken(adminPage.request);
		// ほかのロックダウン専用 spec と同じ手順（lock_down() は冪等）。
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

	test('1. パスワードのリセットで失効したら、開いていたタグ画面はログイン画面へ移り、トークンが消え、SSE は再試行しない', async ({
		browser
	}) => {
		test.setTimeout(ONE_INTERVAL_WITH_MARGIN_MS + QUIET_AFTER_LOGIN_MS + 30_000);
		const username = `${USER_PREFIX}${Date.now()}`;
		const createRes = await adminPage.request.post('/api/users', {
			headers: adminHeaders,
			data: {
				username,
				password: VIEWER_PASSWORD,
				displayName: 'E2E 画面の失効確認',
				role: 'viewer'
			}
		});
		expect(createRes.ok(), await createRes.text()).toBe(true);
		const viewer = (await createRes.json()) as UserSummary;

		viewerContext = await browser.newContext();
		const viewerPage = await viewerContext.newPage();
		// SSE の要求と応答の状態（`/api/events` は `fetch` で開く）。
		const eventRequests: number[] = [];
		const eventStatuses: number[] = [];
		viewerPage.on('request', (req) => {
			if (new URL(req.url()).pathname === '/api/events') eventRequests.push(Date.now());
		});
		viewerPage.on('response', (res) => {
			if (new URL(res.url()).pathname === '/api/events') eventStatuses.push(res.status());
		});
		// タグモニタのストリームで気づいたのではないことを確かめる。
		const sockets: string[] = [];
		viewerPage.on('websocket', (ws) => sockets.push(ws.url()));

		await viewerPage.goto('/login');
		const loginRes = await viewerPage.request.post('/api/auth/login', {
			headers: CSRF_HEADERS,
			data: { username, password: VIEWER_PASSWORD }
		});
		const login = (await loginRes.json()) as { success: boolean; token?: string };
		expect(login.success && login.token, JSON.stringify(login)).toBeTruthy();
		await injectAuthToken(viewerPage, login.token as string);

		await viewerPage.goto('/tags');
		await expect(viewerPage.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		// SSE がこのトークンで繋がった（`200`）。
		await expect.poll(() => eventStatuses.includes(200), { timeout: 15_000 }).toBe(true);

		const resetRes = await adminPage.request.post(`/api/users/${viewer.id}/reset-password`, {
			headers: adminHeaders,
			data: { newPassword: `${VIEWER_PASSWORD}x` }
		});
		expect(resetRes.ok(), await resetRes.text()).toBe(true);

		await expect(viewerPage).toHaveURL(/\/login$/, { timeout: ONE_INTERVAL_WITH_MARGIN_MS });
		// SSE の再接続が `401` で拒否された（ここで失効に気づいた）。
		expect(eventStatuses).toContain(401);
		expect(sockets).toHaveLength(0);
		// v1.7.2 の `check()` は `200 false` でもトークンを消す。
		expect(
			await viewerPage.evaluate((key) => window.sessionStorage.getItem(key), TOKEN_STORAGE_KEY)
		).toBeNull();

		// 失効したトークンで SSE を再試行し続けない（トークンが無いので
		// 次のログインまで要求しない）。
		const requestsAtLogin = eventRequests.length;
		await viewerPage.waitForTimeout(QUIET_AFTER_LOGIN_MS);
		expect(eventRequests.length).toBe(requestsAtLogin);
	});
});
