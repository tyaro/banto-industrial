/**
 * 自由トレンドの画面（`/free-trends`、#1207・#1231 PR 8b1）の実 DOM 固定。
 *
 * 固定したい受入条件:
 * 1. 保存された自由トレンドが `<select>` に並び、`?id=` で選べる。作成者（`createdBy`）が出る。
 * 2. 収集を始めると、監視と同じハイブリッドのチャート（`TrendChart`）が出て線が描かれる。
 * 3. 選択を替えると URL の `?id=` が替わる。一覧に無い `?id=` は「見つかりません」と出し、
 *    一覧の選択は使える。
 * 4. 作った人（admin）には「削除」が出る。確認のうえ消すと、一覧から消え、`?id=` が外れる。
 * 5. （8b2a）UI で新しく作る（名前・ペン 2 本）→ 保存 → 一覧に出て `?id=` が付く。
 * 6. （8b2a）既存のペンを 1 本外す → 再読み込みしても下書きが残る → 保存で反映され下書きは消える。
 * 7. （8b2a）REST で先に更新してから UI で保存 → 版の食い違いの文言と「保存されたものを読み込む」。
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
const FT_NEW = `${FREE_TREND_PREFIX}新規-${RUN_ID}`;
const FT_EDIT = `${FREE_TREND_PREFIX}編集-${RUN_ID}`;

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
	revision: number;
	timeWindowSec: number;
	pens: { tagId: number; colorSlot: number | null }[];
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
	const tagIds: Record<string, number> = {};

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
		tagIds[TAG_RAMP] = ramp.id;
		tagIds[TAG_SECOND] = second.id;
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
	test('5. UI で新しく作る（名前・ペン 2 本）と、保存して一覧に出て ?id= が付く', async () => {
		await page.goto(`/free-trends?id=${trends[FT_ONE].id}`);
		await page.getByTestId('free-trend-new').click();
		const pane = page.getByTestId('free-trend-pane');
		await expect(pane).toBeVisible();
		await page.getByTestId('free-trend-name').fill(FT_NEW);
		await page.getByTestId('free-trend-window').selectOption('300');

		await page.getByTestId('free-trend-add-pens').click();
		// 接続先 → 収集グループの木から選ぶ。検索で絞り、チェックして 2 件まとめて追加する。
		await pane.getByRole('searchbox').fill('E2E-FT-');
		const rows = pane.getByRole('row').filter({ hasText: /E2E-FT-(ランプ|第2)/ });
		await expect(rows).toHaveCount(2);
		await rows.nth(0).getByRole('checkbox').check();
		await rows.nth(1).getByRole('checkbox').check();
		await pane.getByRole('button', { name: /選んだ 2 件を追加/ }).click();
		await pane.getByRole('button', { name: /戻る/ }).click();
		await expect(page.getByTestId('free-trend-pen-row')).toHaveCount(2);

		const created = page.waitForResponse(
			(res) => res.url().endsWith('/api/free-trends') && res.request().method() === 'POST'
		);
		await page.getByTestId('free-trend-save').click();
		const response = await created;
		expect(response.status()).toBe(200);
		const body = (await response.json()) as FreeTrendRow;
		expect(body.name).toBe(FT_NEW);
		expect(body.pens).toHaveLength(2);
		trends[FT_NEW] = body;

		await expect(page).toHaveURL(new RegExp(`/free-trends\\?id=${body.id}$`));
		await expect(picker().locator('option', { hasText: FT_NEW })).toHaveCount(1);
		await expect(picker()).toHaveValue(String(body.id));
		await expect(page.getByTestId('free-trend-pane')).toHaveCount(0);
		await expect(page.getByTestId('free-trend-draft-badge')).toHaveCount(0);
	});

	test('6. 既存のペンを外すと下書きになり、再読み込みしても残り、保存で反映される', async () => {
		// 属性つきのペンでも保存で落ちないことを見るため、REST で線種を付けてから UI で触る。
		const seeded = await page.request.post('/api/free-trends', {
			headers,
			data: {
				name: FT_EDIT,
				timeWindowSec: 600,
				pens: [
					{ tagId: tagIds[TAG_RAMP], colorSlot: 4, lineStyle: 'dashed', lineWidth: 2 },
					{ tagId: tagIds[TAG_SECOND], colorSlot: null }
				]
			}
		});
		expect(seeded.ok(), await seeded.text()).toBe(true);
		const row = (await seeded.json()) as FreeTrendRow;
		trends[FT_EDIT] = row;

		await page.goto(`/free-trends?id=${row.id}`);
		await page.getByTestId('free-trend-edit').click();
		await expect(page.getByTestId('free-trend-pen-row')).toHaveCount(2);
		await page.getByTestId('free-trend-pen-remove').nth(1).click();
		await expect(page.getByTestId('free-trend-pen-row')).toHaveCount(1);
		await expect(page.getByTestId('free-trend-draft-discard')).toBeVisible();

		await page.reload();
		await expect(page.getByTestId('free-trend-draft-badge')).toBeVisible();
		await page.getByTestId('free-trend-edit').click();
		await expect(page.getByTestId('free-trend-pen-row')).toHaveCount(1);

		const updated = page.waitForResponse(
			(res) => res.url().endsWith(`/api/free-trends/${row.id}`) && res.request().method() === 'PUT'
		);
		await page.getByTestId('free-trend-save').click();
		const response = await updated;
		expect(response.status()).toBe(200);
		const body = (await response.json()) as FreeTrendRow & {
			pens: { tagId: number; lineStyle: string | null; lineWidth: number | null }[];
		};
		expect(body.pens).toHaveLength(1);
		// 保存済みの属性は、下書きと保存でそのまま保たれる。
		expect(body.pens[0]).toMatchObject({
			tagId: tagIds[TAG_RAMP],
			lineStyle: 'dashed',
			lineWidth: 2
		});
		trends[FT_EDIT] = body;

		await expect(page.getByTestId('free-trend-pane')).toHaveCount(0);
		await expect(page.getByTestId('free-trend-draft-badge')).toHaveCount(0);
		await page.reload();
		await expect(page.getByTestId('free-trend-draft-badge')).toHaveCount(0);
	});

	test('7. 先に REST で更新されていると、保存は版の食い違いで止まり、読み込み直せる', async () => {
		const target = trends[FT_EDIT];
		await page.goto(`/free-trends?id=${target.id}`);
		await page.getByTestId('free-trend-edit').click();
		await page.getByTestId('free-trend-name').fill(`${FT_EDIT}-UI`);

		// UI が読んだ版のあとに、別の人が先に変えた。
		const other = await page.request.put(`/api/free-trends/${target.id}`, {
			headers,
			data: {
				name: `${FT_EDIT}-REST`,
				timeWindowSec: 600,
				pens: [{ tagId: tagIds[TAG_RAMP], colorSlot: null }],
				expectedRevision: target.revision
			}
		});
		expect(other.ok(), await other.text()).toBe(true);

		const conflicted = page.waitForResponse(
			(res) =>
				res.url().endsWith(`/api/free-trends/${target.id}`) && res.request().method() === 'PUT'
		);
		await page.getByTestId('free-trend-save').click();
		expect((await conflicted).status()).toBe(409);
		const notice = page.getByTestId('free-trend-conflict');
		await expect(notice).toContainText('ほかの人が先に変えました');
		// 押すまで下書きは残る。
		await expect(page.getByTestId('free-trend-name')).toHaveValue(`${FT_EDIT}-UI`);

		await page.getByTestId('free-trend-load-saved').click();
		await expect(page.getByTestId('free-trend-conflict')).toHaveCount(0);
		await expect(page.getByTestId('free-trend-name')).toHaveValue(`${FT_EDIT}-REST`);
		await expect(page.getByTestId('free-trend-draft-badge')).toHaveCount(0);
	});
});
