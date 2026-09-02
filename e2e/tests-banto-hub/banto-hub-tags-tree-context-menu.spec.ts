/**
 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「旧画面を消すと失われる機能」）
 * の実 DOM 受け入れテスト。PLC 接続画面・収集グループ画面を廃止する前に、
 * タグ画面のツリー右クリックメニューへ移設した機能のうち、**virtual
 * （calc/mem）配下の収集グループ操作**をここで固定する:
 *
 * - `calc`/`mem`（virtual）接続そのものの再設定・削除は禁止のまま
 * - 配下の収集グループは作成・再設定・削除できる
 * - `canWrite` がある利用者にはツリー上部に常設の作成ボタン
 *   （「PLC接続を追加」「収集グループを追加」）が出る
 *
 * （`tagTreeContextMenu.ts::resolveTreeContextMenuItems`）。
 *
 * **viewer ロールの読み取り専用閲覧（`ConnectionDrawer`/
 * `CollectionGroupDrawer` の `readOnly` モード、`resolveReadOnlyTreeContextMenuItems`/
 * `resolveTreeContextMenuItemsForRole`）は、この E2E スイートでは検証
 * できない。** 理由: このスイートの `banto-hub.exe` は新規 DB で起動した
 * ままロックダウン（`docs/tag-server-design.md` §5.6「試運転モードと
 * ロックダウン」の `POST /api/commissioning/lock-down`）を一度も行わない
 * ため、常に**試運転モード**で動く。試運転モード中は
 * `commissioning.rs::synthetic_identity` がどんなリクエスト（viewer で
 * ログインしたものも含む）も合成 admin identity として扱うため、viewer
 * ユーザーでログインしても `canWrite` は常に true になり、role による
 * 差が一切現れない（実機で確認済み - viewer ログイン後も「収集グループを
 * 追加」の常設ボタンが表示され続けた）。**このスイート全体が試運転モードの
 * バイパスに依存して動いている**（このファイル以外の全 spec も実ログイン
 * 経由のトークンを使うだけでロックダウンはしない）ため、viewer 検証のためだけに
 * ここでロックダウンすると、以降走る他 spec のふるまいを変えてしまう
 * リスクがある一方、このファイルはスイートの最後に走る前提を将来の
 * spec 追加が崩しうる（アルファベット順 `testMatch` に依存した前提は
 * 壊れやすい）ため、環境側を変える対応は採らない。
 *
 * 代わりに viewer ロールの権限差（「詳細を表示」の1項目のみ・作成/再設定/
 * 削除を含まない）は、環境に依存しない純関数の単体テストで固定している:
 * `apps/banto-hub/src/lib/banto/tagTreeContextMenu.test.ts`
 * の `describe('resolveReadOnlyTreeContextMenuItems', ...)` と
 * `describe('resolveTreeContextMenuItemsForRole', ...)`
 * （`canWriteResources('viewer') === false` を前提に、virtual 接続でも
 * 再設定・削除が絶対に含まれないことまで固定している）。
 *
 * `banto-hub-tags-monitor-tree.spec.ts` と同じパターン: 別
 * `describe.serial` ブロック、認証・前提データは `page.request` で直接
 * REST を叩いて作る（`simulation: true`、実 PLC 不要）。ファイル名は
 * `smoke` より辞書順で後（`tags-tree-context-menu` > `smoke`）にしてある
 * （`banto-hub-auth.ts` の注記参照 - smoke の初回セットアップ DOM 検証を
 * 壊さないため）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const REAL_CONN = `e2e-tcm-conn-${RUN_ID}`;
const REAL_GROUP = `e2e-tcm-grp-${RUN_ID}`;
const VIRT_GROUP = `e2e-tcm-virtgrp-${RUN_ID}`;
const VIRT_GROUP_RENAMED = `${VIRT_GROUP}-renamed`;

interface ConnectionRow {
	id: number;
	name: string;
	protocol: string;
}
interface GroupRow {
	id: number;
	name: string;
	plcConnectionId: number;
}

/**
 * 冪等な後始末。このテストが作る行（実接続/実グループ/virtual配下の
 * 収集グループ）だけを名前で絞って消す - `calc`/`mem` そのものは対象外
 * （削除できない予約接続であり、このテストの固定対象）。
 */
async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as GroupRow[];
		const targetNames = new Set([REAL_GROUP, VIRT_GROUP, VIRT_GROUP_RENAMED]);
		for (const g of groups.filter((g) => targetNames.has(g.name))) {
			await request.delete(`/api/collection-groups/${g.id}`, { headers });
		}
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as ConnectionRow[];
		for (const c of connections.filter((c) => c.name === REAL_CONN)) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe
	.serial('banto-hub タグツリー右クリックメニュー: virtual配下操作・常設作成入口 (T19 S1-a)', () => {
	let adminPage: Page;
	let adminHeaders: Record<string, string>;
	let calcConnectionId: number;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage();
		await adminPage.goto('/login');
		const adminToken = await fetchAuthToken(adminPage.request);
		await injectAuthToken(adminPage, adminToken);
		adminHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${adminToken}` };

		await cleanupFixtures(adminPage.request, adminHeaders);

		// 実接続+実グループ（常設作成ボタン確認とは無関係だが、他 spec と
		// 同じ前提データの持ち方に揃えておく - 将来 viewer 検証の環境が
		// 整った際にこのファイルへ足し戻しやすくするため）。
		const connRes = await adminPage.request.post('/api/plc-connections', {
			headers: adminHeaders,
			data: {
				name: REAL_CONN,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
			}
		});
		expect(connRes.ok()).toBe(true);
		const conn = (await connRes.json()) as ConnectionRow;

		const groupRes = await adminPage.request.post('/api/collection-groups', {
			headers: adminHeaders,
			data: { name: REAL_GROUP, plcConnectionId: conn.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);

		// calc（virtual）接続の id を取得する - サーバー起動時に自動
		// プロビジョニングされる予約接続（`banto_tags::CALC_CONNECTION_NAME`）。
		const connectionsRes = await adminPage.request.get('/api/plc-connections', {
			headers: adminHeaders
		});
		const connections = (await connectionsRes.json()) as ConnectionRow[];
		const calc = connections.find((c) => c.name === 'calc' && c.protocol === 'virtual');
		if (!calc) {
			throw new Error(
				'calc（virtual）接続が見つかりません - サーバー起動時の自動プロビジョニングを確認してください'
			);
		}
		calcConnectionId = calc.id;
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec への影響を避ける）。
		await cleanupFixtures(adminPage.request, adminHeaders);
		await adminPage.close();
	});

	test('1. virtual（calc）接続ノードは「収集グループを作成」のみで、接続自体の再設定・削除は出ない', async () => {
		await adminPage.goto('/tags');
		// `exact: true` 必須: 他 spec（banto-hub-tags-delete-impact.spec.ts）
		// が `e2e-del-impact-calc-group` という calc 配下グループを残して
		// おり、非 exact だと部分一致で strict mode violation になる
		// （実際にこの事故で本テストが1度落ちている - 他 spec が残す
		// データと衝突しうる前提で名前ベースの取得は必ず exact にする）。
		const calcNode = adminPage.getByRole('tree').getByRole('button', { name: 'calc', exact: true });
		await expect(calcNode).toBeVisible();
		await calcNode.click({ button: 'right' });

		const menu = adminPage.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();
		await expect(
			menu.getByRole('menuitem', { name: '収集グループを作成', exact: true })
		).toBeVisible();
		await expect(menu.getByRole('menuitem', { name: '接続を再設定', exact: true })).toHaveCount(0);
		await expect(menu.getByRole('menuitem', { name: '接続を削除', exact: true })).toHaveCount(0);

		await adminPage.keyboard.press('Escape');
		await expect(menu).toHaveCount(0);
	});

	test('2. virtual（calc）配下の収集グループを作成できる', async () => {
		await adminPage.goto('/tags');
		const calcNode = adminPage.getByRole('tree').getByRole('button', { name: 'calc', exact: true });
		await calcNode.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '収集グループを作成', exact: true }).click();

		const wizard = adminPage.getByRole('dialog', { name: '新規作成', exact: true });
		await expect(wizard).toBeVisible();
		await wizard.locator('#group-name').fill(VIRT_GROUP);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		// 接続ノードの右クリックから開いたので、所属 PLC 接続には calc が
		// プリセットされている（`openGroupCreateDrawer(action.connectionId)`、
		// tags/+page.svelte::activateTreeContextMenuAction 参照）。
		await expect(wizard.locator('select').first()).toHaveValue(String(calcConnectionId));
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();
		await wizard.getByRole('button', { name: '作成', exact: true }).click();

		await expect(adminPage.getByText('作成しました')).toBeVisible();
		await expect(groupNodeByName(adminPage, VIRT_GROUP)).toBeVisible();
	});

	test('3. virtual（calc）配下の収集グループを再設定できる', async () => {
		await adminPage.goto('/tags');
		const groupNode = groupNodeByName(adminPage, VIRT_GROUP);
		await expect(groupNode).toBeVisible();
		await groupNode.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '収集グループを再設定', exact: true }).click();

		const drawer = adminPage.getByRole('dialog', { name: `${VIRT_GROUP} を編集`, exact: true });
		await expect(drawer).toBeVisible();
		await drawer.locator('#group-name').fill(VIRT_GROUP_RENAMED);
		await drawer.getByRole('button', { name: '保存', exact: true }).click();
		await expect(adminPage.getByText('更新しました')).toBeVisible();
	});

	test('4. virtual（calc）配下の収集グループを削除できる', async () => {
		await adminPage.goto('/tags');
		const groupNode = groupNodeByName(adminPage, VIRT_GROUP_RENAMED);
		await expect(groupNode).toBeVisible();
		await groupNode.click({ button: 'right' });

		// 削除確認の window.confirm は自動で「OK」を押す
		// （`CollectionGroupDrawer.svelte::handleDelete`、他 spec と同じ作法）。
		adminPage.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await adminPage.getByRole('menuitem', { name: '収集グループを削除', exact: true }).click();

		await expect(adminPage.getByText('削除しました')).toBeVisible();
		await expect(groupNodeByName(adminPage, VIRT_GROUP_RENAMED)).toHaveCount(0);
	});

	test('5. canWrite がある利用者にはツリー上部に常設の作成ボタンが出る', async () => {
		await adminPage.goto('/tags');
		await expect(
			adminPage.getByRole('button', { name: 'PLC接続を追加', exact: true })
		).toBeVisible();
		await expect(
			adminPage.getByRole('button', { name: '収集グループを追加', exact: true })
		).toBeVisible();
	});
});
