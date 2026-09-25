/**
 * #441: 開いていたタグモニタのストリームがセッションの失効で閉じられたら、
 * 画面はログイン画面へ移る（再接続を繰り返さない）。
 *
 * `chromium-locked-down` プロジェクト（`banto-hub.playwright.config.ts`）
 * 専用 - セッションで開くストリーム（`/api/v1/stream`）はロックダウン済みで
 * しか使われない。
 *
 * 流れ: 管理者が閲覧者を作る → 閲覧者の別のブラウザコンテキストでモニタを
 * 開き「接続中」を待つ → 管理者が閲覧者のパスワードをリセットする（閲覧者の
 * セッションが失効する）→ サーバーの再検証（`stream.rs` の
 * `REVALIDATE_INTERVAL` = 15 秒。短くする手段は無い）が close `1008` +
 * `session_revoked` で閉じる → 画面がルートガードを走らせ直して `/login`
 * へ移る。修正前は通常の切断として再接続を繰り返し、`/monitor` に残る。
 *
 * 再接続が起きていないことも見る: 閲覧者のページで開かれた WebSocket は
 * 最初の 1 本だけ。
 */
import { expect, test, type BrowserContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const USER_PREFIX = 'e2e-stream-revoke-';
const VIEWER_PASSWORD = 'E2eStreamRevoke1';
/** 再検証の周期（15 秒）+ 照合と画面の移動の余裕。 */
const ONE_INTERVAL_WITH_MARGIN_MS = 25_000;

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

test.describe.serial('banto-hub ストリームの失効（ロックダウン済み専用サーバー）', () => {
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

	test('1. パスワードのリセットで失効したら、開いていたモニタは 1 周期以内にログイン画面へ移り、再接続しない', async ({
		browser
	}) => {
		test.setTimeout(ONE_INTERVAL_WITH_MARGIN_MS + 30_000);
		const username = `${USER_PREFIX}${Date.now()}`;
		const createRes = await adminPage.request.post('/api/users', {
			headers: adminHeaders,
			data: {
				username,
				password: VIEWER_PASSWORD,
				displayName: 'E2E 失効確認',
				role: 'viewer'
			}
		});
		expect(createRes.ok(), await createRes.text()).toBe(true);
		const viewer = (await createRes.json()) as UserSummary;

		viewerContext = await browser.newContext();
		const viewerPage = await viewerContext.newPage();
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

		await viewerPage.goto('/monitor');
		await expect(viewerPage.getByTestId('monitor-stream-status')).toContainText('接続中', {
			timeout: 15_000
		});
		expect(sockets).toHaveLength(1);
		expect(sockets[0]).toContain('/api/v1/stream');

		const resetRes = await adminPage.request.post(`/api/users/${viewer.id}/reset-password`, {
			headers: adminHeaders,
			data: { newPassword: `${VIEWER_PASSWORD}x` }
		});
		expect(resetRes.ok(), await resetRes.text()).toBe(true);

		await expect(viewerPage).toHaveURL(/\/login$/, { timeout: ONE_INTERVAL_WITH_MARGIN_MS });
		// トークンが消えたかは見ない: サーバーは失効したセッションの
		// `/api/auth/check` に `200 false` を返し、`@banto/admin-core` は
		// `401` のときだけトークンを消す（画面を開いたときのガードと同じ、
		// #436 からの挙動）。次のログインで上書きされる。
		// 1008 の後に再接続していない（修正前は 1 秒・2 秒…のバックオフで
		// 張り直し、認証で拒否され続けた）。
		expect(sockets).toHaveLength(1);
	});
});
