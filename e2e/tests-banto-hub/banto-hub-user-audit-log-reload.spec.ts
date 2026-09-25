/**
 * banto-hub の `/audit-log`（監査ログ一覧）の**回復導線**の実 DOM 固定（#428）。
 * chronogazer の `e2e/tests/user-audit-log-reload.spec.ts`（#410）と同じ 2 本。
 *
 * 修正前は「再読み込み」ボタンが無く、総件数が 0 のあいだ `BantoGrid` が
 * 通知する表示範囲 `{0, 0}` では要求が 1 本も出なかった。そのため
 * **絞り込みで 0 件になった後に絞り込みを外しても 0 件のまま**、一覧を開いた
 * 後に記録された操作も画面からは取り込めなかった。ブロックキャッシュの判断は
 * `apps/banto-hub/src/routes/(app)/audit-log/auditBlocks.test.ts`（vitest）が
 * 固定しているが、ボタンが**出ているか**と、`BantoGrid` が実際に `{0, 0}` を
 * 通知する経路は、画面を操作しないと確かめられない。
 *
 * **スタブを使わない**: 監査ログは実サーバーで本当に増やせる（ログインの失敗も
 * `login_failed` として記録される - `rest.rs` の `audited_credential_verifier`）。
 *
 * ファイル名について: `banto-hub-smoke.spec.ts` の最初のテストが初回セット
 * アップを行うので、それより後の名前でなければならない（`workers: 1`/
 * `fullyParallel: false` でファイル名の辞書順に実行する）。試運転モードの
 * 1 台目（`chromium` プロジェクト）で走る。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, ensureLoggedIn, HUB_ADMIN_USERNAME } from './banto-hub-auth';

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
		data: { username: HUB_ADMIN_USERNAME, password: 'wrong-password-for-audit-e2e' }
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

test.describe.serial('banto-hub 監査ログ一覧の回復導線（#428）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		await ensureLoggedIn(page);
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
