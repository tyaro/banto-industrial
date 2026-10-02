/**
 * #445: ストリームが切れている間にセッションが失効したら、再接続は認証で
 * 拒否されるが、ブラウザには `1006` しか見えない。画面は再接続が続けて失敗
 * したらログイン状態を確かめ、失効ならログイン画面へ移って再接続をやめる。
 * 通常の一時的な切断では、ログイン画面へ移らず、確認も呼びすぎない。
 *
 * `chromium-locked-down` プロジェクト（`banto-hub.playwright.config.ts`）
 * 専用 - セッションで開くストリーム（`/api/v1/stream`）はロックダウン済みで
 * しか使われない。
 *
 * `banto-hub-stream-injected.spec.ts`（#441）と同じ作法で、
 * `page.routeWebSocket` で `/api/v1/stream` を差し替える。接続ごとに扱いを
 * 切り替える:
 * - `mock`: 実サーバーへは繋がず、開いた接続として振る舞う（最初の接続と、
 *   一時的な切断からの回復）。
 * - `reject`: 開く前に `1006` で閉じる（ネットワークの一時的な切断で、
 *   再接続のハンドシェイクが通らない状態）。
 * - `server`: 実サーバーへ繋ぐ（`connectToServer()`）。失効したセッションは
 *   サーバーがハンドシェイクを `401` で拒否し、ブラウザには `1006` になる。
 *
 * 失効は実サーバーで起こす（管理者が閲覧者のパスワードをリセットする）。
 * ログイン状態の確認も実サーバー。
 *
 * banto v2.0.0（#260）から、確認は SessionController の `GET /api/auth/identity`
 * の 1 往復（v1 は画面が自分で `/api/auth/check` を呼んでいた）。数えるのは
 * identity の要求。期待の変化:
 * - 有効と**確定**したら、確認はそれで終わる（切断後に 1 回。テスト 2）。
 * - 照合できない（`unverified`）間は、画面を保って再接続を続け、controller が
 *   背景で確認を続ける（1 秒から倍々で 30 秒まで）ので、確認は「1 回」では
 *   終わらない（テスト 4）。ログイン画面にもエラー画面にも移らない。
 * - 拒否（失効の確定）なら /login（テスト 1・3）。
 */
import {
	expect,
	test,
	type Browser,
	type BrowserContext,
	type Page,
	type WebSocketRoute
} from '@playwright/test';
import { CSRF_HEADERS, TOKEN_STORAGE_KEY, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const USER_PREFIX = 'e2e-stream-reject-';
const VIEWER_PASSWORD = 'E2eStreamReject1';
/**
 * 失効に気づくまでの上限: 再接続の 2 回の失敗（1 秒 + 2 秒の待ち）+ 確認 +
 * ルートガード + 画面の移動。15 秒周期の再検証（#441）を待たないことも見る。
 */
const DETECT_WITHIN_MS = 10_000;
/** 再接続の待ち（4 秒・8 秒）より長く待って、接続も確認も増えないことを見る。 */
const LONGER_THAN_BACKOFF_MS = 9_000;

type StreamMode = 'mock' | 'reject' | 'server';

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
	.serial('banto-hub ストリームが切れている間の失効（ロックダウン済み専用サーバー）', () => {
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

	test.afterEach(async () => {
		await viewerContext?.close();
		viewerContext = null;
	});

	test.afterAll(async () => {
		await deleteLeftoverUsers(adminPage, adminHeaders);
		await adminPage.close();
	});

	/**
	 * 閲覧者を作ってログインし、`/api/v1/stream` を差し替えたページで
	 * モニタを開く（最初の接続は `mock` で開き「接続中」になる）。
	 */
	async function openViewerMonitor(browser: Browser) {
		const username = `${USER_PREFIX}${Date.now()}`;
		const createRes = await adminPage.request.post('/api/users', {
			headers: adminHeaders,
			data: {
				username,
				password: VIEWER_PASSWORD,
				displayName: 'E2E 切断中の失効',
				role: 'viewer'
			}
		});
		expect(createRes.ok(), await createRes.text()).toBe(true);
		const viewer = (await createRes.json()) as UserSummary;

		viewerContext = await browser.newContext();
		const page = await viewerContext.newPage();

		const state = {
			mode: 'mock' as StreamMode,
			/** 開かれた接続ごとの扱い。 */
			attempts: [] as StreamMode[],
			mocks: [] as WebSocketRoute[],
			/** `/api/auth/identity` の呼び出し時刻（ログイン状態の確認、v2 の controller の 1 往復）。 */
			identityChecks: [] as number[]
		};
		page.on('request', (request) => {
			if (new URL(request.url()).pathname === '/api/auth/identity') {
				state.identityChecks.push(Date.now());
			}
		});
		await page.routeWebSocket(/\/api\/v1\/stream$/, async (ws) => {
			const mode = state.mode;
			state.attempts.push(mode);
			if (mode === 'mock') {
				state.mocks.push(ws);
			} else if (mode === 'reject') {
				// 開く前に閉じる（ハンドシェイクが通らなかったのと同じく `open` は来ない）。
				await ws.close({ code: 1006, reason: '' });
			} else {
				ws.connectToServer();
			}
		});

		await page.goto('/login');
		const loginRes = await page.request.post('/api/auth/login', {
			headers: CSRF_HEADERS,
			data: { username, password: VIEWER_PASSWORD }
		});
		const login = (await loginRes.json()) as { success: boolean; token?: string };
		expect(login.success && login.token, JSON.stringify(login)).toBeTruthy();
		await injectAuthToken(page, login.token as string);

		await page.goto('/monitor');
		const status = page.getByTestId('monitor-stream-status');
		await expect(status).toContainText('接続中（リアルタイム更新中）');
		expect(state.attempts).toEqual(['mock']);
		return { page, state, viewer, status };
	}

	test('1. 切れている間にパスワードのリセットで失効したら、再接続の失敗から数秒でログイン画面へ移り、再接続をやめる', async ({
		browser
	}) => {
		test.setTimeout(60_000);
		const { page, state, viewer } = await openViewerMonitor(browser);

		// ストリームを切る（再接続のハンドシェイクは当面通らない）。
		state.mode = 'reject';
		const cutAt = Date.now();
		await state.mocks[0].close({ code: 1001, reason: '' });

		// 切れている間に失効させる。以後の再接続は実サーバーが認証で拒否する。
		const resetRes = await adminPage.request.post(`/api/users/${viewer.id}/reset-password`, {
			headers: adminHeaders,
			data: { newPassword: `${VIEWER_PASSWORD}x` }
		});
		expect(resetRes.ok(), await resetRes.text()).toBe(true);
		state.mode = 'server';

		await expect(page).toHaveURL(/\/login$/, { timeout: DETECT_WITHIN_MS });
		const detectedMs = Date.now() - cutAt;
		test.info().annotations.push({
			type: 'detected-after-ms',
			description: String(detectedMs)
		});
		// 最初の接続 + 失敗した再接続 2 回（うち実サーバーの拒否が 1 回以上）で止まる。
		expect(state.attempts.slice(1)).toHaveLength(2);
		expect(state.attempts).toContain('server');

		const attemptsAtLogin = state.attempts.length;
		await page.waitForTimeout(LONGER_THAN_BACKOFF_MS);
		expect(state.attempts).toHaveLength(attemptsAtLogin);
		await expect(page).toHaveURL(/\/login$/);
	});

	test('2. 通常の一時的な切断（再接続が 2 回拒否された後に戻る）では、ログイン画面へ移らず、確認は identity の 1 回だけ（有効と確定したら背景の確認は起きない）', async ({
		browser
	}) => {
		test.setTimeout(60_000);
		const { page, state, status } = await openViewerMonitor(browser);
		const checksBeforeCut = state.identityChecks.length;

		state.mode = 'reject';
		await state.mocks[0].close({ code: 1001, reason: '' });
		await expect(status).toContainText('再接続中');
		await expect(status).not.toContainText('接続中（');

		// 2 回拒否されたら（1 秒 + 2 秒）確認（identity）が 1 回走り、有効と確定したので
		// 再接続を続ける。
		await expect.poll(() => state.attempts.length, { timeout: 10_000 }).toBe(3);
		state.mode = 'mock';
		await expect.poll(() => state.identityChecks.length - checksBeforeCut).toBe(1);
		await expect(status).not.toContainText('接続中（');

		// 次の再接続（4 秒後）で戻る。
		await expect(status).toContainText('接続中（リアルタイム更新中）', { timeout: 10_000 });
		expect(state.attempts).toEqual(['mock', 'reject', 'reject', 'mock']);
		await expect(page).toHaveURL(/\/monitor$/);

		await page.waitForTimeout(LONGER_THAN_BACKOFF_MS);
		expect(state.identityChecks.length - checksBeforeCut).toBe(1);
		expect(state.attempts).toHaveLength(4);
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test('3. 切れている間に保存しているトークンが消えたら（ほかの経路の 401・ほかのタブのログアウト）、待ち続けずにログイン画面へ移る（#447 のレビュー）', async ({
		browser
	}) => {
		test.setTimeout(60_000);
		const { page, state } = await openViewerMonitor(browser);

		state.mode = 'reject';
		await state.mocks[0].close({ code: 1001, reason: '' });
		await page.evaluate((key) => {
			sessionStorage.removeItem(key);
			localStorage.removeItem(key);
		}, TOKEN_STORAGE_KEY);

		// 次の再接続（1 秒後）でトークンが無いと分かり、ルートガードが /login へ送る。
		await expect(page).toHaveURL(/\/login$/, { timeout: DETECT_WITHIN_MS });
		// ソケットは作らない（トークンが無いので）。
		expect(state.attempts).toEqual(['mock']);
	});

	test('4. 照合できない（identity が 500）間は、画面を保って再接続を続け、確認は背景でも続く（1 回では終わらない）。戻れば通常の表示へ', async ({
		browser
	}) => {
		test.setTimeout(90_000);
		const { page, state, status } = await openViewerMonitor(browser);
		// 画面を開いた後で照合だけを失敗させる（開くときのガードは通す）。
		await page.route('**/api/auth/identity', (route) =>
			route.fulfill({
				status: 500,
				contentType: 'application/json',
				body: JSON.stringify({ kind: 'storage', message: 'database is locked' })
			})
		);
		try {
			const checksBeforeCut = state.identityChecks.length;
			state.mode = 'reject';
			await state.mocks[0].close({ code: 1001, reason: '' });

			// 2 回拒否された後の確認は unverified。v1 はここで確認が止まったが、v2 は
			// controller が背景で確かめ続ける（1 秒・2 秒…）ので 2 回以上になる。
			await expect
				.poll(() => state.identityChecks.length - checksBeforeCut, { timeout: 15_000 })
				.toBeGreaterThanOrEqual(2);
			// 画面は保つ: ログイン画面にもエラー画面（503）にも移らず、再接続を続ける。
			await expect(page).toHaveURL(/\/monitor$/);
			await expect(page.getByRole('button', { name: '再試行' })).toHaveCount(0);
			await expect(status).toContainText('再接続中');
			await expect.poll(() => state.attempts.length, { timeout: 10_000 }).toBeGreaterThanOrEqual(3);
		} finally {
			await page.unroute('**/api/auth/identity');
		}

		// サーバーが戻れば、次の再接続で通常の表示へ戻る（ログインはそのまま）。
		state.mode = 'mock';
		await expect(status).toContainText('接続中（リアルタイム更新中）', { timeout: 30_000 });
		await expect(page).toHaveURL(/\/monitor$/);
		expect(
			await page.evaluate((key) => window.sessionStorage.getItem(key), TOKEN_STORAGE_KEY)
		).not.toBeNull();
	});
});
