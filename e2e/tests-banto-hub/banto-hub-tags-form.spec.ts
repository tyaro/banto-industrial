/**
 * タグ create/edit Drawer の `<form>` 化・Enter 送信・必須表示（T18-1
 * 続き、TAG-UX-C 1点目、docs/banto-hub-desktop-plan.md §9.4「create /
 * edit を `<form>` 化し、必須表示、Enter 送信、クライアント軽量検証と
 * サーバー検証を組み合わせる」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-busy.spec.ts`/`banto-hub-tags-dirty-confirm.spec.ts` と
 * 同じパターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提
 * データ（PLC接続・収集グループ・タグ）は `page.request` で直接 REST を
 * 叩いて作る。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-form-plc';
const GROUP_NAME = 'e2e-form-group';
const CREATE_TAG_NAME = 'e2e-form-create-tag';
const EDIT_TAG_NAME = 'e2e-form-edit-tag';

test.describe.serial('banto-hub タグ create/edit Drawer の form 化 (TAG-UX-C)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + 収集グループ +
		// 編集テスト用のタグ1件（実 PLC/実ネットワークへは繋がない）。
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
		const group = (await groupRes.json()) as { id: number };

		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: EDIT_TAG_NAME,
				collectionGroupId: group.id,
				// modbus-tcp 接続配下なので Modbus 参照番号形式が必要
				// （dirty-confirm spec と同じ理由）。
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. create Drawer: 必須項目を入力後 Enter で送信され、タグが作成される', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(CREATE_TAG_NAME);
		await drawer.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		const addressInput = drawer.getByLabel('アドレス');
		// modbus-tcp 接続配下なので Modbus 参照番号形式（`Address::parse`、
		// crates/banto-plc/src/address.rs）が必要 - 他 spec と同じ理由。
		await addressInput.fill('40010');
		// テキスト入力内で Enter を押すと(ブラウザ既定の挙動として) フォームの
		// submit イベントが発火する - ボタンクリックではなくこの経路が
		// 「Enter 送信」の受け入れ対象。
		await addressInput.press('Enter');

		await expect(page.getByText('作成しました')).toBeVisible();
		// handleCreate() は成功後も Drawer を開いたまま createForm だけ
		// blankForm() に戻す（連続作業を想定した既存挙動） - フォームが
		// 空に戻っていることで送信が実際に行われたことを確認する。
		await expect(drawer.getByLabel('名前')).toHaveValue('');

		await drawer.getByRole('button', { name: '閉じる' }).click();
		await expect(page.getByRole('gridcell', { name: CREATE_TAG_NAME, exact: true })).toBeVisible();
	});

	test('2. create Drawer: 名前を空のまま送信すると HTML5 validation でサーバーに送信されない', async () => {
		let postCount = 0;
		await page.route('**/api/tags', async (route) => {
			if (route.request().method() === 'POST') postCount++;
			await route.continue();
		});

		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		// 名前は空のまま、他の必須項目（収集グループ・アドレス）だけ埋める。
		await drawer.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		await drawer.getByLabel('アドレス').fill('40020');
		await drawer.getByRole('button', { name: '作成' }).click();

		// ブラウザの HTML5 制約検証が submit イベント自体を止めるため、
		// onsubmit 経由の handleCreate() は呼ばれず POST は発生しない。
		// (少し待って非同期な取り漏れがないことも確認する。)
		await page.waitForTimeout(300);
		expect(postCount).toBe(0);

		// Drawer は開いたまま、名前欄がブラウザの制約検証で invalid と
		// 判定されている。
		await expect(drawer).toBeVisible();
		const nameInvalid = await drawer
			.getByLabel('名前')
			.evaluate((el) => !(el as HTMLInputElement).checkValidity());
		expect(nameInvalid).toBe(true);

		await page.unroute('**/api/tags');
	});

	test('3. edit Drawer: 変更後 Enter で送信され、タグが更新される', async () => {
		await page.goto('/tags');
		await page.getByRole('gridcell', { name: EDIT_TAG_NAME, exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${EDIT_TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		const unitInput = drawer.getByLabel('単位');
		await unitInput.fill('℃');
		await unitInput.press('Enter');

		await expect(page.getByText('更新しました')).toBeVisible();
	});
});
