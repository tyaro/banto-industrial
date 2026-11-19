/**
 * #1161 PR4b: ChronoGazer の API キー画面（`/api-keys`、設計書 §3.4・§7）の実 DOM 受け入れテスト。
 * 管理者がメニューバーの「管理 ▾」から API キーへ移り、発行（平文のキーはその場だけ）→ 一覧 →
 * そのキーで `Authorization: Bearer` の GET が CSRF のヘッダー無しで通る → 失効で 401 → 削除で
 * 一覧から消える。`read` のキーでは書き込みが 403。
 *
 * ファイル名: 管理者アカウントは `smoke.spec.ts` が作るので辞書順でそれより後（`s` < `u`）。
 * 後始末: 作ったキーは失効して削除する（途中で落ちても `afterAll` で残りを掃除する）。
 */
import { expect, test, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
const KEY_NAME = 'e2e-agent-read';

async function login(page: Page): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
	await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer API キー画面 (#1161 PR4b)', () => {
	let page: Page;
	let plainKey = '';

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
		await login(page);
	});

	test.afterAll(async () => {
		// 途中で落ちたときの掃除: セッションの token で残りのキーを失効して削除する。
		try {
			const res = await page.request.post('/api/auth/login', {
				headers: CSRF_HEADERS,
				data: { username: ADMIN_USERNAME, password: ADMIN_PASSWORD }
			});
			const { token } = (await res.json()) as { token?: string };
			if (token) {
				const headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
				const list = await page.request.get('/api/api-keys', { headers });
				const rows = (await list.json()) as { id: number; name: string }[];
				for (const row of rows.filter((r) => r.name === KEY_NAME)) {
					await page.request.post(`/api/api-keys/${row.id}/revoke`, { headers });
					await page.request.delete(`/api/api-keys/${row.id}`, { headers });
				}
			}
		} finally {
			await page.close();
		}
	});

	test('1. メニューバーの「管理」から API キーへ移れる', async () => {
		const nav = page.getByRole('banner').getByRole('navigation', { name: '主要ナビゲーション' });
		await nav.getByRole('button', { name: '管理' }).click();
		await nav.getByRole('link', { name: 'API キー' }).click();
		await expect(page).toHaveURL(/\/api-keys$/);
		await expect(page.getByRole('heading', { name: 'API キー', level: 2 })).toBeVisible();
	});

	test('2a. 注意（§7）: loopback 待受では出ず、LAN 待受（0.0.0.0）の HTTP では出る', async () => {
		const warning = page.getByTestId('plaintext-warning');
		await page.reload();
		await expect(page.getByRole('button', { name: '発行' })).toBeVisible();
		await expect(warning).toHaveCount(0);

		await page.route('**/api/settings/server', async (route) => {
			const response = await route.fetch();
			const body = (await response.json()) as Record<string, unknown>;
			await route.fulfill({ response, json: { ...body, bind: '0.0.0.0' } });
		});
		await page.reload();
		await expect(warning).toBeVisible();
		await expect(warning).toContainText('127.0.0.1');
		// ハンドラは route.fetch() を待つので、処理中に外して reload すると要求が打ち切られて
		// fulfill が「already handled」で落ちる（CI で再試行も含めて落ちた）。処理中のハンドラを待ってから外す。
		await page.unroute('**/api/settings/server', { behavior: 'wait' });
		await page.reload();
		await expect(warning).toHaveCount(0);
	});

	test('2. 発行すると平文のキーがその場だけ出て、一覧に載る', async () => {
		await page.getByLabel('名前').fill(KEY_NAME);
		await page.getByRole('button', { name: '発行' }).click();
		const secret = page.getByTestId('issued-key-value');
		await expect(secret).toBeVisible();
		await expect(page.getByTestId('issued-key')).toContainText('二度と表示できません');
		plainKey = (await secret.textContent())?.trim() ?? '';
		expect(plainKey.length).toBeGreaterThan(20);

		const row = page.getByTestId('api-key-row').filter({ hasText: KEY_NAME });
		await expect(row).toHaveCount(1);
		await expect(row).toContainText('read');
		await expect(row).toContainText('有効');
		// 一覧には平文のキー全体は出ない（prefix だけ）。
		await expect(row).not.toContainText(plainKey);
	});

	test('3. そのキーで CSRF のヘッダー無しに GET でき、read では書き込めない（403）', async ({
		playwright,
		baseURL
	}) => {
		const ctx = await playwright.request.newContext({
			baseURL,
			extraHTTPHeaders: { Authorization: `Bearer ${plainKey}` }
		});
		try {
			const ok = await ctx.get('/api/tags');
			expect(ok.status()).toBe(200);
			const denied = await ctx.post('/api/collect/stop');
			expect(denied.status()).toBe(403);
			// 管理系（API キー自身）はキーでは通らない。
			const admin = await ctx.get('/api/api-keys');
			expect([401, 403]).toContain(admin.status());
		} finally {
			await ctx.dispose();
		}
	});

	test('4. 失効すると同じキーが 401 になり、行は「失効」で削除ボタンに変わる', async ({
		playwright,
		baseURL
	}) => {
		const row = page.getByTestId('api-key-row').filter({ hasText: KEY_NAME });
		page.once('dialog', (dialog) => void dialog.accept());
		await row.getByRole('button', { name: '失効' }).click();
		await expect(row).toContainText('失効');
		await expect(row.getByRole('button', { name: '削除' })).toBeVisible();

		const ctx = await playwright.request.newContext({
			baseURL,
			extraHTTPHeaders: { Authorization: `Bearer ${plainKey}` }
		});
		try {
			expect((await ctx.get('/api/tags')).status()).toBe(401);
		} finally {
			await ctx.dispose();
		}
	});

	test('5. 削除すると一覧から消える', async () => {
		const row = page.getByTestId('api-key-row').filter({ hasText: KEY_NAME });
		page.once('dialog', (dialog) => void dialog.accept());
		await row.getByRole('button', { name: '削除' }).click();
		await expect(row).toHaveCount(0);
	});
});
