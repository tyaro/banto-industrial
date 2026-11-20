/**
 * 自由トレンドの画面（`/free-trends`、#1207・#1231 PR 8b1）の実 DOM 固定。
 *
 * 固定したい受入条件:
 * 1. 保存された自由トレンドが `<select>` に並び、`?id=` で選べる。作成者（`createdBy`）が出る。
 * 2. 収集を始めると、監視と同じハイブリッドのチャート（`TrendChart`）が出て線が描かれる。
 * 3. 選択を替えると URL の `?id=` が替わる。一覧に無い `?id=` は「見つかりません」と出し、
 *    一覧の選択は使える。
 * 4. 作った人（admin）には「削除」が出る。確認のうえ消すと、一覧から消え、`?id=` が外れる。
 *
 * ## ファイル名（実行順）
 *
 * 収集を開始するので、`user-settings-collect.spec.ts`（空のレジストリを前提にする）より後
 * （`user-simulator-` は `user-settings-` の後）。初回セットアップの `smoke.spec.ts` より後でもある。
 *
 * ## フィクスチャ
 *
 * 接続・収集グループ・タグ・自由トレンドは REST で作り、`beforeAll` の先頭と `afterAll` で
 * 名前から掃除する。自由トレンドが使うタグは消せないので、自由トレンド → タグ → 収集グループ →
 * 接続の順。他のスペックの有効な接続は一時的に無効にする（`user-simulator-monitor.spec.ts` と同じ）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

const DEV_PLC_PORT = 8803;

const CONNECTION_NAME = 'E2E-FT-PLC';
const COLLECTION_GROUP_NAME = 'E2E-FT-収集';
const TAG_RAMP = 'E2E-FT-ランプ';
const TAG_SECOND = 'E2E-FT-第2';
const TAG_NAMES = [TAG_RAMP, TAG_SECOND];
/** 自由トレンドの名前の接頭辞。実行ごとに一意な名前にし、掃除は接頭辞で行う。 */
const FREE_TREND_PREFIX = 'E2E-FT-自由';
const RUN_ID = String(Date.now());
const FT_ONE = `${FREE_TREND_PREFIX}1-${RUN_ID}`;
const FT_TWO = `${FREE_TREND_PREFIX}2-${RUN_ID}`;

interface NamedRow {
	id: number;
	name: string;
}
interface ConnectionRow extends NamedRow {
	protocol: string;
	settings: Record<string, unknown>;
	enabled: boolean;
}
interface FreeTrendRow extends NamedRow {
	createdBy: string;
	canEdit: boolean;
}

async function fetchApiHeaders(request: APIRequestContext): Promise<ApiHeaders> {
	const res = await request.post('/api/auth/login', {
		headers: CSRF_HEADERS,
		data: { username: ADMIN_USERNAME, password: ADMIN_PASSWORD }
	});
	const body = (await res.json()) as { success: boolean; token?: string; error?: string };
	if (!body.success || !body.token) {
		throw new Error(`REST のログインに失敗しました: ${body.error ?? res.status()}`);
	}
	return { ...CSRF_HEADERS, Authorization: `Bearer ${body.token}` };
}

async function getList<T>(
	request: APIRequestContext,
	headers: ApiHeaders,
	url: string
): Promise<T[]> {
	const res = await request.get(url, { headers });
	if (!res.ok()) throw new Error(`GET ${url} が ${res.status()}: ${await res.text()}`);
	return (await res.json()) as T[];
}

async function postJson<T>(
	request: APIRequestContext,
	headers: ApiHeaders,
	url: string,
	data: unknown
): Promise<T> {
	const res = await request.post(url, { headers, data });
	if (!res.ok()) throw new Error(`POST ${url} が ${res.status()}: ${await res.text()}`);
	return (await res.json()) as T;
}

async function setConnectionEnabled(
	request: APIRequestContext,
	headers: ApiHeaders,
	conn: ConnectionRow,
	enabled: boolean
): Promise<void> {
	const res = await request.put(`/api/plc-connections/${conn.id}`, {
		headers,
		data: { name: conn.name, protocol: conn.protocol, enabled, settings: conn.settings }
	});
	if (!res.ok()) {
		throw new Error(`PUT /api/plc-connections/${conn.id} が ${res.status()}: ${await res.text()}`);
	}
}

async function attempt(failures: string[], what: string, step: () => Promise<void>): Promise<void> {
	try {
		await step();
	} catch (err) {
		failures.push(`${what}: ${err instanceof Error ? err.message : String(err)}`);
	}
}

/** 収集を止め、このスペックのものを依存の逆順に消す（見つからなければ何もしない）。 */
async function cleanupFixtures(request: APIRequestContext, headers: ApiHeaders): Promise<string[]> {
	const failures: string[] = [];
	await attempt(failures, '収集の停止', async () => {
		const res = await request.post('/api/collect/stop', { headers });
		if (!res.ok()) throw new Error(`${res.status()}: ${await res.text()}`);
	});
	const deleteWhere = async (listUrl: string, match: (name: string) => boolean): Promise<void> => {
		const rows = await getList<NamedRow>(request, headers, listUrl);
		for (const row of rows.filter((r) => match(r.name))) {
			const res = await request.delete(`${listUrl}/${row.id}`, { headers });
			if (res.status() !== 204 && res.status() !== 404) {
				throw new Error(`DELETE ${listUrl}/${row.id} が ${res.status()}: ${await res.text()}`);
			}
		}
	};
	await attempt(failures, '自由トレンドの削除', () =>
		deleteWhere('/api/free-trends', (name) => name.startsWith(FREE_TREND_PREFIX))
	);
	await attempt(failures, 'タグの削除', () =>
		deleteWhere('/api/tags', (name) => TAG_NAMES.includes(name))
	);
	await attempt(failures, '収集グループの削除', () =>
		deleteWhere('/api/collection-groups', (name) => name === COLLECTION_GROUP_NAME)
	);
	await attempt(failures, 'PLC接続の削除', () =>
		deleteWhere('/api/plc-connections', (name) => name === CONNECTION_NAME)
	);
	return failures;
}

test.describe.serial('chronogazer 自由トレンドの画面（#1207・#1231 PR 8b1）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let disabledByUs: ConnectionRow[] = [];
	const trends: Record<string, FreeTrendRow> = {};

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		headers = await fetchApiHeaders(page.request);
		const failures = await cleanupFixtures(page.request, headers);
		expect(failures, `前回分の後始末に失敗しました: ${failures.join(' / ')}`).toEqual([]);

		const connections = await getList<ConnectionRow>(page.request, headers, '/api/plc-connections');
		for (const conn of connections.filter((c) => c.enabled && c.name !== CONNECTION_NAME)) {
			await setConnectionEnabled(page.request, headers, conn, false);
			disabledByUs.push(conn);
		}

		const conn = await postJson<NamedRow>(page.request, headers, '/api/plc-connections', {
			name: CONNECTION_NAME,
			protocol: 'modbus-tcp',
			enabled: true,
			settings: { host: '127.0.0.1', port: DEV_PLC_PORT, unit_id: 1 }
		});
		const cg = await postJson<NamedRow>(page.request, headers, '/api/collection-groups', {
			name: COLLECTION_GROUP_NAME,
			plcConnectionId: conn.id,
			periodMs: 500,
			enabled: true
		});
		const tagBase = { collectionGroupId: cg.id, dataType: 'u16', decimals: 0, enabled: true };
		const ramp = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_RAMP,
			address: '40001',
			unit: 'cnt'
		});
		const second = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_SECOND,
			address: '40004'
		});
		const pen = (id: number) => ({ tagId: id });
		trends[FT_ONE] = await postJson<FreeTrendRow>(page.request, headers, '/api/free-trends', {
			name: FT_ONE,
			timeWindowSec: 300,
			pens: [pen(ramp.id)]
		});
		trends[FT_TWO] = await postJson<FreeTrendRow>(page.request, headers, '/api/free-trends', {
			name: FT_TWO,
			timeWindowSec: 600,
			pens: [pen(ramp.id), pen(second.id)]
		});

		await page.goto('/login');
		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test.afterAll(async () => {
		if (!headers) headers = await fetchApiHeaders(page.request);
		const failures = await cleanupFixtures(page.request, headers);
		for (const conn of disabledByUs) {
			await attempt(failures, `接続 ${conn.name} を有効に戻す`, () =>
				setConnectionEnabled(page.request, headers, conn, true)
			);
		}
		disabledByUs = [];
		await page.close();
		expect(failures, `後始末に失敗しました: ${failures.join(' / ')}`).toEqual([]);
	});

	const picker = () => page.getByTestId('free-trend-select');

	test('1. 保存した自由トレンドを選べて、作成者が出る。収集が動いていなければそう出る', async () => {
		await page.goto(`/free-trends?id=${trends[FT_ONE].id}`);
		await expect(page.getByRole('heading', { level: 2, name: '自由トレンド' })).toBeVisible();
		await expect(picker()).toHaveValue(String(trends[FT_ONE].id));
		await expect(picker().locator('option', { hasText: FT_TWO })).toHaveCount(1);
		await expect(page.getByTestId('free-trend-meta')).toContainText(
			`作成 ${trends[FT_ONE].createdBy}`
		);
		await expect(page.getByText('収集が動いていません')).toBeVisible();
	});

	test('2. 収集を始めるとチャートが出て線が描かれる', async () => {
		const res = await page.request.post('/api/collect/start', { headers });
		expect(res.ok(), `POST /api/collect/start が ${res.status()}`).toBe(true);
		const trend = page.getByRole('region', { name: `${FT_ONE} のトレンド表示` });
		await expect(trend).toBeVisible({ timeout: 20_000 });
		await expect(trend.locator('.bi-trend__line').first()).toBeVisible({ timeout: 20_000 });
	});

	test('3. 選択を替えると ?id= が替わる。一覧に無い ?id= は断る', async () => {
		await picker().selectOption(String(trends[FT_TWO].id));
		await expect(page).toHaveURL(new RegExp(`/free-trends\\?id=${trends[FT_TWO].id}$`));
		await expect(page.getByRole('region', { name: `${FT_TWO} のトレンド表示` })).toBeVisible({
			timeout: 20_000
		});

		await page.goto('/free-trends?id=999999999');
		await expect(page.getByText('指定した自由トレンドは見つかりません')).toBeVisible();
		await expect(picker()).toBeVisible();
		await expect(picker().locator('option', { hasText: FT_ONE })).toHaveCount(1);
	});

	test('4. 作った人は確認のうえ削除でき、一覧から消えて ?id= が外れる', async () => {
		await page.goto(`/free-trends?id=${trends[FT_TWO].id}`);
		await expect(picker()).toHaveValue(String(trends[FT_TWO].id));
		const deleteButton = page.getByRole('button', { name: '削除', exact: true });
		await expect(deleteButton).toBeVisible();

		page.once('dialog', (dialog) => {
			expect(dialog.message()).toContain(FT_TWO);
			void dialog.accept();
		});
		const deleted = page.waitForResponse(
			(res) =>
				res.url().endsWith(`/api/free-trends/${trends[FT_TWO].id}`) &&
				res.request().method() === 'DELETE'
		);
		await deleteButton.click();
		expect((await deleted).status()).toBe(204);

		await expect(picker().locator('option', { hasText: FT_TWO })).toHaveCount(0);
		await expect(page).not.toHaveURL(/id=/);
		await expect(picker().locator('option', { hasText: FT_ONE })).toHaveCount(1);
	});
});
