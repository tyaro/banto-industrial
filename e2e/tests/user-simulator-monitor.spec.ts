/**
 * 監視画面（`/monitor`、R1-D の D-1）の実 DOM 固定。
 *
 * 固定したい受入条件:
 * 1. 収集が動いていないときは「収集が動いていません」（値の欄を 0 や空で出さない）。
 * 2. 開発用 PLC（`playwright.config.ts` の 2 つ目の `webServer`、Modbus TCP、
 *    127.0.0.1:8803、40001 からランプ波）相手に収集を始めると、デジタル表示の
 *    値が実際に変わる。単位・しきい値（色だけでなく文字）が出る。
 * 3. 設定が不正で外したタグ（品質 `invalid`）は「—」と「設定不正（収集対象外）」、
 *    タグ設定へのリンクで出る（0 にしない）。
 * 4. タブで表示グループを切り替えられ、選択は URL の `?group=` に載る。描けない
 *    種別（D-1 ではトレンド）は「この表示種別は準備中です」。
 * 5. コマンドパレットの「グループ: ◯◯ を表示」でも切り替えられる。`/monitor` を
 *    開き直すと、この端末で最後に見たグループが選ばれている。
 *
 * ## ファイル名（実行順）
 *
 * 収集を開始するので、「起動時の自動開始が空のレジストリで終わった状態」を前提に
 * する `user-settings-collect.spec.ts` より**後**でなければならない（`user-m...`
 * だと前に来てしまう）。`user-simulator-roundtrip.spec.ts` と同じ `user-simulator-`
 * にし、`m` < `r` でその直前に置く。roundtrip は開始前の状態として「停止」も
 * 受け付ける（このスペックの `afterAll` が止める）。
 *
 * ## フィクスチャ
 *
 * 接続・収集グループ・タグ・表示グループは REST で作る（画面の作成は
 * `user-simulator-roundtrip.spec.ts` と `user-display-groups.spec.ts` が固定済み）。
 * `beforeAll` の先頭と `afterAll` の両方で名前から掃除する（チェックリスト §3）。
 * ペンに割り当てたタグは消せないので、表示グループ → タグ → 収集グループ → 接続
 * の順。**他のスペックの有効な接続は一時的に無効にする**（`tags.spec.ts` は
 * 192.168.11.200 を指す接続を残す - E2E から実在しうる機器へ接続しに行かない。
 * roundtrip と同じ理由・同じ作法）。
 *
 * **不正なタグの作り方**は roundtrip と同じ: REST は保存時に拒否する（#418）ので、
 * 正常なアドレスで作ってから DB の行を直接書き換える。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import path from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { RUN_DIR_ENV, RUN_TOKEN_ENV, ownsRunDir } from '../chronogazer-e2e-run-dir';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

const DEV_PLC_PORT = 8803;
const DB_FILE_NAME = 'chronogazer-e2e.sqlite3';

const CONNECTION_NAME = 'E2E-MON-PLC';
const COLLECTION_GROUP_NAME = 'E2E-MON-収集';
const TAG_RAMP = 'E2E-MON-ランプ';
const TAG_INVALID = 'E2E-MON-不正タグ';
const TAG_HIGH = 'E2E-MON-上限';
const GROUP_1 = 'E2E-MON-デジタル1';
const GROUP_2 = 'E2E-MON-デジタル2';
const GROUP_TREND = 'E2E-MON-トレンド';
const TAG_NAMES = [TAG_RAMP, TAG_INVALID, TAG_HIGH];
const GROUP_NAMES = [GROUP_1, GROUP_2, GROUP_TREND];

interface NamedRow {
	id: number;
	name: string;
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
	const deleteByName = async (listUrl: string, names: string[]): Promise<void> => {
		const rows = await getList<NamedRow>(request, headers, listUrl);
		for (const row of rows.filter((r) => names.includes(r.name))) {
			const res = await request.delete(`${listUrl}/${row.id}`, { headers });
			if (res.status() !== 204 && res.status() !== 404) {
				throw new Error(`DELETE ${listUrl}/${row.id} が ${res.status()}: ${await res.text()}`);
			}
		}
	};
	await attempt(failures, '表示グループの削除', () =>
		deleteByName('/api/display-groups', GROUP_NAMES)
	);
	await attempt(failures, 'タグの削除', () => deleteByName('/api/tags', TAG_NAMES));
	await attempt(failures, '収集グループの削除', () =>
		deleteByName('/api/collection-groups', [COLLECTION_GROUP_NAME])
	);
	await attempt(failures, 'PLC接続の削除', () =>
		deleteByName('/api/plc-connections', [CONNECTION_NAME])
	);
	return failures;
}

/** DB のタグの行のアドレスを直接書き換える（roundtrip の `rewriteTagAddress` と同じ）。 */
function rewriteTagAddress(dbPath: string, tagId: number, address: string): void {
	const db = new DatabaseSync(dbPath);
	try {
		db.exec('PRAGMA busy_timeout = 5000');
		const result = db.prepare('UPDATE tags SET address = ? WHERE id = ?').run(address, tagId);
		expect(Number(result.changes), `tags.id = ${tagId} の行を書き換えられなかった`).toBe(1);
	} finally {
		db.close();
	}
}

test.describe.serial('chronogazer 監視画面（R1-D の D-1）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let disabledByUs: ConnectionRow[] = [];
	const groupIds: Record<string, number> = {};

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

		const dbDir = process.env[RUN_DIR_ENV] ?? '';
		expect(ownsRunDir(dbDir, process.env[RUN_TOKEN_ENV]), '一時ディレクトリの所有').toBe(true);

		const conn = await postJson<NamedRow>(page.request, headers, '/api/plc-connections', {
			name: CONNECTION_NAME,
			protocol: 'modbus-tcp',
			host: '127.0.0.1',
			port: DEV_PLC_PORT,
			unitId: 1,
			enabled: true,
			wordOrder: ''
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
		const invalid = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_INVALID,
			address: '40002'
		});
		// u16 は 0 以上なので、H = 0 なら必ず「H 上限以上」になる（しきい値の表示の確認）。
		const high = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_HIGH,
			address: '40003',
			thresholdH: 0
		});
		rewriteTagAddress(path.join(dbDir, DB_FILE_NAME), invalid.id, 'D3000');

		const pen = (id: number) => ({ tagId: id, colorSlot: null });
		for (const [name, kind, pens] of [
			[GROUP_1, 'digital', [ramp.id, invalid.id, high.id]],
			[GROUP_2, 'digital', [high.id]],
			[GROUP_TREND, 'trend', [ramp.id]]
		] as const) {
			const created = await postJson<NamedRow>(page.request, headers, '/api/display-groups', {
				name,
				kind,
				attributes: {},
				pens: pens.map(pen)
			});
			groupIds[name] = created.id;
		}

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

	const tab = (name: string) => page.getByRole('tab', { name, exact: true });
	const panel = () => page.getByRole('tabpanel');
	const cell = (tagName: string) => panel().getByRole('listitem').filter({ hasText: tagName });

	test('1. 収集が動いていないときは「収集が動いていません」で、値の欄を出さない', async () => {
		await page.goto(`/monitor?group=${groupIds[GROUP_1]}`);
		await expect(tab(GROUP_1)).toHaveAttribute('aria-selected', 'true');
		await expect(panel().getByText('収集が動いていません')).toBeVisible();
		await expect(panel().getByRole('listitem')).toHaveCount(0);
	});

	test('2. 収集を始めると値が出て変わる。不正なタグは「—」と設定不正、しきい値は文字でも出る', async () => {
		const res = await page.request.post('/api/collect/start', { headers });
		expect(res.ok(), `POST /api/collect/start が ${res.status()}`).toBe(true);

		const rampValue = cell(TAG_RAMP).locator('.value');
		await expect(rampValue).toHaveText(/^\d+$/, { timeout: 20_000 });
		await expect(cell(TAG_RAMP)).toContainText('cnt');
		await expect(cell(TAG_RAMP)).toContainText('正常');
		const first = await rampValue.textContent();
		await expect(rampValue).not.toHaveText(first ?? '', { timeout: 15_000 });

		await expect(cell(TAG_INVALID).locator('.value')).toHaveText('—');
		await expect(cell(TAG_INVALID)).toContainText('設定不正（収集対象外）');
		await expect(cell(TAG_INVALID).getByRole('link', { name: 'タグ設定で直す' })).toHaveAttribute(
			'href',
			'/tags'
		);

		await expect(cell(TAG_HIGH)).toContainText('H 上限以上');
		await expect(cell(TAG_HIGH).locator('.value')).toHaveText(/^\d+$/);
	});

	test('3. タブで切り替えると URL の ?group= が変わり、値は前のグループから持ち越さない', async () => {
		await tab(GROUP_2).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_2]}$`));
		await expect(tab(GROUP_2)).toHaveAttribute('aria-selected', 'true');
		await expect(cell(TAG_HIGH)).toContainText('H 上限以上');
		await expect(panel().getByRole('listitem')).toHaveCount(1);
		await expect(cell(TAG_RAMP)).toHaveCount(0);

		await tab(GROUP_TREND).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_TREND]}$`));
		await expect(panel().getByText('この表示種別は準備中です')).toBeVisible();
		await expect(panel().getByRole('listitem')).toHaveCount(0);
	});

	test('4. コマンドパレットで切り替えられ、開き直すと最後に見たグループが選ばれている', async () => {
		await page.keyboard.press('Control+k');
		const input = page.getByRole('dialog').getByRole('combobox');
		await expect(input).toBeVisible();
		await input.fill(GROUP_1);
		await page.getByRole('option', { name: `グループ: ${GROUP_1} を表示` }).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_1]}$`));
		await expect(tab(GROUP_1)).toHaveAttribute('aria-selected', 'true');
		await expect(cell(TAG_RAMP).locator('.value')).toHaveText(/^\d+$/, { timeout: 20_000 });

		// 別の画面からでもパレットのグループのコマンドが使える。`goto` 直後は
		// 画面の JS がまだ動いていないことがある（Ctrl+K を受ける前に押すと何も
		// 起きない）ので、ヘッダーの「コマンドパレットを開く」（何度押しても開く
		// だけ）を、開くまで押す。
		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		const input2 = page.getByRole('dialog').getByRole('combobox');
		await expect(async () => {
			await page.getByRole('button', { name: 'コマンドパレットを開く' }).click();
			await expect(input2).toBeVisible({ timeout: 1_000 });
		}).toPass({ timeout: 10_000 });
		await input2.fill(GROUP_2);
		await page.getByRole('option', { name: `グループ: ${GROUP_2} を表示` }).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_2]}$`));

		// URL に指定が無ければ、この端末で最後に見たグループ（GROUP_2）。
		await page.goto('/monitor');
		await expect(tab(GROUP_2)).toHaveAttribute('aria-selected', 'true');
		await expect(page).toHaveURL(/\/monitor$/);
	});
});
