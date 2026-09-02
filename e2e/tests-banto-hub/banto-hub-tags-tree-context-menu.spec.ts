/**
 * T19 S1-a（docs/banto-hub-t19-design.md §7.1「旧画面を消すと失われる機能」）
 * の実 DOM 受け入れテスト。PLC 接続画面・収集グループ画面を廃止する前に、
 * タグ画面のツリー右クリックメニューへ移設した3つの機能を固定する:
 *
 * 1. `calc`/`mem`（virtual）接続そのものの再設定・削除は禁止のまま、
 *    配下の収集グループは作成・再設定・削除できること
 *    （`tagTreeContextMenu.ts::resolveTreeContextMenuItems`）。
 * 2. viewer ロールは接続・グループの詳細を閲覧できるが、入力欄は編集
 *    不可・保存/削除/接続テストのボタンは出ないこと
 *    （`ConnectionDrawer`/`CollectionGroupDrawer` の `readOnly` モード）。
 * 3. `canWrite` がある利用者にはツリー上部に常設の作成ボタン
 *    （「PLC接続を追加」「収集グループを追加」）が出て、無い利用者には
 *    出ないこと。
 *
 * `banto-hub-tags-monitor-tree.spec.ts` と同じパターン: 別
 * `describe.serial` ブロック、認証・前提データは `page.request` で直接
 * REST を叩いて作る（`simulation: true`、実 PLC 不要）。ファイル名は
 * `smoke` より辞書順で後（`tags-tree-context-menu` > `smoke`）にしてある
 * （`banto-hub-auth.ts` の注記参照 - smoke の初回セットアップ DOM 検証を
 * 壊さないため）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const REAL_CONN = `e2e-tcm-conn-${RUN_ID}`;
const REAL_GROUP = `e2e-tcm-grp-${RUN_ID}`;
const VIRT_GROUP = `e2e-tcm-virtgrp-${RUN_ID}`;
const VIRT_GROUP_RENAMED = `${VIRT_GROUP}-renamed`;
const VIEWER_USERNAME = `e2e-tcm-viewer-${RUN_ID}`;
const VIEWER_PASSWORD = 'E2eTcmViewerPass1';
const VIEWER_DISPLAY_NAME = 'E2E閲覧者(TCM)';

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
interface UserRow {
	id: number;
	username: string;
}

/**
 * 冪等な後始末。このテストが作る行（実接続/実グループ/virtual配下の
 * 収集グループ/viewer ユーザー）だけを名前で絞って消す - `calc`/`mem`
 * そのものは対象外（削除できない予約接続であり、このテストの固定対象）。
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
	const usersRes = await request.get('/api/users', { headers });
	if (usersRes.ok()) {
		const users = (await usersRes.json()) as UserRow[];
		for (const u of users.filter((u) => u.username === VIEWER_USERNAME)) {
			await request.delete(`/api/users/${u.id}`, { headers });
		}
	}
}

test.describe
	.serial('banto-hub タグツリー右クリックメニュー: virtual配下操作・viewer閲覧・常設作成入口 (T19 S1-a)', () => {
	let adminPage: Page;
	let viewerPage: Page;
	let adminHeaders: Record<string, string>;
	let calcConnectionId: number;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage();
		await adminPage.goto('/login');
		const adminToken = await fetchAuthToken(adminPage.request);
		await injectAuthToken(adminPage, adminToken);
		adminHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${adminToken}` };

		await cleanupFixtures(adminPage.request, adminHeaders);

		// viewer 閲覧テスト用の実接続+実グループ。
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

		// viewer ユーザーを作成し、別ページ（別ブラウザコンテキスト =
		// sessionStorage が admin と分離される）でログインする。
		const userRes = await adminPage.request.post('/api/users', {
			headers: adminHeaders,
			data: {
				username: VIEWER_USERNAME,
				password: VIEWER_PASSWORD,
				displayName: VIEWER_DISPLAY_NAME,
				role: 'viewer'
			}
		});
		expect(userRes.ok()).toBe(true);

		viewerPage = await browser.newPage();
		await viewerPage.goto('/login');
		const loginRes = await viewerPage.request.post('/api/auth/login', {
			headers: CSRF_HEADERS,
			data: { username: VIEWER_USERNAME, password: VIEWER_PASSWORD }
		});
		expect(loginRes.ok()).toBe(true);
		const loginBody = (await loginRes.json()) as { success: boolean; token?: string };
		expect(loginBody.success).toBe(true);
		await injectAuthToken(viewerPage, loginBody.token as string);
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec への影響を避ける）。
		await cleanupFixtures(adminPage.request, adminHeaders);
		await adminPage.close();
		await viewerPage.close();
	});

	test('1. virtual（calc）接続ノードは「収集グループを作成」のみで、接続自体の再設定・削除は出ない', async () => {
		await adminPage.goto('/tags');
		const calcNode = adminPage.getByRole('tree').getByRole('button', { name: 'calc' });
		await expect(calcNode).toBeVisible();
		await calcNode.click({ button: 'right' });

		const menu = adminPage.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();
		await expect(menu.getByRole('menuitem', { name: '収集グループを作成' })).toBeVisible();
		await expect(menu.getByRole('menuitem', { name: '接続を再設定' })).toHaveCount(0);
		await expect(menu.getByRole('menuitem', { name: '接続を削除' })).toHaveCount(0);

		await adminPage.keyboard.press('Escape');
		await expect(menu).toHaveCount(0);
	});

	test('2. virtual（calc）配下の収集グループを作成できる', async () => {
		await adminPage.goto('/tags');
		const calcNode = adminPage.getByRole('tree').getByRole('button', { name: 'calc' });
		await calcNode.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '収集グループを作成' }).click();

		const wizard = adminPage.getByRole('dialog', { name: '新規作成' });
		await expect(wizard).toBeVisible();
		await wizard.locator('#group-name').fill(VIRT_GROUP);
		await wizard.getByRole('button', { name: '次へ' }).click();

		// 接続ノードの右クリックから開いたので、所属 PLC 接続には calc が
		// プリセットされている（`openGroupCreateDrawer(action.connectionId)`、
		// tags/+page.svelte::activateTreeContextMenuAction 参照）。
		await expect(wizard.locator('select').first()).toHaveValue(String(calcConnectionId));
		await wizard.getByRole('button', { name: '次へ' }).click();
		await wizard.getByRole('button', { name: '作成' }).click();

		await expect(adminPage.getByText('作成しました')).toBeVisible();
		await expect(
			adminPage.getByRole('tree').getByRole('button', { name: VIRT_GROUP })
		).toBeVisible();
	});

	test('3. virtual（calc）配下の収集グループを再設定できる', async () => {
		await adminPage.goto('/tags');
		const groupNode = adminPage.getByRole('tree').getByRole('button', { name: VIRT_GROUP });
		await expect(groupNode).toBeVisible();
		await groupNode.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '収集グループを再設定' }).click();

		const drawer = adminPage.getByRole('dialog', { name: `${VIRT_GROUP} を編集` });
		await expect(drawer).toBeVisible();
		await drawer.locator('#group-name').fill(VIRT_GROUP_RENAMED);
		await drawer.getByRole('button', { name: '保存' }).click();
		await expect(adminPage.getByText('更新しました')).toBeVisible();
	});

	test('4. virtual（calc）配下の収集グループを削除できる', async () => {
		await adminPage.goto('/tags');
		const groupNode = adminPage.getByRole('tree').getByRole('button', { name: VIRT_GROUP_RENAMED });
		await expect(groupNode).toBeVisible();
		await groupNode.click({ button: 'right' });

		// 削除確認の window.confirm は自動で「OK」を押す
		// （`CollectionGroupDrawer.svelte::handleDelete`、他 spec と同じ作法）。
		adminPage.once('dialog', (dialog) => {
			void dialog.accept();
		});
		await adminPage.getByRole('menuitem', { name: '収集グループを削除' }).click();

		await expect(adminPage.getByText('削除しました')).toBeVisible();
		await expect(
			adminPage.getByRole('tree').getByRole('button', { name: VIRT_GROUP_RENAMED })
		).toHaveCount(0);
	});

	test('5. viewer は実接続の詳細を閲覧できるが、入力は編集不可・保存/削除/接続テストは出ない', async () => {
		await viewerPage.goto('/tags');
		// 常設の作成ボタンは canWrite が無いので出ない（3点目の裏側）。
		await expect(viewerPage.getByRole('button', { name: 'PLC接続を追加' })).toHaveCount(0);
		await expect(viewerPage.getByRole('button', { name: '収集グループを追加' })).toHaveCount(0);

		const connNode = viewerPage.getByRole('tree').getByRole('button', { name: REAL_CONN });
		await expect(connNode).toBeVisible();
		await connNode.click({ button: 'right' });

		const menu = viewerPage.getByRole('menu', { name: '作成メニュー' });
		await expect(menu).toBeVisible();
		// viewer 向けメニューは「詳細を表示」の1項目のみ（作成/再設定/削除は無い）。
		await expect(menu.getByRole('menuitem')).toHaveCount(1);
		await menu.getByRole('menuitem', { name: '詳細を表示' }).click();

		const drawer = viewerPage.getByRole('dialog', { name: `${REAL_CONN} の詳細` });
		await expect(drawer).toBeVisible();
		await expect(drawer.locator('#connection-name')).toBeDisabled();
		await expect(drawer.getByRole('button', { name: '保存' })).toHaveCount(0);
		await expect(drawer.getByRole('button', { name: '削除' })).toHaveCount(0);
		await expect(drawer.getByRole('button', { name: '接続テスト' })).toHaveCount(0);

		await drawer.getByRole('button', { name: '閉じる' }).click();
		await expect(drawer).toHaveCount(0);
	});

	test('6. viewer は実グループの詳細も閲覧できるが、入力は編集不可・保存/削除は出ない', async () => {
		await viewerPage.goto('/tags');
		const groupNode = viewerPage.getByRole('tree').getByRole('button', { name: REAL_GROUP });
		await expect(groupNode).toBeVisible();
		await groupNode.click({ button: 'right' });
		await viewerPage.getByRole('menuitem', { name: '詳細を表示' }).click();

		const drawer = viewerPage.getByRole('dialog', { name: `${REAL_GROUP} の詳細` });
		await expect(drawer).toBeVisible();
		await expect(drawer.locator('#group-name')).toBeDisabled();
		await expect(drawer.getByRole('button', { name: '保存' })).toHaveCount(0);
		await expect(drawer.getByRole('button', { name: '削除' })).toHaveCount(0);
	});

	test('7. canWrite がある利用者にはツリー上部に常設の作成ボタンが出る', async () => {
		await adminPage.goto('/tags');
		await expect(adminPage.getByRole('button', { name: 'PLC接続を追加' })).toBeVisible();
		await expect(adminPage.getByRole('button', { name: '収集グループを追加' })).toBeVisible();
	});
});
