/**
 * タグ登録の表の「現在値」「品質」「時刻」の列（#1255 段階 ②）の実 DOM 受け入れテスト。
 *
 * `banto-hub-stream-injected.spec.ts` と同じく、`page.routeWebSocket` で `/api/v1/stream` を
 * 差し替え（実サーバーへは繋がない）、値のスナップショットと close を注入する。REST は
 * 実サーバー（`simulation: true` の接続、実 PLC 不要）。ロックダウンはしない（通常の
 * `chromium` プロジェクト）ので、ファイル名は `smoke` より後ろに並ぶ `banto-hub-tags-*`。
 *
 * 見ること:
 * 1. 値を受ける前は `--`。スナップショットで値（単位付き）・品質・時刻が出る。
 * 2. 購読の範囲はツリーで選んだ範囲: 「すべて」は `*`、グループを選ぶとそのグループの
 *    ワイルドカードで同じソケット上で購読し直す。
 * 3. バックプレッシャ切断（1013）はタグモニタと同じ文言で出し、再接続した直後は
 *    スナップショットが届くまで古い値を出さない。
 */
import {
	expect,
	test,
	type APIRequestContext,
	type Locator,
	type Page,
	type WebSocketRoute
} from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONN = `e2e-val-conn-${RUN_ID}`;
const GROUP = `e2e-val-grp-${RUN_ID}`;
const TAG = `e2e-val-tag-${RUN_ID}`;
const EXTERNAL = `${CONN}.${GROUP}.${TAG}`;

async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const targets = groups.filter((g) => g.name === GROUP);
		const groupIds = new Set(targets.map((g) => g.id));
		const tagsRes = await request.get('/api/tags', { headers });
		if (tagsRes.ok()) {
			const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
			for (const tag of tags.filter((t) => groupIds.has(t.collectionGroupId))) {
				await request.delete(`/api/tags/${tag.id}`, { headers });
			}
		}
		for (const g of targets) await request.delete(`/api/collection-groups/${g.id}`, { headers });
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as Array<{ id: number; name: string }>;
		for (const c of connections.filter((c) => c.name === CONN)) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe.serial('banto-hub タグ登録の値の列 (#1255 段階 ②)', () => {
	let page: Page;
	let authedHeaders: Record<string, string> = {};
	let sockets: WebSocketRoute[] = [];
	/** 各ソケットが受けた `subscribe` の `tags`（順に）。 */
	let subscribes: string[][][] = [];

	function row(): Locator {
		return page.getByRole('row').filter({ hasText: TAG });
	}
	function cellOf(field: string): Locator {
		return row().locator(`[data-cell-field="${field}"]`);
	}
	function status(): Locator {
		return page.getByTestId('tag-stream-status');
	}

	function sendValue(index: number, v: number): void {
		sockets[index].send(
			JSON.stringify({ op: 'data', values: [{ tag: EXTERNAL, v, q: 'good', t: Date.now() }] })
		);
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupFixtures(page.request, authedHeaders);
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONN,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connectionRes.ok(), await connectionRes.text()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };
		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok(), await groupRes.text()).toBe(true);
		const group = (await groupRes.json()) as { id: number };
		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: TAG,
				collectionGroupId: group.id,
				address: '40001',
				dataType: 'i16',
				unit: 'kPa',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok(), await tagRes.text()).toBe(true);

		await page.routeWebSocket(/\/api\/v1\/stream$/, (ws) => {
			const index = sockets.length;
			sockets.push(ws);
			subscribes[index] = [];
			ws.onMessage((message) => {
				if (typeof message !== 'string') return;
				const parsed = JSON.parse(message) as { op?: string; tags?: string[] };
				if (parsed.op === 'subscribe' && parsed.tags) subscribes[index].push(parsed.tags);
			});
		});
	});

	test.afterAll(async () => {
		await page.unrouteAll({ behavior: 'ignoreErrors' });
		await cleanupFixtures(page.request, authedHeaders);
		await page.close();
	});

	test('1. スナップショットで現在値・品質・時刻が出る（受ける前は --）。購読は「すべて」', async () => {
		sockets = [];
		subscribes = [];
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG);
		await expect(row()).toBeVisible();
		await expect.poll(() => subscribes[0]?.length ?? 0).toBe(1);
		expect(subscribes[0][0]).toEqual(['*']);
		await expect(cellOf('currentValue')).toHaveText(/^--/);
		await expect(cellOf('currentTime')).toHaveText(/^--/);

		sendValue(0, 42);
		await expect(status()).toContainText('接続中（リアルタイム更新中）');
		await expect(cellOf('currentValue')).toContainText('42 kPa');
		await expect(cellOf('currentQuality')).toHaveText('良好');
		await expect(cellOf('currentTime')).not.toHaveText(/^--/);
		// 値の列は読み取り専用（編集の入力欄にならない）。
		await expect(cellOf('currentValue').locator('input')).toHaveCount(0);
	});

	test('2. ツリーでグループを選ぶと、購読をそのグループの範囲へ同じソケット上で替える', async () => {
		await page.getByRole('tree').getByRole('treeitem', { name: GROUP }).click();
		await expect.poll(() => subscribes[0]?.length ?? 0).toBe(2);
		expect(subscribes[0][1]).toEqual([`${CONN}.${GROUP}.*`]);
		expect(sockets).toHaveLength(1);
		// 購読し直した直後は、スナップショットが届くまで古い値を出さない。
		await expect(cellOf('currentValue')).toHaveText(/^--/);
		sendValue(0, 43);
		await expect(cellOf('currentValue')).toContainText('43 kPa');

		await page.getByRole('tree').getByRole('treeitem', { name: 'すべて' }).first().click();
		await expect.poll(() => subscribes[0]?.length ?? 0).toBe(3);
		expect(subscribes[0][2]).toEqual(['*']);
	});

	test('3. バックプレッシャ切断（1013）はタグモニタと同じ文言。再接続後はスナップショットまで --', async () => {
		sendValue(0, 44);
		await expect(cellOf('currentValue')).toContainText('44 kPa');

		await sockets[0].close({ code: 1013, reason: 'slow consumer' });
		await expect(status()).toContainText('サーバーが遅い購読者として切断しました（自動再接続中…）');
		await expect.poll(() => sockets.length).toBe(2);
		await expect(cellOf('currentValue')).toHaveText(/^--/);
		await expect.poll(() => subscribes[1]?.length ?? 0).toBe(1);
		sendValue(1, 45);
		await expect(status()).toContainText('接続中（リアルタイム更新中）');
		await expect(cellOf('currentValue')).toContainText('45 kPa');
	});
});
