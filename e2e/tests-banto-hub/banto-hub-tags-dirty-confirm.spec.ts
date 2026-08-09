/**
 * タグ編集 Drawer の dirty 追跡・破棄確認（T18-1、TAG-UX-C 一部、
 * docs/banto-hub-desktop-plan.md §9.4「dirty 状態を持ち、Esc、背景、×、
 * 別行選択、画面移動で同じ破棄確認を行う」）の実 DOM 受け入れテスト。
 * `formDirty.test.ts`（vitest、純関数の単体テスト）ではまだ確認できて
 * いない「実 DOM で `×` を押したときに `window.confirm` が実際に出て、
 * キャンセルすると Drawer が閉じないままか」を確認する。
 *
 * `banto-hub-tags-continuous.spec.ts` と同じパターン: 別 `describe.serial`
 * ブロック（別 `page`）、認証は `banto-hub-auth.ts::ensureLoggedIn` 相当
 * （未初期化なら setup、初期化済みなら login を自動判定）、前提データ
 * （PLC接続・収集グループ・タグ）は UI 操作ではなく `page.request` で
 * 直接 REST を叩いて作る - このテストの本題は編集 Drawer の dirty/
 * 破棄確認 DOM 挙動であって、タグ作成フォーム自体は他テストの責務。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-dirty-confirm-plc';
const GROUP_NAME = 'e2e-dirty-confirm-group';
const TAG_NAME = 'e2e-dirty-confirm-tag';

test.describe.serial('banto-hub タグ Drawer dirty 破棄確認 (TAG-UX-C)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + 収集グループ +
		// タグ1件（編集対象、実 PLC/実ネットワークへは繋がない）。
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
				// modbus-tcp 接続配下なので Modbus 参照番号形式（`Address::parse`、
				// `crates/banto-plc/src/address.rs`）が必要 - 連続登録テストの
				// `D100` 系は SLMP/三菱デバイス表記で、modbus-tcp では拒否される。
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

	test.beforeEach(async () => {
		await page.goto('/tags');
		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		await expect(page.getByRole('dialog', { name: `${TAG_NAME} を編集` })).toBeVisible();
	});

	test('1. 名前を変更して × → confirm をキャンセル → Drawer は開いたまま', async () => {
		// BantoGrid の列フィルターボタン（aria-label="名前の絞り込み"）と
		// `getByLabel('名前')` が重複するため、Drawer 内に限定する。
		const drawer = page.getByRole('dialog', { name: `${TAG_NAME} を編集` });
		const nameInput = drawer.getByLabel('名前');
		await nameInput.fill(`${TAG_NAME}-changed`);

		let dialogMessage: string | null = null;
		page.once('dialog', (dialog) => {
			dialogMessage = dialog.message();
			void dialog.dismiss();
		});

		await page.getByRole('button', { name: '閉じる' }).click();

		// dismiss() 完了を待ってから状態を確認する（dialog イベントは
		// 非同期に発火するため、クリック直後だと確認前の可能性がある）。
		await expect
			.poll(() => dialogMessage, { message: 'window.confirm が呼ばれること' })
			.toBe('変更を破棄しますか？');

		// キャンセルしたので Drawer は閉じておらず、入力した値も保持される。
		await expect(page.getByRole('dialog', { name: `${TAG_NAME} を編集` })).toBeVisible();
		await expect(nameInput).toHaveValue(`${TAG_NAME}-changed`);
	});

	test('2. 変更なしで × → confirm を出さずに即座に閉じる', async () => {
		let dialogShown = false;
		page.on('dialog', (dialog) => {
			dialogShown = true;
			void dialog.dismiss();
		});

		await page.getByRole('button', { name: '閉じる' }).click();

		await expect(page.getByRole('dialog', { name: `${TAG_NAME} を編集` })).toBeHidden();
		expect(dialogShown).toBe(false);
	});
});
