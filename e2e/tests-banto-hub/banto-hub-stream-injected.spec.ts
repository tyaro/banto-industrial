/**
 * #441 のレビュー対応: モニタ画面を通した結合テスト（close を注入する側）。
 *
 * `chromium-locked-down` プロジェクト（`banto-hub.playwright.config.ts`）
 * 専用 - ロックダウン後はモニタがセッションで `/api/v1/stream` を開き、
 * ログイン状態の確認（ルートガード）が実際に走る。
 *
 * サーバーに任意の close（未知の理由文の `1008`、今のセッションが有効なままの
 * `session_revoked`）を送らせる手段は無いので、`page.routeWebSocket` で
 * `/api/v1/stream` を**丸ごと差し替え**（実サーバーへは繋がない）、値の
 * スナップショットと close を注入する。REST（カタログ・ルートガードの
 * `/api/auth/check`）は実サーバーを使い、照合できないケースだけ
 * `/api/auth/check` を `page.route` で 500 / 通信失敗にする。実サーバーの
 * 失効で閉じるケースは `banto-hub-stream-revoked.spec.ts`（注入しない）。
 *
 * 見ること:
 * 1. 未知の理由文の `1008` → 理由と「再接続」ボタンを出し、押すまで接続しない。
 *    止まっている間の表は「最終受信値」「陳腐化（受信時: 良好）」。押したら
 *    新しいスナップショットで通常の表示へ戻る。
 * 2. `session_revoked` → 確認し直して `200 true` → `/monitor` に残り、再開用の
 *    ソケットを 1 本だけ作り、スナップショットで通常の表示へ戻る。
 * 3. `session_revoked` → 確認し直して `500` / 通信失敗 → 再試行付きのエラー
 *    画面。トークンは残り、ソケットは増えない。
 */
import {
	expect,
	test,
	type APIRequestContext,
	type Locator,
	type Page,
	type Route,
	type WebSocketRoute
} from '@playwright/test';
import { CSRF_HEADERS, TOKEN_STORAGE_KEY, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONN = `e2e-inj-conn-${RUN_ID}`;
const GROUP = `e2e-inj-grp-${RUN_ID}`;
const TAG = `e2e-inj-tag-${RUN_ID}`;
const EXTERNAL = `${CONN}.${GROUP}.${TAG}`;
const NAME_PREFIX = 'e2e-inj-';

/** 自動再接続のバックオフ（1 秒 → 2 秒）より長く待って、接続が増えないことを見る。 */
const LONGER_THAN_BACKOFF_MS = 3_500;
const SESSION_CHECK_FAILED_TEXT = 'ログイン状態を確認できませんでした';

async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const targets = groups.filter((g) => g.name.startsWith(NAME_PREFIX));
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
		for (const c of connections.filter((c) => c.name.startsWith(NAME_PREFIX))) {
			await request.delete(`/api/plc-connections/${c.id}`, { headers });
		}
	}
}

test.describe
	.serial('banto-hub モニタの停止と再開（close を注入、ロックダウン済み専用サーバー）', () => {
	let page: Page;
	let token = '';
	let authedHeaders: Record<string, string> = {};
	/** このテストで開かれた `/api/v1/stream`（差し替え済み）。 */
	let sockets: WebSocketRoute[] = [];
	/** 各ソケットが最初の `subscribe` を送ったか。 */
	let subscribed: boolean[] = [];

	function row(): Locator {
		return page.getByRole('row').filter({ hasText: EXTERNAL });
	}
	function valueCell(): Locator {
		return row().getByRole('cell').nth(3);
	}
	function qualityCell(): Locator {
		return row().getByRole('cell').nth(4);
	}
	function status(): Locator {
		return page.getByTestId('monitor-stream-status');
	}

	async function sendSnapshot(index: number, v: number): Promise<void> {
		await expect.poll(() => subscribed[index] ?? false).toBe(true);
		sockets[index].send(
			JSON.stringify({ op: 'data', values: [{ tag: EXTERNAL, v, q: 'good', t: Date.now() }] })
		);
	}

	/** `/monitor` を開き、差し替えたソケットで良好な値 42 を受けた状態にする。 */
	async function openMonitorWithGoodValue(): Promise<void> {
		sockets = [];
		subscribed = [];
		await page.goto('/monitor');
		await expect(row()).toBeVisible();
		await expect.poll(() => sockets.length).toBe(1);
		await sendSnapshot(0, 42);
		await expect(status()).toContainText('接続中（リアルタイム更新中）');
		await expect(valueCell()).toHaveText('42');
		await expect(qualityCell()).toHaveText('良好');
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		token = await fetchAuthToken(page.request);
		// ほかのロックダウン専用 spec と同じ手順（lock_down() は冪等）。
		const lockDownRes = await page.request.post('/api/commissioning/lock-down', {
			headers: { ...CSRF_HEADERS, Authorization: `Bearer ${token}` }
		});
		expect(lockDownRes.ok(), await lockDownRes.text()).toBe(true);
		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupFixtures(page.request, authedHeaders);
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONN,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
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
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok(), await tagRes.text()).toBe(true);

		// 実サーバーへは繋がない（`connectToServer()` を呼ばない）。
		await page.routeWebSocket(/\/api\/v1\/stream$/, (ws) => {
			const index = sockets.length;
			sockets.push(ws);
			subscribed[index] = false;
			ws.onMessage((message) => {
				if (typeof message === 'string' && message.includes('"subscribe"')) {
					subscribed[index] = true;
				}
			});
		});
	});

	test.afterAll(async () => {
		await page.unrouteAll({ behavior: 'ignoreErrors' });
		await cleanupFixtures(page.request, authedHeaders);
		await page.close();
	});

	test('1. 未知の理由文の 1008: 理由と再接続ボタンを出し、押すまで繋がない。止まっている間は最終受信値', async () => {
		await openMonitorWithGoodValue();

		await sockets[0].close({ code: 1008, reason: 'e2e_unknown_reason' });
		await expect(status()).toContainText('e2e_unknown_reason');
		const resumeButton = status().getByRole('button', { name: '再接続' });
		await expect(resumeButton).toBeVisible();
		await expect(status()).not.toContainText('接続中');
		await expect(page.getByRole('columnheader', { name: '最終受信値' })).toBeVisible();
		await expect(valueCell()).toHaveText('42');
		await expect(qualityCell()).toHaveText('陳腐化（受信時: 良好）');

		await page.waitForTimeout(LONGER_THAN_BACKOFF_MS);
		expect(sockets).toHaveLength(1);

		await resumeButton.click();
		await expect.poll(() => sockets.length).toBe(2);
		await expect(valueCell()).toHaveText('--');
		await sendSnapshot(1, 43);
		await expect(status()).toContainText('接続中（リアルタイム更新中）');
		await expect(page.getByRole('columnheader', { name: '値', exact: true })).toBeVisible();
		await expect(valueCell()).toHaveText('43');
		await expect(qualityCell()).toHaveText('良好');
		expect(sockets).toHaveLength(2);
	});

	test('2. session_revoked → 確認し直して 200 true: /monitor に残り、ソケットを 1 本だけ張り直して通常へ戻る', async () => {
		await openMonitorWithGoodValue();

		await sockets[0].close({ code: 1008, reason: 'session_revoked' });
		await expect.poll(() => sockets.length).toBe(2);
		await expect(page).toHaveURL(/\/monitor$/);
		await expect(valueCell()).toHaveText('--');
		await sendSnapshot(1, 44);
		await expect(status()).toContainText('接続中（リアルタイム更新中）');
		await expect(valueCell()).toHaveText('44');
		await expect(qualityCell()).toHaveText('良好');

		await page.waitForTimeout(LONGER_THAN_BACKOFF_MS);
		expect(sockets).toHaveLength(2);
		expect(await page.evaluate((key) => sessionStorage.getItem(key), TOKEN_STORAGE_KEY)).toBe(
			token
		);
	});

	for (const [label, failCheck] of [
		[
			'500',
			(route: Route) =>
				route.fulfill({
					status: 500,
					contentType: 'application/json',
					body: JSON.stringify({ kind: 'storage', message: 'database is locked' })
				})
		],
		['通信失敗', (route: Route) => route.abort('failed')]
	] as const) {
		test(`3. session_revoked → 確認し直して ${label}: 再試行付きのエラー画面、トークンを保持、ソケットは増えない`, async () => {
			await openMonitorWithGoodValue();
			// 画面を開いた後で照合だけを失敗させる（開くときのガードは通す）。
			await page.route('**/api/auth/check', failCheck);
			try {
				await sockets[0].close({ code: 1008, reason: 'session_revoked' });
				await expect(page.getByText(SESSION_CHECK_FAILED_TEXT)).toBeVisible();
				await expect(page.getByRole('button', { name: '再試行' })).toBeVisible();
				expect(page.url()).not.toMatch(/\/login$/);
				expect(await page.evaluate((key) => sessionStorage.getItem(key), TOKEN_STORAGE_KEY)).toBe(
					token
				);
				await page.waitForTimeout(LONGER_THAN_BACKOFF_MS);
				expect(sockets).toHaveLength(1);
			} finally {
				await page.unroute('**/api/auth/check');
			}
		});
	}
});
