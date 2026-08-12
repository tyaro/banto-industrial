/**
 * TAG-P0-2「保存成功」と「実行可能」を一致させる（docs/banto-hub-desktop-plan.md
 * §9.3）の実 DOM 受け入れテスト。バックエンドの全構成 preflight
 * （`preflight_transaction` → `build_config_from`/`build_catalog_from`/
 * `computed::build_plan`、`apps/banto-hub/core/src/rest.rs`）は T14-3 で
 * 実装済みで、不正アドレスは DB に保存されず 422 validation を返す
 * （`apps/banto-hub/core/tests/t11_batch_tags.rs::single_invalid_address_rolls_back_the_db_and_configured_revision`）。
 * 本 spec は残っていたギャップ、すなわち UI 側がその失敗（
 * `field: "configuration"`、`rest.rs::preflight_api_error`）を画面に一切
 * 出さずサイレント失敗していないことの実 DOM 確認（`cursor/t18-1-tag-p0-2-e3cb`）。
 *
 * `banto-hub-tags-form.spec.ts` と同じパターン: 別 `describe.serial`
 * ブロック（別 `page`）、認証・前提データ（PLC接続・収集グループ）は
 * `page.request` で直接 REST を叩いて作る。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-p0-2-plc';
const GROUP_NAME = 'e2e-p0-2-group';
const INVALID_TAG_NAME = 'e2e-p0-2-invalid-tag';
const VALID_TAG_NAME = 'e2e-p0-2-valid-tag';

test.describe.serial('banto-hub タグ preflight 失敗の可視性 (TAG-P0-2)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの modbus-tcp 接続 + 収集グループ
		// （実 PLC/実ネットワークへは繋がない）。
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
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('Modbus 接続配下に SLMP 形式アドレス D100 を登録すると preflight で拒否され、成功トースト・DB保存のいずれも起きない', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(INVALID_TAG_NAME);
		await drawer.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		const addressInput = drawer.getByLabel('アドレス');
		// D100 は MELSEC（SLMP）デバイス表記であり、この接続の modbus-tcp
		// （`Address::parse`、crates/banto-plc/src/address.rs）では不正
		// - Modbus 参照番号は5〜6桁の数字のみ受け付ける。
		await addressInput.fill('D100');
		await addressInput.press('Enter');

		// 受け入れ条件「不正な Modbus / SLMP アドレスを登録して『成功』のみを
		// 表示しない」: 成功トーストは出ない。
		await expect(page.getByText('作成しました')).toHaveCount(0);

		// preflight 失敗（field="configuration"、`rest.rs::preflight_api_error`）
		// が画面内に見える - フォーム全体エラーと、メッセージに「アドレス」を
		// 含むためアドレス欄直下にも同じ文言が出る（`applyFieldErrors` の
		// アドレスコピー）。
		const alert = drawer.getByRole('alert');
		await expect(alert).toBeVisible();
		await expect(alert).toContainText('アドレス');
		await expect(alert).toContainText('不正');

		// Drawer は開いたままで、送信前の入力を保持している（黙って何も
		// 起きなかったことにしない）。
		await expect(drawer).toBeVisible();
		await expect(drawer.getByLabel('名前')).toHaveValue(INVALID_TAG_NAME);

		// DB にも保存されていない（preflight 失敗は mutation ごと rollback -
		// `t11_batch_tags.rs::single_invalid_address_rolls_back_the_db_and_configured_revision`
		// と同じ保証をUI経路でも確認する）。
		const listRes = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(listRes.ok()).toBe(true);
		const tags = (await listRes.json()) as { name: string }[];
		expect(tags.some((t) => t.name === INVALID_TAG_NAME)).toBe(false);

		// グリッド（Drawer の背後、DOM上には既に存在する - 送信は失敗して
		// `reload()` も呼ばれていないので一覧に増えていない）にもそのタグ名は
		// 出ない。ここで「閉じる」は押さない - フォームは送信前の入力を保持した
		// まま dirty なので、閉じようとすると別 spec
		// （`banto-hub-tags-dirty-confirm.spec.ts`）が確認済みの
		// `window.confirm` 破棄確認が挟まり本題から逸れるため。
		await expect(page.getByRole('gridcell', { name: INVALID_TAG_NAME, exact: true })).toHaveCount(
			0
		);
	});

	test('回帰: 正当な Modbus 参照番号アドレスなら preflight を通過して作成に成功する', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(VALID_TAG_NAME);
		await drawer.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		const addressInput = drawer.getByLabel('アドレス');
		await addressInput.fill('40010');
		await addressInput.press('Enter');

		await expect(page.getByText('作成しました')).toBeVisible();
		await expect(drawer.getByRole('alert')).toHaveCount(0);

		// T18-2c: create Drawer には「登録して閉じる」ボタンも増え、部分一致だと
		// Drawer 右上の × （aria-label="閉じる"）と二重マッチするため exact 指定。
		await drawer.getByRole('button', { name: '閉じる', exact: true }).click();
		await expect(page.getByRole('gridcell', { name: VALID_TAG_NAME, exact: true })).toBeVisible();
	});
});
