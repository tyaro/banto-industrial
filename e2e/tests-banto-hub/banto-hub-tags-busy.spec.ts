/**
 * タグ編集 Drawer の busy 相互排他（T18-1 続き、TAG-UX-C、
 * docs/banto-hub-desktop-plan.md §9.4「保存、削除、検証、登録、閉じるを
 * Drawer 単位の busy 状態で相互排他にする」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-dirty-confirm.spec.ts` と同じパターン: 別
 * `describe.serial` ブロック（別 `page`）、認証・前提データ作成は
 * `page.request` で直接 REST を叩く。
 *
 * T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）で挙動が変わった:
 * 削除は「遅延実行」になり、`window.confirm` の OK 直後に
 * （実際の `DELETE` を待たず）ドロワーが閉じるようになった。そのため
 * 「削除実行中は保存ボタンが disabled になり、×でも閉じられない」という
 * 旧テストの前提（削除中もドロワーが開いたまま busy になる）が成立しなく
 * なった - `deleting` busy フラグ自体を削除している
 * （`(app)/tags/+page.svelte` の `isDrawerBusy()` 参照）。このテストは
 * 「実際の DELETE が遅延・失敗しても、確認 OK 後は busy 待ちなしに即座に
 * ドロワーが閉じる」という新しい契約の回帰ガードに書き換えた
 * （`page.route` での遅延ゲートはそのまま流用 - 「実際の削除がどれだけ
 * 遅くても、UI 側は待たない」ことを示すのに使う）。
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

	test('削除確認後は busy 待ちなしに即座にドロワーが閉じる（実際の DELETE は待たない）', async () => {
		// DELETE /api/tags/:id だけを遅延させる - GET/POST/PUT や他 API には
		// 触れない。T19 S2-c2 以降、実際の DELETE は確認 OK の数秒後
		// （`UNDO_WINDOW_MS`）に送られるが、このゲートで「実際に送られた
		// その DELETE がまだ応答を返していない」状態を作り、その間も
		// ドロワーが（とっくに）閉じていることを示す - 「削除は Drawer の
		// busy 状態と無関係になった」ことの回帰ガード。
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

		// T19 S2-c2（UX-40、docs/banto-hub-t19-design.md §3.10）: 削除は
		// 「遅延実行」になり、confirm 直後にドロワーを閉じる・一覧から隠す
		// （`deleting` busy フラグは削除済み - `isDrawerBusy()` 参照）。
		// 実際の DELETE がまだ送信すらされていない（`UNDO_WINDOW_MS` 未経過）
		// この時点で、既にドロワーは閉じている。
		await expect(drawer).toBeHidden();
		await expect(page.getByRole('gridcell', { name: TAG_NAME, exact: true })).toHaveCount(0);

		// ゲートを解放し、猶予後に実際の DELETE が送られて完了することを
		// 確認する（フィクスチャの後始末も兼ねる）。
		const deleteRequest = page.waitForResponse(
			(res) => res.request().method() === 'DELETE' && /\/api\/tags\/\d+$/.test(res.url())
		);
		releaseDelete?.();
		await deleteRequest;
	});
});
