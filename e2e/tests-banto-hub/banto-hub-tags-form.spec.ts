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
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-form-plc';
const GROUP_NAME = 'e2e-form-group';
const CREATE_TAG_NAME = 'e2e-form-create-tag';
const EDIT_TAG_NAME = 'e2e-form-edit-tag';

test.describe.serial('banto-hub タグ create/edit Drawer の form 化 (TAG-UX-C)', () => {
	let page: Page;
	// T18-2c: 「登録して次へ」の親設定引継ぎ検証（テスト1.5）で、収集グループ
	// の `<select>` value（数値 ID の文字列表現）と突き合わせるために保持する。
	let groupId: number;

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
		groupId = group.id;

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
		// T19 S1-c（UX-33）: 「新規登録」はツリーでグループが選択されている
		// ときしか出ない上、開いた create Drawer は選択中グループへ確定済み
		// （収集グループの `<select>` が disabled）になる - 旧実装は空の
		// フォームから `<select>` で選ぶ前提だったため、まずツリーで対象
		// グループを選んでおく（`groupNodeByName` は `banto-hub-auth.ts`
		// 参照 - グループ行のアクセシブル名は「名前 (件数) 周期」の合成で
		// `exact: true` が成立しない）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(CREATE_TAG_NAME);
		const addressInput = drawer.getByLabel('アドレス');
		// modbus-tcp 接続配下なので Modbus 参照番号形式（`Address::parse`、
		// crates/banto-plc/src/address.rs）が必要 - 他 spec と同じ理由。
		await addressInput.fill('40010');
		// テキスト入力内で Enter を押すと(ブラウザ既定の挙動として) フォームの
		// submit イベントが発火する - ボタンクリックではなくこの経路が
		// 「Enter 送信」の受け入れ対象。
		await addressInput.press('Enter');

		await expect(page.getByText('作成しました')).toBeVisible();
		// T18-2c: テキスト入力内での Enter 実装送信は、DOM 上で先に置いた
		// 「登録して次へ」ボタンが既定（`+page.svelte`
		// `create-register-next`/`SubmitEvent.submitter` 参照）- Drawer は
		// 開いたまま、名前・アドレスだけ空へ戻り（`carryFormForNext`）、他の
		// 共通値（収集グループ等の「親設定」を含む）は引き継がれる。ここでは
		// 名前欄が空に戻っていることで送信が実際に行われたことを確認する。
		await expect(drawer.getByLabel('名前')).toHaveValue('');

		// T18-2c: create Drawer には「登録して閉じる」ボタンも増え、部分一致だと
		// Drawer 右上の × （aria-label="閉じる"）と二重マッチするため exact 指定。
		await drawer.getByRole('button', { name: '閉じる', exact: true }).click();
		await expect(page.getByRole('gridcell', { name: CREATE_TAG_NAME, exact: true })).toBeVisible();
	});

	test('1.5. create Drawer: 「登録して次へ」は親設定（収集グループ）と共通値（データ型・単位）を引き継ぎ、名前/アドレスだけ空にする（T18-2c、TAG-UX-2）', async () => {
		await page.goto('/tags');
		// T19 S1-c（UX-33）: テスト1と同じ理由でツリーの対象グループ選択が
		// 先に必要（`groupNodeByName` doc comment 参照）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill('e2e-form-carry-1');
		// 既定 dataType（f32）から明示的に切り替えて、「直前の入力を引き継ぐ」
		// ことを既定値との一致で偽陽性にならないよう確認する。
		await drawer.getByLabel('データ型').selectOption({ value: 'i16' });
		await drawer.getByLabel('単位').fill('℃');
		const addressInput = drawer.getByLabel('アドレス');
		await addressInput.fill('40030');
		await drawer.getByRole('button', { name: '登録して次へ' }).click();

		await expect(page.getByText('作成しました')).toBeVisible();

		// 名前・アドレスは次のタグ入力に向けて空へ戻る。
		await expect(drawer.getByLabel('名前')).toHaveValue('');
		await expect(addressInput).toHaveValue('');
		// 親設定（収集グループ）と、その他の共通値（データ型・単位）は
		// 直前の入力のまま保持される（TAG-UX-2「親設定と明示選択した共通値を
		// 保持」）。
		await expect(drawer.getByLabel('収集グループ')).toHaveValue(String(groupId));
		await expect(drawer.getByLabel('データ型')).toHaveValue('i16');
		await expect(drawer.getByLabel('単位')).toHaveValue('℃');

		// フォーカスは次の論理入力（名前）へ移っている。
		await expect(drawer.getByLabel('名前')).toBeFocused();

		await drawer.getByRole('button', { name: '閉じる', exact: true }).click();
		await expect(
			page.getByRole('gridcell', { name: 'e2e-form-carry-1', exact: true })
		).toBeVisible();
	});

	test('1.6. create Drawer: 「登録して閉じる」は保存成功後に Drawer 自体を閉じる（T18-2c、TAG-UX-2）', async () => {
		await page.goto('/tags');
		// T19 S1-c（UX-33）: テスト1と同じ理由でツリーの対象グループ選択が
		// 先に必要（`groupNodeByName` doc comment 参照）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill('e2e-form-close-1');
		await drawer.getByLabel('アドレス').fill('40040');
		await drawer.getByRole('button', { name: '登録して閉じる' }).click();

		await expect(page.getByText('作成しました')).toBeVisible();
		// 「登録して次へ」と違い、× を別途押さなくても Drawer 自体が閉じる。
		await expect(drawer).toBeHidden();
		await expect(
			page.getByRole('gridcell', { name: 'e2e-form-close-1', exact: true })
		).toBeVisible();
	});

	test('2. create Drawer: 名前を空のまま送信すると HTML5 validation でサーバーに送信されない', async () => {
		let postCount = 0;
		await page.route('**/api/tags', async (route) => {
			if (route.request().method() === 'POST') postCount++;
			await route.continue();
		});

		// T19 S1-c（UX-33）: ページ遷移はしていないが、直前のテスト（1.6）で
		// 選択したツリーのグループがまだ保持されている保証に頼らず、明示的に
		// 選び直す（`groupNodeByName` doc comment 参照。既に選択中のノードを
		// 再クリックしても treeFilter は変わらず無害）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		// 名前は空のまま送信する。収集グループはツリー選択で確定済み
		// （create Drawer の `<select>` は disabled - 上のコメント参照）。
		// 2026-09-01 オーナー要望（タグ名が空欄ならアドレスをタグ名にする
		// プリフィル、`$lib/banto/tagNamePrefill.ts`）以降、アドレス欄へ
		// 入力すると名前欄が自動で埋まってしまい「名前が空のまま」という
		// この検証の前提が崩れるため、あえてアドレスは埋めない - 名前欄
		// 自体の HTML5 `required` 制約が効いているかどうかは、アドレスの
		// 有無に関わらず `nameInvalid` の判定（下）だけで確認できる。
		// T18-2c: create Drawer のボタンは「登録して次へ」「登録して閉じる」の
		// 2つに分かれた（旧「作成」ボタン）。HTML5 制約検証はどちらのボタンで
		// 押しても等しく submit イベント自体を止めるため、ここでは
		// 既定（Enter 押下時と同じ）の「登録して次へ」を使う。
		await drawer.getByRole('button', { name: '登録して次へ' }).click();

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
