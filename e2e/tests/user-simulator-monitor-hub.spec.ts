/**
 * Hub 経由のタグを含む表示グループの監視（#383 段階3 P5、2026-10-11 オーナー決定）の
 * 実 DOM 固定。
 *
 * **Hub はモック**（`../chronogazer-mock-hub.ts`: Node の最小の HTTP + WebSocket。
 * 空きポート）で、ChronoGazer の収集は**実際の経路**で Hub の購読を通る
 * （`banto-serve` を `BANTO_HUB_MEMORY_KEYS=1` で起動している - `playwright.config.ts`。
 * 既定の `banto-serve` は OS キーリングが無く Hub 経由の接続を収集しない）。
 *
 * 固定したい受入条件:
 * 1. Hub 経由のタグを含む表示グループが、収集を始めると監視画面で**値を出す**。
 *    モック Hub は値の出所（`value_source`）を `simulation` / `derived_simulation` /
 *    未知の文字列にしている（2026-10-11 オーナー決定: ChronoGazer は Hub の
 *    `value_source` を解釈せず、品質だけで扱う）が、値も「正常」も出る。
 * 2. イベント一覧は接続を `conn:<id>` ではなく名前（`Hub`）で出し、種別は
 *    「接続」（「PLC接続」ではない）。
 * 3. Hub を止めると、監視画面は値を出さず「通信エラー」になる。設定 > 収集の
 *    接続ごとの状態は「再接続中」のまま回数（「1回目」）を出さない。
 * 4. Hub を同じポートで戻すと値が戻り、イベントに「切断」「再接続」が出る。
 *
 * ## ファイル名（実行順）
 *
 * 収集を開始するので、`user-settings-collect.spec.ts` より後。`monitor` スペックと
 * 同じ作法で、始めに前回分と他のスペックの有効な接続を片付け（実在しうる機器へ
 * 接続しに行かない）、終わりに収集を止めて掃除する。`user-simulator-monitor.spec.ts`
 * より前に走る（`-` < `.`）が、そちらも自分で掃除してから始める。
 *
 * ## フィクスチャ
 *
 * Hub への接続（API キーの採用）・Hub 経由のタグの登録は REST（画面の操作は
 * `user-tags-from-hub.spec.ts` が固定済み）。後始末は表示グループ → タグ → 収集グループ
 * → Hub 接続 → Hub への接続の設定の順。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { MockHub, type MockHubTag } from '../chronogazer-mock-hub';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

/** モック Hub に渡す API キー（形だけ。モックは検証しない）。 */
const HUB_KEY = 'bh_abcd1234_e2e-mock-key';

const COLLECTION_GROUP_NAME = 'E2E-HUBMON-収集';
const DISPLAY_GROUP_NAME = 'E2E-HUBMON-表示';
const TEMP = { name: 'temp', external: 'E2EHM.Fast.temp' };
const RUN = { name: 'run', external: 'E2EHM.Fast.run' };
const FUTURE = { name: 'future', external: 'E2EHM.Fast.future' };
const TAG_NAMES = [TEMP.name, RUN.name, FUTURE.name];
const HUB_CONNECTION_NAME = 'Hub';

const HUB_TAGS: MockHubTag[] = [
	{ externalName: TEMP.external, dataType: 'f64', value: 21.5, valueSource: 'simulation' },
	{ externalName: RUN.external, dataType: 'bit', value: 1, valueSource: 'derived_simulation' },
	{ externalName: FUTURE.external, dataType: 'f64', value: 7, valueSource: 'future_source' }
];

interface NamedRow {
	id: number;
	name: string;
	protocol?: string;
	address?: string;
}
interface ConnectionRow extends NamedRow {
	protocol: string;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
	wordOrder: string;
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
		data: {
			name: conn.name,
			protocol: conn.protocol,
			host: conn.host,
			port: conn.port,
			unitId: conn.unitId,
			enabled,
			wordOrder: conn.wordOrder
		}
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
	const deleteWhere = async (listUrl: string, match: (row: NamedRow) => boolean): Promise<void> => {
		const rows = await getList<NamedRow>(request, headers, listUrl);
		for (const row of rows.filter(match)) {
			const res = await request.delete(`${listUrl}/${row.id}`, { headers });
			if (res.status() !== 204 && res.status() !== 404) {
				throw new Error(`DELETE ${listUrl}/${row.id} が ${res.status()}: ${await res.text()}`);
			}
		}
	};
	await attempt(failures, '表示グループの削除', () =>
		deleteWhere('/api/display-groups', (r) => r.name === DISPLAY_GROUP_NAME)
	);
	await attempt(failures, 'タグの削除', () =>
		deleteWhere('/api/tags', (r) => r.address?.startsWith('E2EHM.') === true)
	);
	await attempt(failures, '収集グループの削除', () =>
		deleteWhere('/api/collection-groups', (r) => r.name === COLLECTION_GROUP_NAME)
	);
	await attempt(failures, 'Hub 接続の削除', () =>
		deleteWhere('/api/plc-connections', (r) => r.protocol === 'hub')
	);
	await attempt(failures, 'Hub への接続の設定の削除', async () => {
		const res = await request.delete('/api/hub', { headers });
		if (!res.ok() && res.status() !== 404) {
			throw new Error(`DELETE /api/hub が ${res.status()}: ${await res.text()}`);
		}
	});
	return failures;
}

test.describe.serial('chronogazer 監視画面: Hub 経由のタグ（#383 段階3 P5）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let hub: MockHub | null = null;
	let hubPort = 0;
	let disabledByUs: ConnectionRow[] = [];

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		headers = await fetchApiHeaders(page.request);
		const failures = await cleanupFixtures(page.request, headers);
		expect(failures, `前回分の後始末に失敗しました: ${failures.join(' / ')}`).toEqual([]);

		// 実在しうる機器へ接続しに行かない（`tags.spec.ts` が残す接続を一時的に無効にする）。
		const connections = await getList<ConnectionRow>(page.request, headers, '/api/plc-connections');
		for (const conn of connections.filter((c) => c.enabled)) {
			await setConnectionEnabled(page.request, headers, conn, false);
			disabledByUs.push(conn);
		}

		hub = await MockHub.start(HUB_TAGS);
		hubPort = hub.port;
		const adopted = await page.request.post('/api/hub/adopt-key', {
			headers,
			data: { endpoint: hub.endpoint, key: HUB_KEY }
		});
		expect(adopted.ok(), `POST /api/hub/adopt-key が ${adopted.status()}`).toBe(true);

		const registered = await postJson<{ ok: boolean }>(
			page.request,
			headers,
			'/api/tags/from-hub',
			{
				newGroup: { name: COLLECTION_GROUP_NAME, periodMs: 500 },
				tags: HUB_TAGS.map((tag) => ({
					externalName: tag.externalName,
					name: tag.externalName.split('.').pop(),
					dataType: tag.dataType,
					unit: tag.dataType === 'f64' && tag.value === 21.5 ? 'degC' : null,
					decimals: tag.dataType === 'f64' ? 1 : 0
				}))
			}
		);
		expect(registered.ok, 'POST /api/tags/from-hub').toBe(true);

		const tags = await getList<NamedRow>(page.request, headers, '/api/tags');
		const penFor = (name: string) => {
			const tag = tags.find((t) => t.name === name && t.address?.startsWith('E2EHM.'));
			if (!tag) throw new Error(`タグ ${name} が登録されていません`);
			return { tagId: tag.id, colorSlot: null };
		};
		await postJson<NamedRow>(page.request, headers, '/api/display-groups', {
			name: DISPLAY_GROUP_NAME,
			kind: 'digital',
			attributes: {},
			pens: TAG_NAMES.map(penFor)
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
		await attempt(failures, 'モック Hub の停止', async () => {
			await hub?.stop();
			hub = null;
		});
		await page.close();
		expect(failures, `後始末に失敗しました: ${failures.join(' / ')}`).toEqual([]);
	});

	const panel = () => page.getByRole('tabpanel');
	const cell = (tagName: string) => panel().getByRole('listitem').filter({ hasText: tagName });

	async function openMonitor(): Promise<void> {
		await page.goto('/monitor');
		await page.getByRole('tab', { name: DISPLAY_GROUP_NAME, exact: true }).click();
		await expect(page.getByRole('tab', { name: DISPLAY_GROUP_NAME, exact: true })).toHaveAttribute(
			'aria-selected',
			'true'
		);
	}

	test('1. Hub 経由のタグの値が出る（value_source が simulation・derived_simulation・未知でも「正常」）', async () => {
		const res = await page.request.post('/api/collect/start', { headers });
		expect(res.ok(), `POST /api/collect/start が ${res.status()}`).toBe(true);
		await openMonitor();

		await expect(cell(TEMP.name).locator('.value')).toHaveText('21.5', { timeout: 30_000 });
		await expect(cell(TEMP.name)).toContainText('degC');
		await expect(cell(TEMP.name)).toContainText('正常');
		await expect(cell(RUN.name).locator('.value')).toHaveText('True');
		await expect(cell(RUN.name)).toContainText('正常');
		await expect(cell(FUTURE.name).locator('.value')).toHaveText('7.0');
		await expect(cell(FUTURE.name)).toContainText('正常');
	});

	test('2. イベント一覧は接続を名前で出し、種別は「接続」（「PLC接続」ではない）', async () => {
		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		const row = page
			.getByRole('row')
			.filter({ has: page.getByRole('gridcell', { name: HUB_CONNECTION_NAME, exact: true }) })
			.filter({ has: page.getByRole('gridcell', { name: '接続', exact: true }) });
		await expect(row.first()).toBeVisible({ timeout: 15_000 });
		await expect(page.getByRole('gridcell', { name: 'PLC接続', exact: true })).toHaveCount(0);
		// この Hub 接続のキー（`conn:<id>`）はそのままでは出ない。削除済みの接続の
		// 古いイベントはキーのまま残るので、一覧全体ではなくこの接続の id で見る。
		const connections = await getList<ConnectionRow>(page.request, headers, '/api/plc-connections');
		const hubConnection = connections.find((c) => c.protocol === 'hub');
		expect(hubConnection, 'Hub 接続が登録されている').toBeDefined();
		await expect(
			page.getByRole('gridcell', { name: `conn:${hubConnection?.id}`, exact: true })
		).toHaveCount(0);
	});

	test('3. Hub を止めると、値は出ず「通信エラー」になり、接続の状態は回数なしの「再接続中」', async () => {
		await hub?.stop();
		hub = null;
		await openMonitor();
		for (const name of TAG_NAMES) {
			await expect(cell(name)).toContainText('通信エラー', { timeout: 30_000 });
			await expect(cell(name).locator('.value')).toHaveText('—');
		}

		await page.goto('/settings/collect');
		const row = page
			.locator('table.collect-connections tbody tr')
			.filter({ hasText: HUB_CONNECTION_NAME });
		await expect(row).toContainText('再接続中', { timeout: 20_000 });
		await expect(row).not.toContainText('回目');
	});

	test('4. Hub を戻すと値が戻り、イベントに「切断」「再接続」が出る', async () => {
		hub = await MockHub.start(HUB_TAGS, { port: hubPort });
		await openMonitor();
		// 再接続は購読クライアントの backoff に従う（最大数十秒かかりうる）。
		await expect(cell(TEMP.name).locator('.value')).toHaveText('21.5', { timeout: 90_000 });
		await expect(cell(TEMP.name)).toContainText('正常');

		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		const kindRow = (kind: string) =>
			page
				.getByRole('row')
				.filter({ has: page.getByRole('gridcell', { name: HUB_CONNECTION_NAME, exact: true }) })
				.filter({ has: page.getByRole('gridcell', { name: kind, exact: true }) });
		await expect(async () => {
			await page.getByRole('button', { name: '再読み込み' }).click();
			await expect(kindRow('切断').first()).toBeVisible({ timeout: 1_000 });
			await expect(kindRow('再接続').first()).toBeVisible({ timeout: 1_000 });
		}).toPass({ timeout: 20_000 });
	});
	test('5. /tags: Hub 経由のグループを選ぶとタグの作成フォームが Hub 用になり、接続の選択肢は生の protocol 名を出さない', async () => {
		await page.goto('/tags');
		const tagForm = page.locator('section.registry-section').nth(2).locator('div.create');
		await expect(tagForm.getByLabel('デバイスアドレス')).toBeVisible();
		await expect(tagForm.getByLabel('スケーリング: 生値 下限')).toBeVisible();

		await tagForm.getByLabel('収集グループ').selectOption({ label: COLLECTION_GROUP_NAME });
		await expect(tagForm.getByLabel('Hub のタグ名（外部名）')).toBeVisible();
		await expect(tagForm.getByLabel('スケーリング: 生値 下限')).toHaveCount(0);
		// データ型の 64bit に「Modbusのみ」は付かない。
		await expect(
			tagForm.getByLabel('データ型').locator('option', { hasText: 'Modbus' })
		).toHaveCount(0);

		const groupForm = page.locator('section.registry-section').nth(1).locator('div.create');
		const options = await groupForm.getByLabel('接続').locator('option').allTextContents();
		expect(options).toContain(`${HUB_CONNECTION_NAME}（Hub 経由）`);
		expect(options.some((text) => text.includes('（hub）'))).toBe(false);
	});
});
