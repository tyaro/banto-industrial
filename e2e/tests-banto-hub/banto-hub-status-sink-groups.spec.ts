/**
 * S6（docs/banto-hub-external-db-design.md §5.2・§5.3・§5.5・§7 row S6）: DB
 * Sink の sink group 管理画面（`/sink`）と状態画面（`/status`）の「DB
 * Sink」節の SvelteKit/TS 側受け入れテスト。実 PostgreSQL には依存しない -
 * `banto-hub-tags-db-source-connection.spec.ts`（S1/S3）と同じ方針で、
 * postgres 接続は REST 上に登録するだけで実際の疎通は一切試みない。
 * サイドカーの状態は `page.request.put('/api/sink/status', …)` で管理者
 * セッションから直接 push して模擬する（`require_sink_admin`は API キー
 * だけでなく admin ロールのセッションも受け付ける -
 * `apps/banto-hub/core/src/rest.rs::require_sink_admin`参照）。
 *
 * 固定する内容:
 * 1. sink group の新規作成（DB 接続は postgres のみが選択肢に出る・
 *    推奨 DDL プレビューが `buildRecommendedDdl`/`recommended_ddl`と同じ
 *    文面・タグ検索で対象タグを選べる）。
 * 2. 作成した group の編集（`intervalMs`変更が保存される）。
 * 3. `PUT /api/sink/status`で push した内容が `/status` の「DB Sink」節
 *    （サイドカー online・group の状態/キュー滞留）にそのまま反映される。
 * 4. 一覧からの削除。
 *
 * ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後
 * （`banto-hub-auth.ts`のモジュール doc「ensureLoggedIn を使う新規 spec は
 * 辞書順で smoke より後」参照）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const PLC_CONN = `e2e-sink-plc-${RUN_ID}`;
const PG_CONN = `e2e-sink-pg-${RUN_ID}`;
const GROUP_NAME = `e2e-sink-group-${RUN_ID}`;
const TAG_NAME = `e2e-sink-tag-${RUN_ID}`;
const SINK_GROUP_NAME = `e2e-sink-log-${RUN_ID}`;
const TABLE_NAME = `public.e2e_sink_${RUN_ID}`;

interface ConnectionRow {
	id: number;
	name: string;
	protocol: string;
}

interface SinkGroupRow {
	id: number;
	name: string;
}

async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const sinkRes = await request.get('/api/sink/groups', { headers });
	if (sinkRes.ok()) {
		const groups = (await sinkRes.json()) as SinkGroupRow[];
		for (const g of groups.filter((g) => g.name === SINK_GROUP_NAME)) {
			await request.delete(`/api/sink/groups/${g.id}`, { headers });
		}
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as ConnectionRow[];
		for (const c of connections.filter((c) => c.name === PLC_CONN || c.name === PG_CONN)) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe.serial('banto-hub DB Sink: sink group の作成・編集・状態表示・削除（S6）', () => {
	let adminPage: Page;
	let adminHeaders: Record<string, string>;
	let sinkGroupId: number;

	test.beforeAll(async ({ browser }) => {
		adminPage = await browser.newPage();
		await adminPage.goto('/login');
		const adminToken = await fetchAuthToken(adminPage.request);
		await injectAuthToken(adminPage, adminToken);
		adminHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${adminToken}` };

		await cleanupFixtures(adminPage.request, adminHeaders);

		// PLC 接続 → 収集グループ → タグ（sink group が参照するタグの前提データ、
		// db 接続とは無関係な通常タグでよい - 実装指示7「db 接続を持たないタグ」）。
		const plcRes = await adminPage.request.post('/api/plc-connections', {
			headers: adminHeaders,
			data: {
				name: PLC_CONN,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true,
				wordOrder: 'low_high'
			}
		});
		expect(plcRes.ok()).toBe(true);
		const plcConn = (await plcRes.json()) as ConnectionRow;

		const groupRes = await adminPage.request.post('/api/collection-groups', {
			headers: adminHeaders,
			data: {
				name: GROUP_NAME,
				plcConnectionId: plcConn.id,
				periodMs: 1000,
				enabled: true,
				defaultWritable: true
			}
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };

		const tagRes = await adminPage.request.post('/api/tags', {
			headers: adminHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false
			}
		});
		expect(tagRes.ok()).toBe(true);

		// postgres（DB Source）接続 - sink group の dbConnectionId 用。
		const pgRes = await adminPage.request.post('/api/plc-connections', {
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
		expect(pgRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await cleanupFixtures(adminPage.request, adminHeaders);
		await adminPage.close();
	});

	test('1. sink group を作成する（postgres接続のみが選択肢・推奨DDLプレビュー・タグ検索選択）', async () => {
		await adminPage.goto('/sink');
		await adminPage.getByRole('button', { name: '新規作成', exact: true }).click();

		const drawer = adminPage.getByRole('dialog', { name: 'sink group を作成', exact: true });
		await expect(drawer).toBeVisible();

		// DB 接続の選択肢は postgres のみ（modbus-tcp の PLC_CONN は出ない）。
		const dbSelect = drawer.getByLabel('DB 接続（postgres のみ）');
		await expect(dbSelect.locator('option', { hasText: PLC_CONN })).toHaveCount(0);
		await expect(dbSelect.locator('option', { hasText: PG_CONN })).toHaveCount(1);

		await drawer.getByLabel('名前', { exact: true }).fill(SINK_GROUP_NAME);
		await dbSelect.selectOption({ label: PG_CONN });
		await drawer.getByLabel('保存先テーブル（schema.table 可）').fill(TABLE_NAME);

		// 推奨 DDL プレビューが `sinkGroupForm.ts::buildRecommendedDdl`/
		// `apps/banto-hub-sink/src/sql.rs::recommended_ddl`と同じ文面である
		// ことを固定する。
		const ddlBlock = drawer.locator('.ddl-code');
		await expect(ddlBlock).toContainText(`CREATE TABLE "public"."e2e_sink_${RUN_ID}"`);
		await expect(ddlBlock).toContainText('ts timestamptz NOT NULL');
		await expect(ddlBlock).toContainText('tag_id bigint NOT NULL');
		await expect(ddlBlock).toContainText('external_name text NOT NULL');
		await expect(ddlBlock).toContainText('value double precision');
		await expect(ddlBlock).toContainText('quality text NOT NULL');

		// タグ検索で対象タグだけに絞ってチェックする。
		await drawer.getByPlaceholder('検索（タグ名・アドレス・接続・グループ）').fill(TAG_NAME);
		const tagCheckbox = drawer
			.locator('.tag-row', { hasText: TAG_NAME })
			.locator('input[type="checkbox"]');
		await expect(tagCheckbox).toHaveCount(1);
		await tagCheckbox.check();
		await expect(drawer.getByText('1件選択中')).toBeVisible();

		const created = adminPage.waitForResponse(
			(r) => r.url().includes('/api/sink/groups') && r.request().method() === 'POST'
		);
		await drawer.getByRole('button', { name: '作成', exact: true }).click();
		const createdResponse = await created;
		const createdBody = (await createdResponse.json()) as SinkGroupRow;
		sinkGroupId = createdBody.id;

		await expect(adminPage.getByText('作成しました')).toBeVisible();

		const row = adminPage.locator('tr', { hasText: SINK_GROUP_NAME });
		await expect(row).toBeVisible();
		await expect(row).toContainText(PG_CONN);
		await expect(row).toContainText('interval（定周期）');
		await expect(row).toContainText(TABLE_NAME);
		await expect(row).toContainText('1'); // タグ数
	});

	test('2. sink group を編集する（intervalMs の変更が保存される）', async () => {
		await adminPage.goto('/sink');
		const row = adminPage.locator('tr', { hasText: SINK_GROUP_NAME });
		await row.getByRole('button', { name: '編集', exact: true }).click();

		const drawer = adminPage.getByRole('dialog', {
			name: `${SINK_GROUP_NAME} を編集`,
			exact: true
		});
		await expect(drawer).toBeVisible();
		await expect(drawer.getByLabel('発行間隔（ミリ秒）')).toHaveValue('1000');

		await drawer.getByLabel('発行間隔（ミリ秒）').fill('5000');

		const updated = adminPage.waitForResponse(
			(r) => r.url().includes(`/api/sink/groups/${sinkGroupId}`) && r.request().method() === 'PUT'
		);
		await drawer.getByRole('button', { name: '保存', exact: true }).click();
		await updated;

		await expect(adminPage.getByText('更新しました')).toBeVisible();
		await adminPage.keyboard.press('Escape');

		const row2 = adminPage.locator('tr', { hasText: SINK_GROUP_NAME });
		await expect(row2).toContainText('5000');
	});

	test('3. PUT /api/sink/status で push した内容が /status の「DB Sink」節に反映される', async () => {
		const pushRes = await adminPage.request.put('/api/sink/status', {
			headers: adminHeaders,
			data: {
				groups: [
					{
						id: sinkGroupId,
						state: 'running',
						queued: 3,
						dropped: 1,
						lastFlushAt: Date.now(),
						lastError: null
					}
				]
			}
		});
		expect(pushRes.ok()).toBe(true);

		await adminPage.goto('/status');
		const sinkSection = adminPage.locator('section', {
			has: adminPage.getByRole('heading', { name: 'DB Sink', exact: true })
		});
		await expect(sinkSection).toBeVisible();
		await expect(sinkSection).toContainText('オンライン');

		const groupRow = sinkSection.locator('tr', { hasText: `#${sinkGroupId}` });
		await expect(groupRow).toBeVisible();
		await expect(groupRow).toContainText('正常');
		await expect(groupRow).toContainText('3');
		await expect(groupRow).toContainText('1');
	});

	test('4. 一覧から sink group を削除する', async () => {
		await adminPage.goto('/sink');
		const row = adminPage.locator('tr', { hasText: SINK_GROUP_NAME });
		await expect(row).toBeVisible();

		adminPage.once('dialog', (dialog) => void dialog.accept());
		const deleted = adminPage.waitForResponse(
			(r) =>
				r.url().includes(`/api/sink/groups/${sinkGroupId}`) && r.request().method() === 'DELETE'
		);
		await row.getByRole('button', { name: '削除', exact: true }).click();
		await deleted;

		await expect(adminPage.getByText('削除しました')).toBeVisible();
		await expect(adminPage.locator('tr', { hasText: SINK_GROUP_NAME })).toHaveCount(0);
	});
});
