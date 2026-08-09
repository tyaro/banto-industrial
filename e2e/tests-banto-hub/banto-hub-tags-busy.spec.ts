/**
 * タグ編集 Drawer の busy 相互排他（T18-1 続き、TAG-UX-C、
 * docs/banto-hub-desktop-plan.md §9.4「保存、削除、検証、登録、閉じるを
 * Drawer 単位の busy 状態で相互排他にする」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-dirty-confirm.spec.ts` と同じパターン: 別
 * `describe.serial` ブロック（別 `page`）、認証・前提データ作成は
 * `page.request` で直接 REST を叩く。このテストの本題は「削除実行中に
 * 他の操作（保存・×での破棄確認/クローズ）がブロックされるか」であり、
 * `page.route` で `DELETE /api/tags/:id` を遅延させて busy 状態の窓を
 * 作る。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-busy-plc';
const GROUP_NAME = 'e2e-busy-group';
const TAG_NAME = 'e2e-busy-tag';

test.describe.serial('banto-hub タグ Drawer busy 相互排他 (TAG-UX-C)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + 収集グループ +
		// タグ1件（削除対象、実 PLC/実ネットワークへは繋がない）。
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
				name: TAG_NAME,
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

	test('削除実行中は保存ボタンが disabled になり、×でも閉じられない', async () => {
		// DELETE /api/tags/:id だけを遅延させる - GET/POST/PUT や他 API には
		// 触れず、reload() 等の後続処理に影響しないようにする。
		let releaseDelete: (() => void) | undefined;
		const deleteGate = new Promise<void>((resolve) => {
			releaseDelete = resolve;
		});
		await page.route('**/api/tags/*', async (route) => {
			if (route.request().method() !== 'DELETE') {
				await route.continue();
				return;
			}
			await deleteGate;
			await route.continue();
		});

		await page.goto('/tags');
		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		await expect(drawer).toBeVisible();

		// 削除確認の window.confirm は自動で「OK」を押す。
		page.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await drawer.getByRole('button', { name: '削除' }).click();

		// 削除リクエストが（route でゲートされたまま）実行中の間 -
		// 保存・削除自身が disabled になり、Drawer 単位で相互排他される
		// ことを確認する。
		const saveButton = drawer.getByRole('button', { name: '保存' });
		const deleteButton = drawer.getByRole('button', { name: '削除' });
		await expect(saveButton).toBeDisabled();
		await expect(deleteButton).toBeDisabled();

		// busy 中は confirmDiscardIfNeeded() が確認すら出さず false を返す
		// ため、× を押しても window.confirm は出ず、Drawer も閉じない。
		let dialogShownWhileBusy = false;
		page.once('dialog', (dialog) => {
			dialogShownWhileBusy = true;
			void dialog.dismiss();
		});
		await page.getByRole('button', { name: '閉じる' }).click();
		await expect(drawer).toBeVisible();
		expect(dialogShownWhileBusy).toBe(false);

		// ゲートを解放して削除を完了させる - 削除成功後は Drawer が閉じる
		// （既存挙動、`handleDelete` が `drawerMode = null` にする）。
		releaseDelete?.();
		await expect(drawer).toBeHidden();
	});
});
