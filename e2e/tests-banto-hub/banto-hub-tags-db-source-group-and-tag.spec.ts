/**
 * S3（docs/banto-hub-external-db-design.md §7 row S3「Source の UI / CSV /
 * MCP: Drawer の DB フィールド、列名候補の提示、`db` タグの登録 UI、E2E」）:
 * postgres（DB Source）接続配下に収集グループ（`query_sql` 付き）を作り、
 * その配下に `db` タグ（結果列名を手入力）を登録する一連の SvelteKit/TS
 * 側受け入れテスト。
 *
 * `banto-hub-tags-db-source-connection.spec.ts`（S1、postgres 接続そのもの
 * の作成・接続テスト・ツリー DB バッジ）とは対象を分ける - こちらは S2/S2b
 * で解禁されたグループ作成・タグ登録の UI（S3 の新規実装分）だけを固定する。
 * 実 PostgreSQL には依存しない: 収集は開始しない（DB Source task は収集
 * Running のときしか接続しに行かない、§4.8/§6-16）ため、`query_sql` が
 * 実在しない/DB に到達できなくても登録自体は成功する。「列を取得」ボタン
 * （`describeCollectionGroup`）は実 DB 疎通が要るため、ここでは結果列名を
 * 手入力する経路だけを検証する（実装指示「CI で実 PostgreSQL に依存しない」
 * と同じ判断）。
 *
 * 固定する内容:
 * 1. postgres 接続配下では「収集グループを作成」が有効（S1 の disabled
 *    ガードは撤去済み）。
 * 2. 収集グループ作成ウィザードで postgres 接続を選ぶと SQL（SELECT 文）
 *    欄が現れ、必須（空欄では「次へ」に進めない）。作成できたら、ツリーの
 *    グループノードに「SQL」バッジが付く。
 * 3. そのグループ配下に「タグを作成」で開いたフォームは、タグ種別が
 *    `<select>` ではなく読み取り専用バッジ「db（DB Source）」で、アドレス
 *    欄が「結果列名」というラベルになっている。結果列名を手入力して作成
 *    できる。
 * 4. 作成したタグはグリッドの「種別」列に `db` と表示される。
 *
 * `banto-hub-tags-tree-context-menu.spec.ts`/`banto-hub-tags-db-source-connection.spec.ts`
 * と同じパターン: 別 `describe.serial` ブロック、前提データ（接続）は
 * `page.request` で直接 REST を叩いて作る（接続作成そのものは S1 で
 * 別途固定済みのため UI 経由で繰り返さない）。同期点は
 * `page.waitForResponse`（クリックより前に張る）で、トースト文言は使わない
 * （記憶メモの教訓）。
 */
import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const PG_CONN = `e2e-dbsrc-grp-conn-${RUN_ID}`;
const PG_GROUP = `e2e-dbsrc-grp-${RUN_ID}`;
const DB_TAG_NAME = `e2e-dbsrc-tag-${RUN_ID}`;
const RESULT_COLUMN = 'temperature';

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
interface TagRow {
	id: number;
	name: string;
	collectionGroupId: number;
}

function escapeRegExp(value: string): string {
	return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

/**
 * `banto-hub-tags-db-source-connection.spec.ts::connectionNodeByName` と
 * 同じ理由（ツリーの接続ノードのアクセシブル名は「名前 + DB バッジ」の
 * 合成になるため `exact: true` は成立しない）でこのファイルにも複製する。
 */
function connectionNodeByName(page: Page, name: string): Locator {
	return page
		.getByRole('tree')
		.getByRole('button', { name: new RegExp(`^${escapeRegExp(name)}( |$)`) });
}

async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const tagsRes = await request.get('/api/tags', { headers });
	if (tagsRes.ok()) {
		const tags = (await tagsRes.json()) as TagRow[];
		for (const t of tags.filter((t) => t.name === DB_TAG_NAME)) {
			await request.delete(`/api/tags/${t.id}`, { headers });
		}
	}
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as GroupRow[];
		for (const g of groups.filter((g) => g.name === PG_GROUP)) {
			await request.delete(`/api/collection-groups/${g.id}`, { headers });
		}
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as ConnectionRow[];
		for (const c of connections.filter((c) => c.name === PG_CONN)) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe
	.serial('banto-hub postgres（DB Source）グループ作成・db タグ登録 (S3)', () => {
	let adminPage: Page;
	let adminHeaders: Record<string, string>;
	let pgConnectionId: number;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage();
		await adminPage.goto('/login');
		const adminToken = await fetchAuthToken(adminPage.request);
		await injectAuthToken(adminPage, adminToken);
		adminHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${adminToken}` };

		await cleanupFixtures(adminPage.request, adminHeaders);

		// postgres 接続そのものの作成 UI は S1
		// （`banto-hub-tags-db-source-connection.spec.ts`）で固定済みのため、
		// ここでは前提データとして REST で直接作る。
		const connRes = await adminPage.request.post('/api/plc-connections', {
			headers: adminHeaders,
			data: {
				name: PG_CONN,
				protocol: 'postgres',
				host: '10.0.0.9',
				port: 5432,
				unitId: 1,
				enabled: true,
				simulation: false,
				wordOrder: 'low_high',
				database: 'appdb',
				username: 'appuser',
				password: 'hunter2'
			}
		});
		expect(connRes.ok()).toBeTruthy();
		pgConnectionId = ((await connRes.json()) as ConnectionRow).id;
	});

	test.afterAll(async () => {
		await cleanupFixtures(adminPage.request, adminHeaders);
		await adminPage.close();
	});

	test('1. postgres 接続配下では「収集グループを作成」が有効（S1 の disabled ガードは撤去済み）', async () => {
		await adminPage.goto('/tags');
		const node = connectionNodeByName(adminPage, PG_CONN);
		await expect(node).toBeVisible();
		await node.click({ button: 'right' });

		const menu = adminPage.getByRole('menu', { name: '作成メニュー' });
		const createGroupItem = menu.getByRole('menuitem', { name: '収集グループを作成', exact: true });
		await expect(createGroupItem).toBeVisible();
		await expect(createGroupItem).not.toHaveAttribute('aria-disabled', 'true');

		await adminPage.keyboard.press('Escape');
	});

	test('2. SQL 付きの収集グループを作成でき、ツリーに「SQL」バッジが付く', async () => {
		await adminPage.goto('/tags');
		const node = connectionNodeByName(adminPage, PG_CONN);
		await node.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: '収集グループを作成', exact: true }).click();

		const wizard = adminPage.getByRole('dialog', { name: '新規作成', exact: true });
		await expect(wizard).toBeVisible();
		await wizard.locator('#group-name').fill(PG_GROUP);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		// 接続ノードの右クリックから開いたので postgres 接続がプリセット
		// されている（`openGroupCreateDrawer`）。
		await expect(wizard.locator('select').first()).toHaveValue(String(pgConnectionId));

		// SQL 欄が必須 - 空欄では「次へ」に進めない。
		await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeDisabled();
		await wizard
			.locator('#group-query-sql')
			.fill(`SELECT id, ${RESULT_COLUMN}, running FROM sensors`);
		await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeEnabled();
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		await expect(wizard.getByText(`SELECT id, ${RESULT_COLUMN}, running FROM sensors`)).toBeVisible();

		const created = adminPage.waitForResponse(
			(r) => r.url().includes('/api/collection-groups') && r.request().method() === 'POST'
		);
		await wizard.getByRole('button', { name: '作成', exact: true }).click();
		await created;

		await expect(adminPage.getByText('作成しました')).toBeVisible();
		const groupNode = groupNodeByName(adminPage, PG_GROUP);
		await expect(groupNode).toBeVisible();
		await expect(groupNode).toContainText('SQL');
	});

	test('3. グループ配下に db タグを結果列名の手入力で登録できる（タグ種別はバッジ、アドレス欄は「結果列名」）', async () => {
		await adminPage.goto('/tags');
		const groupNode = groupNodeByName(adminPage, PG_GROUP);
		await expect(groupNode).toBeVisible();
		await groupNode.click({ button: 'right' });
		await adminPage.getByRole('menuitem', { name: /配下にタグを作成/, exact: false }).click();

		const drawer = adminPage.getByRole('dialog', { name: '新規作成', exact: true });
		await expect(drawer).toBeVisible();

		// S3（実装指示4）: タグ種別は `<select>` ではなく読み取り専用バッジ。
		await expect(drawer.getByTestId('tag-kind-db-badge')).toBeVisible();
		await expect(drawer.getByTestId('tag-kind-db-badge')).toContainText('db');
		await expect(drawer.locator('select#tag-kind')).toHaveCount(0);

		await drawer.locator('#tag-name').fill(DB_TAG_NAME);
		// アドレス欄のラベルが「結果列名」になっている（PLC アドレスではない）。
		await expect(drawer.getByText('結果列名')).toBeVisible();
		await drawer.locator('#tag-address').fill(RESULT_COLUMN);

		const created = adminPage.waitForResponse(
			(r) => r.url().includes('/api/tags') && r.request().method() === 'POST'
		);
		await drawer.locator('#create-register-close').click();
		await created;

		await expect(adminPage.getByText('作成しました')).toBeVisible();
	});

	test('4. 作成した db タグはグリッドの「種別」列に db と表示される', async () => {
		await adminPage.goto('/tags');
		// `role=row`のアクセシブル名計算に依存せず、行内のテキスト内容で
		// 絞り込む（`.filter({ hasText })`は可視テキストの部分一致で、
		// アクセシビリティツリーの name 計算の実装差に左右されない）。
		const row = adminPage.getByRole('row').filter({ hasText: DB_TAG_NAME });
		await expect(row).toBeVisible();
		await expect(row.getByRole('gridcell', { name: RESULT_COLUMN, exact: true })).toBeVisible();
		await expect(row.getByRole('gridcell', { name: 'db', exact: true })).toBeVisible();
	});
});
