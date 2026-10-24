/**
 * TAG-P0-2「保存成功」と「実行可能」を一致させる（docs/banto-hub-desktop-plan.md
 * §9.3）の実 DOM 受け入れテスト。
 *
 * R2b-2b 以降、アドレスの不正は保存の層（`TagService`、接続のドライバの
 * `parse_address`、docs/banto-hub-driver-address-design.md §3）が preflight より
 * 先に `address` の項目エラー（422）で断る。画面ではアドレス欄の直下
 * （`#tag-address-err`）にドライバのメッセージが出る。preflight の
 * `field: "configuration"` の全体エラー（`role="alert"`）は、アドレス以外の
 * 構成の不整合（演算式・依存の循環など）のときに出る。
 *
 * 以下は preflight 側の背景: バックエンドの全構成 preflight
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
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

const CONNECTION_NAME = 'e2e-p0-2-plc';
const GROUP_NAME = 'e2e-p0-2-group';
const INVALID_TAG_NAME = 'e2e-p0-2-invalid-tag';
const VALID_TAG_NAME = 'e2e-p0-2-valid-tag';
const CLEANUP_TARGET = { groupNames: [GROUP_NAME], connectionNames: [CONNECTION_NAME] };

test.describe.serial('banto-hub タグ preflight 失敗の可視性 (TAG-P0-2)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 失敗テストのリトライで beforeAll が再走した場合に備え、前回分の
		// 同名リソースを先に掃除する（初回実行では何もしない）。
		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);

		// 前提データ: シミュレーションモードの modbus-tcp 接続 + 収集グループ
		// （実 PLC/実ネットワークへは繋がない）。
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
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
		await cleanupFixtures(page.request, authedHeaders, CLEANUP_TARGET);
		await page.close();
	});

	test('Modbus 接続配下に SLMP 形式アドレス D100 を登録すると保存時にドライバが拒否し（アドレス欄にエラー）、成功トースト・DB保存のいずれも起きない', async () => {
		await page.goto('/tags');
		// T19 S1-c（UX-33）: 「新規登録」はツリーでグループが選択されている
		// ときしか出ない上、開いた create Drawer は選択中グループへ確定済み
		// （収集グループの `<select>` が disabled）になる - まずツリーで対象
		// グループを選ぶ（`groupNodeByName` は `banto-hub-auth.ts` 参照）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		// #342 段階C: 新規作成フォームも広幅では非モーダルの右ペイン
		// （`<aside aria-label>` = role `complementary`）へ移った（#375 の編集
		// ペインと同じ型）ため `role="dialog"` では取れない。アクセシブル名
		// （`新規作成`）は Modal 時代と同じ。
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(INVALID_TAG_NAME);
		const addressInput = drawer.getByLabel('アドレス');
		// D100 は MELSEC（SLMP）デバイス表記であり、この接続の modbus-tcp
		// ドライバ（`parse_address`、crates/banto-driver-modbus/src/base.rs）では
		// 不正 - Modbus 参照番号は5〜6桁の数字のみ受け付ける。
		await addressInput.fill('D100');
		await addressInput.press('Enter');

		// 受け入れ条件「不正な Modbus / SLMP アドレスを登録して『成功』のみを
		// 表示しない」: 成功トーストは出ない。
		await expect(page.getByText('作成しました')).toHaveCount(0);

		// 保存時のドライバの検証（R2b-2b）が `address` の項目エラー（422）で
		// 断り、そのメッセージがアドレス欄の直下（`#tag-address-err`、
		// `+page.svelte` の `errors.address`）に見える。メッセージ全文は
		// ドライバ側の文言なので、安定した部分（入力値と「Modbus の参照番号」）
		// だけを確かめる。
		const addressError = drawer.locator('#tag-address-err');
		await expect(addressError).toBeVisible();
		await expect(addressError).toContainText('D100');
		await expect(addressError).toContainText('Modbus の参照番号');
		await expect(addressInput).toHaveAttribute('aria-invalid', 'true');

		// Drawer は開いたままで、送信前の入力を保持している（黙って何も
		// 起きなかったことにしない）。
		await expect(drawer).toBeVisible();
		await expect(drawer.getByLabel('名前')).toHaveValue(INVALID_TAG_NAME);

		// DB にも保存されていない（保存の拒否は mutation ごと rollback -
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

	test('回帰: 正当な Modbus 参照番号アドレスなら保存時の検証と preflight を通過して作成に成功する', async () => {
		await page.goto('/tags');
		// T19 S1-c（UX-33）: 前のテストと同じ理由でツリーの対象グループ選択が
		// 先に必要（`groupNodeByName` doc comment 参照）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(VALID_TAG_NAME);
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
