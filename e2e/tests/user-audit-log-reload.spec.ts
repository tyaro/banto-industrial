/**
 * `/audit-log`（監査ログ一覧）の**回復導線**の実 DOM 固定（#410）。
 *
 * 修正前は「再読み込み」ボタンが無く、総件数が 0 のあいだ `BantoGrid` が
 * 通知する表示範囲 `{0, 0}` では要求が 1 本も出なかった。そのため
 * **絞り込みで 0 件になった後に絞り込みを外しても 0 件のまま**、一覧を開いた
 * 後に記録された操作も画面からは取り込めなかった。ブロックキャッシュの判断は
 * `auditBlocks.test.ts`（vitest）が固定しているが、ボタンが**出ているか**と、
 * `BantoGrid` が実際に `{0, 0}` を通知する経路は、画面を操作しないと確かめ
 * られない（このリポジトリには DOM テストの足場が無く、Playwright が等価物 -
 * `user-events-reload.spec.ts` と同じ判断）。
 *
 * **スタブを使わない**: 監査ログは実サーバーで本当に増やせる（ログインの失敗も
 * 記録される）ので、`/events` の #409 P2-4 と違って `page.route` は要らない。
 *
 * ファイル名について: `smoke.spec.ts` の最初のテストが初回セットアップ（管理者
 * アカウント作成）を行うので、それより後の名前でなければならない
 * （`workers: 1`/`fullyParallel: false` でファイル名の辞書順に実行する）。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

/** 一覧の上の「N件の記録があります。」から N を読む（読めるまで待つ）。 */
async function recordCount(page: Page): Promise<number> {
	const note = page.getByText(/^[\d,]+件の記録があります。/);
	await expect(note).toBeVisible();
	const text = (await note.textContent()) ?? '';
	return Number(text.replace(/件の記録があります。.*$/s, '').replace(/,/g, ''));
}

/** 監査ログに 1 行足す（ログインの失敗は `login_failed` として記録される）。 */
async function recordFailedLogin(page: Page): Promise<void> {
	const response = await page.request.post('/api/auth/login', {
		headers: CSRF_HEADERS,
		data: { username: ADMIN_USERNAME, password: 'wrong-password-for-audit-e2e' }
	});
	expect(response.ok()).toBe(true);
	expect((await response.json()).success).toBe(false);
}

async function setActionFilter(page: Page, value: string | null): Promise<void> {
	await page.getByRole('button', { name: 'アクションの絞り込み' }).click();
	const popover = page.getByRole('dialog', { name: 'アクションの絞り込み' });
	if (value === null) {
		await popover.getByRole('button', { name: 'クリア' }).click();
		return;
	}
	await popover.getByPlaceholder('値を入力').fill(value);
	await popover.getByRole('button', { name: '適用' }).click();
}

test.describe.serial('chronogazer 監査ログ一覧の回復導線（#410）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 正常に読めた後も「再読み込み」が出て、押すと後から記録された操作が入る', async () => {
		await page.goto('/audit-log');
		const before = await recordCount(page);
		expect(before).toBeGreaterThan(0);

		const reload = page.getByRole('button', { name: '再読み込み' });
		await expect(reload).toBeVisible();
		await expect(reload).toBeEnabled();

		await recordFailedLogin(page);
		// 境界（asOfId）を固定しているので、押すまでは入らない。
		expect(await recordCount(page)).toBe(before);

		await reload.click();
		await expect(
			page.getByText(`${(before + 1).toLocaleString()}件の記録があります。`)
		).toBeVisible();
		await expect(page.getByRole('row').filter({ hasText: 'ログイン失敗' }).first()).toBeVisible();
	});

	test('2. 絞り込みで 0 件になった後に絞り込みを外すと、要求が出て件数が戻る', async () => {
		await page.goto('/audit-log');
		const all = await recordCount(page);

		await setActionFilter(page, 'no-such-action-for-audit-e2e');
		await expect(page.getByText(/^0件の記録があります。/)).toBeVisible();

		await setActionFilter(page, null);
		await expect(page.getByText(`${all.toLocaleString()}件の記録があります。`)).toBeVisible();
		await expect(page.getByRole('row').filter({ hasText: 'ログイン失敗' }).first()).toBeVisible();
	});
});
