/**
 * モニタの Tree/検索統合 + 確認導線ディープリンク（T18-4a/T18-4c、
 * docs/banto-hub-t18-design.md「T18-4a モニタの Tree/検索統合」「T18-4c
 * 確認導線」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-*.spec.ts` と同じパターン: 別 `describe.serial` ブロック
 * （別 `page`）、認証・前提データは `page.request` で直接 REST を叩いて作る
 * （`simulation: true`、実 PLC 不要）。WS のライブ値までは検証せず、ツリー
 * 選択/検索/ディープリンクによる絞り込み・ハイライトの DOM を検証する
 * （値表示は環境依存なので避ける）。
 *
 * ファイル名について: 認証（`fetchAuthToken`）を使う spec は
 * `banto-hub-smoke.spec.ts` より辞書順で後にする必要がある（先に走ると
 * smoke の初回セットアップ DOM 検証を壊す — `banto-hub-auth.ts` 参照）。
 * `banto-hub-monitor-*` は `smoke` より前にソートされてしまうため、
 * `banto-hub-tags-monitor-*`（`tags` > `smoke`）にしている。共有 DB を
 * 壊さないよう固定名は `RUN_ID` で一意化し、リトライ再走用の冪等掃除を持つ。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONN_A = `e2e-mon-connA-${RUN_ID}`;
const GROUP_A = `e2e-mon-grpA-${RUN_ID}`;
const TAG_A = `e2e-mon-tagA-${RUN_ID}`;
const CONN_B = `e2e-mon-connB-${RUN_ID}`;
const GROUP_B = `e2e-mon-grpB-${RUN_ID}`;
const TAG_B = `e2e-mon-tagB-${RUN_ID}`;

const EXTERNAL_A = `${CONN_A}.${GROUP_A}.${TAG_A}`;
const EXTERNAL_B = `${CONN_B}.${GROUP_B}.${TAG_B}`;

async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const targetGroups = groups.filter((g) => g.name === GROUP_A || g.name === GROUP_B);
		if (targetGroups.length > 0) {
			const groupIds = new Set(targetGroups.map((g) => g.id));
			const tagsRes = await request.get('/api/tags', { headers });
			if (tagsRes.ok()) {
				const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
				for (const tag of tags.filter((t) => groupIds.has(t.collectionGroupId))) {
					await request.delete(`/api/tags/${tag.id}`, { headers });
				}
			}
			for (const g of targetGroups)
				await request.delete(`/api/collection-groups/${g.id}`, { headers });
		}
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as Array<{ id: number; name: string }>;
		for (const c of connections.filter((c) => c.name === CONN_A || c.name === CONN_B)) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe.serial('banto-hub モニタ Tree/検索 + 確認導線ディープリンク (T18-4a/4c)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupAId: number;

	async function createConnGroupTag(
		connName: string,
		groupName: string,
		tagName: string,
		address: string
	): Promise<number> {
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: connName,
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
			data: { name: groupName, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };

		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: tagName,
				collectionGroupId: group.id,
				address,
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
		return group.id;
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupExistingFixtures(page.request, authedHeaders);

		groupAId = await createConnGroupTag(CONN_A, GROUP_A, TAG_A, '40001');
		await createConnGroupTag(CONN_B, GROUP_B, TAG_B, '40002');
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループ/タグは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	test('1. ConnectionTree のグループ選択で一覧が絞られる', async () => {
		await page.goto('/monitor');
		// カタログ読込完了の目安として両タグ行が出るのを待つ（treeFilter=all）。
		await expect(page.getByText(EXTERNAL_A)).toBeVisible();
		await expect(page.getByText(EXTERNAL_B)).toBeVisible();

		// グループ A ノードを選択（ラベルは「グループ名 (件数)」なので部分一致）。
		await page.getByRole('tree').getByRole('button', { name: GROUP_A }).click();

		await expect(page.getByText(EXTERNAL_A)).toBeVisible();
		await expect(page.getByText(EXTERNAL_B)).toHaveCount(0);
	});

	test('2. 検索ボックスで一覧が絞られる', async () => {
		await page.goto('/monitor');
		await expect(page.getByText(EXTERNAL_A)).toBeVisible();

		await page.getByPlaceholder('外部名・名前・アドレスで検索').fill(TAG_A);
		await expect(page.getByText(EXTERNAL_A)).toBeVisible();
		await expect(page.getByText(EXTERNAL_B)).toHaveCount(0);
	});

	test('3. /monitor?group=<id> のディープリンクでそのグループに絞られる', async () => {
		await page.goto(`/monitor?group=${groupAId}`);
		await expect(page.getByText(EXTERNAL_A)).toBeVisible();
		await expect(page.getByText(EXTERNAL_B)).toHaveCount(0);
	});

	test('4. /monitor?focus=<external_name> で該当行に confirm-target が付く', async () => {
		await page.goto(`/monitor?focus=${encodeURIComponent(EXTERNAL_A)}`);
		await expect(page.getByText(EXTERNAL_A)).toBeVisible();

		const highlighted = page.locator('tr.confirm-target');
		await expect(highlighted).toHaveCount(1);
		await expect(highlighted).toContainText(EXTERNAL_A);
	});
});
