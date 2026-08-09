/**
 * タグ連続登録（T11-1）の実 DOM 受け入れテスト - TAG-P0-1
 * （docs/banto-hub-desktop-plan.md §9「TAG-P0-1: 連続登録の点数変更を
 * 修正する」）の受け入れ条件のうち、`continuousRegistration.test.ts`
 * （vitest、純関数の単体テスト）ではまだ確認できていない「実 DOM から
 * 点数を変更したときにプレビュー件数/エラー表示が正しく追従するか」を
 * 確認する。
 *
 * `banto-hub-smoke.spec.ts` とは別の `describe.serial` ブロック（別
 * `page`）- 同じ `webServer`/DB を共有するため、認証は
 * `banto-hub-auth.ts::ensureLoggedIn`（未初期化なら setup、初期化済みなら
 * login を自動判定）で済ませる。前提データ（PLC接続・収集グループ）は
 * UI操作ではなく `page.request` で直接 REST を叩いて作る - このテストの
 * 本題は連続登録 Drawer の DOM 挙動であって、PLC接続/収集グループ作成
 * フォーム自体は plc-connections/collection-groups 側のテストの責務
 * （ここでは前提データ作成の手数を減らして安定性を優先する）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-continuous-plc';
const GROUP_NAME = 'e2e-continuous-group';

test.describe.serial('banto-hub タグ連続登録 DOM (TAG-P0-1)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + その配下の収集
		// グループを1件ずつ作る（実 PLC/実ネットワークへは繋がない - タグ
		// 連続登録フォームが必要とするのは「選択できる収集グループが1件
		// 存在すること」だけ）。
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: {
				name: GROUP_NAME,
				plcConnectionId: connection.id,
				periodMs: 1000,
				enabled: true
			}
		});
		expect(groupRes.ok()).toBe(true);

		// 連続登録 Drawer を開き、点数以外の固定入力(対象グループ・開始
		// アドレス)を済ませておく - 各 test は「点数」欄だけを変えて
		// プレビュー/エラー表示を確認する。
		await page.goto('/tags');
		await page.getByRole('button', { name: '連続登録' }).click();
		await expect(page.getByRole('dialog', { name: '連続登録' })).toBeVisible();
		await page.getByLabel('対象グループ').selectOption({ label: GROUP_NAME });
		await page.getByLabel('開始アドレス').fill('D100');
	});

	test.afterAll(async () => {
		await page.close();
	});

	async function fillCount(value: string): Promise<void> {
		await page.getByLabel('点数').fill(value);
	}

	test('1. 点数1 -> プレビュー（1件）と一致する', async () => {
		await fillCount('1');
		await expect(page.getByRole('heading', { level: 4, name: 'プレビュー（1件）' })).toBeVisible();
	});

	test('2. 点数2 -> プレビュー（2件）と一致する', async () => {
		await fillCount('2');
		await expect(page.getByRole('heading', { level: 4, name: 'プレビュー（2件）' })).toBeVisible();
		// プレビュー行自体も期待した名前/アドレスで並んでいること
		// （名前パターンの既定値 `temp{n}`・開始番号1・開始アドレス D100、
		// データ型の既定 i16 は+1刻み）。
		await expect(page.locator('.preview-table tbody tr')).toHaveCount(2);
		const firstRow = page.locator('.preview-table tbody tr').nth(0);
		await expect(firstRow).toContainText('temp1');
		await expect(firstRow).toContainText('D100');
		const secondRow = page.locator('.preview-table tbody tr').nth(1);
		await expect(secondRow).toContainText('temp2');
		await expect(secondRow).toContainText('D101');
	});

	test('3. 点数1000 -> プレビュー（1000件）と一致する', async () => {
		await fillCount('1000');
		await expect(page.getByRole('heading', { level: 4, name: 'プレビュー（1000件）' })).toBeVisible(
			{ timeout: 15_000 }
		);
		await expect(page.locator('.preview-table tbody tr')).toHaveCount(1000);
	});

	test('4. 点数0 -> 人間可読なエラーになる', async () => {
		await fillCount('0');
		await expect(page.locator('.err')).toHaveText('点数は1以上の整数で指定してください。');
	});

	test('5. 点数-1 -> 人間可読なエラーになる', async () => {
		await fillCount('-1');
		await expect(page.locator('.err')).toHaveText('点数は1以上の整数で指定してください。');
	});

	test('6. 点数1.5(小数) -> 人間可読なエラーになる', async () => {
		await fillCount('1.5');
		await expect(page.locator('.err')).toHaveText('点数は1以上の整数で指定してください。');
	});

	test('7. 点数1001(上限超え) -> 人間可読なエラーになる', async () => {
		await fillCount('1001');
		await expect(page.locator('.err')).toHaveText('点数は1000以下で指定してください。');
	});
});
