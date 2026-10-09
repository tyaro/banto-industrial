/**
 * 監視画面（`/monitor`、R1-D の D-1・D-2・D-3b）の実 DOM 固定。
 *
 * 固定したい受入条件:
 * 1. 収集が動いていないときは「収集が動いていません」（値の欄を 0 や空で出さない）。
 * 2. 開発用 PLC（`playwright.config.ts` の 2 つ目の `webServer`、Modbus TCP、
 *    127.0.0.1:8803、40001 からランプ波）相手に収集を始めると、デジタル表示の
 *    値が実際に変わる。単位・しきい値（色だけでなく文字）が出る。
 * 3. 設定が不正で外したタグ（品質 `invalid`）は「—」と「設定不正（収集対象外）」、
 *    タグ設定へのリンクで出る（0 にしない）。
 * 4. タブで表示グループを切り替えられ、選択は URL の `?group=` に載る。前の
 *    グループの値を持ち越さない。
 * 5. コマンドパレットの「グループ: ◯◯ を表示」でも切り替えられる。`/monitor` を
 *    開き直すと、この端末で最後に見たグループが選ばれている。
 * 6. バー（D-2）: 工学値レンジのタグは棒が出て値が変わる。LL..HH をレンジにした
 *    タグは HH を超えて「レンジ上限超え」・`data-level="HH"`。不正なタグは棒を
 *    描かず「—」。レンジが無いタグは「レンジ未設定」と値の文字。同じ値のしきい値の
 *    名前は 1 つ（`LL/L/H`）にまとまり、レンジとしきい値は画面外の文で読める（#535）。
 * 7. 計器（D-2）: banto の `Gauge` が出て値が変わる。不正なタグは弧を描かず
 *    「—」（banto v6.3.0 の値なし）。しきい値の判定は `data-level` と文字で出る。
 * 8.（#532）しきい値はタグではなく**記録計の側の設定**（`PUT /api/tag-thresholds/{id}`）
 *    に入れ、監視画面はそこから判定する。収集のしきい値のイベントは、判定に使った
 *    しきい値を `/events` の「水準」に出す（`H 0 以上`）。
 * 9. トレンド（D-3b）: 線（`path` の `d`）が描かれて伸びる。時刻の目盛。しきい値の
 *    帯は選んだ 1 ペンだけ（既定はしきい値のある最初のペン、凡例のペンで切り替え、
 *    画面外の文にも出る）。時間窓は端末ごとに覚え、グループの定義には書かない。
 *    収集を止めている間の刻みは `null` で、線が切れる（`d` の `M` が増える）。
 * 10.（#551、2026-10-09 オーナー決定）bit のタグ（Modbus のコイル 00001、開発用 PLC が
 *    トグルする）は、デジタルで `True` / `False`（0 / 1 ではない）。バー・計器は
 *    「レンジ未設定」にならず 0〜1（目盛 `False` / `True`）。トレンドは全ペンが bit なら
 *    縦軸の目盛が `False` / `True`。
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
const TAG_SCALED = 'E2E-MON-工学値';
const TAG_OVER = 'E2E-MON-レンジ超え';
const TAG_EQUAL = 'E2E-MON-同値しきい値';
const GROUP_BAR = 'E2E-MON-バー';
const GROUP_GAUGE = 'E2E-MON-計器';
const TAG_BIT = 'E2E-MON-ビット';
const GROUP_BIT = 'E2E-MON-ビット表示';
const GROUP_BIT_TREND = 'E2E-MON-ビットトレンド';
const TAG_NAMES = [TAG_RAMP, TAG_INVALID, TAG_HIGH, TAG_SCALED, TAG_OVER, TAG_EQUAL, TAG_BIT];
const GROUP_NAMES = [
	GROUP_1,
	GROUP_2,
	GROUP_TREND,
	GROUP_BAR,
	GROUP_GAUGE,
	GROUP_BIT,
	GROUP_BIT_TREND
];

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

/**
 * #532: 記録計の側のしきい値を保存する（タグの本文にはもう載せられない）。
 * 新しいタグは設定が無いので版は 0。
 */
async function putThresholds(
	request: APIRequestContext,
	headers: ApiHeaders,
	tagId: number,
	thresholds: {
		thresholdLl?: number;
		thresholdL?: number;
		thresholdH?: number;
		thresholdHh?: number;
	}
): Promise<void> {
	const res = await request.put(`/api/tag-thresholds/${tagId}`, {
		headers,
		data: { ...thresholds, expectedRevision: 0 }
	});
	if (!res.ok()) {
		throw new Error(`PUT /api/tag-thresholds/${tagId} が ${res.status()}: ${await res.text()}`);
	}
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

test.describe.serial('chronogazer 監視画面（R1-D の D-1・D-2）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let disabledByUs: ConnectionRow[] = [];
	const groupIds: Record<string, number> = {};
	let highTagId = 0;

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
		// LL..HH があるので、バー・計器ではレンジが決まる（値なしの「—」を計器でも
		// 確かめるため。D-2）。品質 invalid なのでデジタルのしきい値表示には出ない。
		const invalid = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_INVALID,
			address: '40002'
		});
		await putThresholds(page.request, headers, invalid.id, { thresholdLl: 0, thresholdHh: 100 });
		// u16 は 0 以上なので、H = 0 なら必ず「H 上限以上」になる（しきい値の表示の確認）。
		const high = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_HIGH,
			address: '40003'
		});
		await putThresholds(page.request, headers, high.id, { thresholdH: 0 });
		highTagId = high.id;
		// D-2: 工学値レンジ（生値をそのまま 0..65535 に写す = 値は変わらない）。
		const scaledTag = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_SCALED,
			address: '40004',
			unit: 'cnt',
			rawLo: 0,
			rawHi: 65535,
			engLo: 0,
			engHi: 65535
		});
		// D-2: 工学値レンジが無く LL..HH = 0..1 がレンジになる。ランプ波は開発用 PLC の
		// 起動から 100ms ごとに増える（`banto_collect::simulation`）ので、ここに来る
		// ころには 1 を超えている（HH 以上・レンジ上限超え。u16 で一周するのは
		// 約 109 分後）。
		const over = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_OVER,
			address: '40005'
		});
		await putThresholds(page.request, headers, over.id, { thresholdLl: 0, thresholdHh: 1 });
		// #535: 等号は正しい設定（LL <= L <= H <= HH）。同じ値の名前は 1 つにまとまる。
		const equal = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_EQUAL,
			address: '40006',
			rawLo: 0,
			rawHi: 65535,
			engLo: 0,
			engHi: 65535
		});
		await putThresholds(page.request, headers, equal.id, {
			thresholdLl: 30000,
			thresholdL: 30000,
			thresholdH: 30000,
			thresholdHh: 60000
		});
		// #551: bit のタグ。コイル 00001 は開発用 PLC が 100ms ごとにトグルする。工学値レンジも
		// しきい値も付けない（「レンジ未設定」にならないことを確かめる）。
		const bit = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: TAG_BIT,
			dataType: 'bit',
			address: '00001'
		});
		rewriteTagAddress(path.join(dbDir, DB_FILE_NAME), invalid.id, 'D3000');

		const pen = (id: number) => ({ tagId: id, colorSlot: null });
		for (const [name, kind, pens] of [
			[GROUP_1, 'digital', [ramp.id, invalid.id, high.id]],
			[GROUP_2, 'digital', [high.id]],
			[GROUP_TREND, 'trend', [ramp.id, high.id]],
			[GROUP_BAR, 'bar', [scaledTag.id, over.id, invalid.id, high.id, equal.id, bit.id]],
			[GROUP_GAUGE, 'gauge', [scaledTag.id, over.id, invalid.id, high.id, bit.id]],
			[GROUP_BIT, 'digital', [bit.id]],
			[GROUP_BIT_TREND, 'trend', [bit.id]]
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

	test('2b. しきい値のイベントは、判定に使ったしきい値を「水準」に出す（#532）', async () => {
		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		// 記録計の側の H = 0 で判定した「超過」の行。`tag:<id>` は完全一致で絞る
		// （`tag:1` が `tag:12` に部分一致しないように）。
		const highRow = page
			.getByRole('row')
			.filter({ has: page.getByRole('gridcell', { name: `tag:${highTagId}`, exact: true }) });
		await expect(highRow.getByRole('gridcell', { name: 'しきい値超過', exact: true })).toBeVisible({
			timeout: 15_000
		});
		await expect(highRow.getByRole('gridcell', { name: 'H 0 以上', exact: true })).toBeVisible();

		await page.goto(`/monitor?group=${groupIds[GROUP_1]}`);
		await expect(tab(GROUP_1)).toHaveAttribute('aria-selected', 'true');
		await expect(cell(TAG_RAMP).locator('.value')).toHaveText(/^\d+$/, { timeout: 20_000 });
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
		await expect(
			panel().getByRole('region', { name: `${GROUP_TREND} のトレンド表示` })
		).toBeVisible({ timeout: 20_000 });
		await expect(panel().getByText('この表示種別は準備中です')).toHaveCount(0);
		await expect(cell(TAG_INVALID)).toHaveCount(0);
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
	test('5. バー: 棒が出て値が変わる。レンジ外・しきい値・値なし・レンジ未設定を区別する', async () => {
		await tab(GROUP_BAR).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_BAR]}$`));
		await expect(panel().getByRole('list', { name: `${GROUP_BAR} のバー表示` })).toBeVisible();

		// 工学値レンジ: 棒（.fill）があり、値が変わる。
		const scaledCell = cell(TAG_SCALED);
		await expect(scaledCell).toHaveAttribute('data-range', 'ok', { timeout: 20_000 });
		const scaledValue = scaledCell.locator('.value');
		await expect(scaledValue).toHaveText(/^\d+$/, { timeout: 20_000 });
		await expect(scaledCell.locator('.fill')).toHaveCount(1);
		const first = await scaledValue.textContent();
		await expect(scaledValue).not.toHaveText(first ?? '', { timeout: 15_000 });

		// LL..HH = 0..1 のレンジを超える: HH（色と文字）・上限超え。棒は上端に丸める。
		const overCell = cell(TAG_OVER);
		await expect(overCell).toHaveAttribute('data-level', 'HH');
		await expect(overCell).toHaveAttribute('data-tone', 'danger');
		await expect(overCell).toHaveAttribute('data-out', 'over');
		await expect(overCell).toContainText('HH 上上限以上');
		await expect(overCell).toContainText('レンジ上限超え');
		await expect(overCell.locator('.fill')).toHaveAttribute('style', /height:\s*100(\.0+)?%/);

		// 図は aria-hidden。レンジとしきい値は画面外の文で読める（#535）。
		await expect(scaledCell.locator('.sr-only')).toHaveText(
			'レンジ 0 cnt〜65535 cnt（工学値レンジ）。しきい値: なし'
		);
		await expect(overCell.locator('.sr-only')).toHaveText(
			'レンジ 0〜1（しきい値の LL〜HH）。しきい値: HH 1、LL 0'
		);

		// 同じ値のしきい値（LL = L = H）は名前が重ならず 1 つにまとまる（#535）。
		const equalCell = cell(TAG_EQUAL);
		await expect(equalCell.locator('.mark-label')).toHaveText(['HH', 'LL/L/H']);
		await expect(equalCell.locator('.mark')).toHaveCount(2);

		// 不正なタグ: レンジはあるが棒を描かない（0 の高さにもしない）。「—」と設定不正。
		const invalidCell = cell(TAG_INVALID);
		await expect(invalidCell).toHaveAttribute('data-range', 'ok');
		await expect(invalidCell).toHaveAttribute('data-state', 'invalid');
		await expect(invalidCell.locator('.value')).toHaveText('—');
		await expect(invalidCell.locator('.track')).toHaveCount(1);
		await expect(invalidCell.locator('.fill')).toHaveCount(0);
		await expect(invalidCell).toContainText('設定不正（収集対象外）');
		await expect(invalidCell.getByRole('link', { name: 'タグ設定で直す' })).toBeVisible();

		// レンジが無いタグ: 棒の代わりに理由とタグ設定へのリンク。値と判定の文字は出す。
		const highCell = cell(TAG_HIGH);
		await expect(highCell).toHaveAttribute('data-range', 'unset');
		await expect(highCell).toContainText('レンジ未設定（タグ設定で工学値レンジを入れてください）');
		await expect(highCell.getByRole('link', { name: 'タグ設定を開く' })).toHaveAttribute(
			'href',
			'/tags'
		);
		await expect(highCell).toHaveAttribute('data-level', 'H');
		await expect(highCell).toContainText('H 上限以上');
		await expect(highCell.locator('.value')).toHaveText(/^\d+$/);
		await expect(highCell.locator('.track')).toHaveCount(0);

		// #551: bit のタグはレンジが無くても 0〜1。「レンジ未設定」にならず、目盛は False / True。
		const bitCell = cell(TAG_BIT);
		await expect(bitCell).toHaveAttribute('data-range', 'ok');
		await expect(bitCell).not.toContainText('レンジ未設定');
		await expect(bitCell.locator('.value')).toHaveText(/^(True|False)$/, { timeout: 20_000 });
		await expect(bitCell.locator('.tick')).toHaveText(['False', 'True']);
		await expect(bitCell.locator('.track')).toHaveCount(1);
		await expect(bitCell.locator('.sr-only')).toHaveText(
			'レンジ False〜True（bit の既定）。しきい値: なし'
		);
	});

	test('6. 計器: banto の Gauge が出て値が変わる。値なしは「—」、しきい値は data-level と文字', async () => {
		await tab(GROUP_GAUGE).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_GAUGE]}$`));
		await expect(panel().getByRole('list', { name: `${GROUP_GAUGE} の計器表示` })).toBeVisible();

		// Gauge の枠は role="img"（中の svg も暗黙の img なので、属性で枠だけを取る）。
		// 名前・値・単位は aria-label に載せている。
		const scaledGauge = cell(TAG_SCALED).locator('[role="img"]');
		await expect(scaledGauge).toHaveAttribute(
			'aria-label',
			new RegExp(`^${TAG_SCALED} \\d+ cnt$`),
			{
				timeout: 20_000
			}
		);
		const first = await scaledGauge.getAttribute('aria-label');
		await expect(scaledGauge).not.toHaveAttribute('aria-label', first ?? '', { timeout: 15_000 });
		await expect(cell(TAG_SCALED)).toHaveAttribute('data-level', 'none');
		await expect(cell(TAG_SCALED).locator('.sr-only')).toHaveText(
			'レンジ 0 cnt〜65535 cnt（工学値レンジ）。しきい値: なし'
		);

		const overCell = cell(TAG_OVER);
		await expect(overCell).toHaveAttribute('data-level', 'HH');
		await expect(overCell).toHaveAttribute('data-tone', 'danger');
		await expect(overCell).toContainText('HH 上上限以上');
		await expect(overCell).toContainText('レンジ上限超え');
		await expect(overCell.locator('[role="img"]')).toBeVisible();

		// 不正なタグ: Gauge は出る（レンジは LL..HH）が、弧を描かず「—」（0 にしない）。
		const invalidCell = cell(TAG_INVALID);
		await expect(invalidCell).toHaveAttribute('data-state', 'invalid');
		await expect(invalidCell.locator('[role="img"]')).toHaveAttribute(
			'aria-label',
			`${TAG_INVALID} —`
		);
		await expect(invalidCell.locator('svg text', { hasText: '—' })).toHaveCount(1);
		await expect(invalidCell).toContainText('設定不正（収集対象外）');

		// レンジが無いタグ: 計器を描かず、理由と値の文字。
		const highCell = cell(TAG_HIGH);
		await expect(highCell).toHaveAttribute('data-range', 'unset');
		await expect(highCell.locator('[role="img"]')).toHaveCount(0);
		await expect(highCell).toContainText('レンジ未設定');
		await expect(highCell).toHaveAttribute('data-level', 'H');
		await expect(highCell.locator('.value')).toHaveText(/^\d+$/);

		// #551: bit のタグも「レンジ未設定」にならず Gauge が出る。両端と値は False / True。
		const bitCell = cell(TAG_BIT);
		await expect(bitCell).toHaveAttribute('data-range', 'ok');
		await expect(bitCell).not.toContainText('レンジ未設定');
		await expect(bitCell.locator('[role="img"]')).toHaveAttribute(
			'aria-label',
			new RegExp(`^${TAG_BIT} (True|False)$`),
			{ timeout: 20_000 }
		);
		await expect(bitCell.locator('svg text.range-label')).toHaveText(['False', 'True']);
		await expect(bitCell.locator('.sr-only')).toHaveText(
			'レンジ False〜True（bit の既定）。しきい値: なし'
		);
	});

	test('7. トレンド: 線が伸び、時刻の目盛・選んだペンの帯・端末ごとの時間窓・止めた間の切れ目', async () => {
		await page.goto(`/monitor?group=${groupIds[GROUP_TREND]}`);
		await expect(tab(GROUP_TREND)).toHaveAttribute('aria-selected', 'true');
		const trend = panel().getByRole('region', { name: `${GROUP_TREND} のトレンド表示` });
		await expect(trend).toBeVisible({ timeout: 20_000 });
		// 線は系列の並び（ペンの並び）どおり。1 本目がランプ。
		const rampPath = trend.locator('.chart-host svg path[fill="none"]').first();
		const segments = async (): Promise<number> =>
			((await rampPath.getAttribute('d')) ?? '').split('M').length - 1;

		// 線が描かれて伸びる（`d` が変わる）。
		await expect(rampPath).toHaveAttribute('d', /L/, { timeout: 20_000 });
		const firstD = await rampPath.getAttribute('d');
		await expect(rampPath).not.toHaveAttribute('d', firstD ?? '', { timeout: 15_000 });
		// 時刻の目盛（時:分:秒。書式は端末のロケールに任せる。Playwright の既定の en-US では
		// 「03:41:50 AM」）。
		await expect(trend.locator('.chart-host svg text.x-tick').first()).toHaveText(
			/^\d{1,2}:\d{2}:\d{2}( [AP]M)?$/
		);

		// 帯の既定は、しきい値のある最初のペン（上限 = H 0、上が開いた注意の帯）。
		const rampPen = trend.getByRole('button', { name: new RegExp(`^${TAG_RAMP}（cnt）`) });
		const highPen = trend.getByRole('button', { name: new RegExp(`^${TAG_HIGH}`) });
		await expect(highPen).toHaveAttribute('aria-pressed', 'true');
		await expect(rampPen).toHaveAttribute('aria-pressed', 'false');
		await expect(trend.locator('.chart-host svg text.band-label')).toHaveText(['H']);
		await expect(trend.locator('.sr-only')).toContainText(
			`しきい値の帯: ${TAG_HIGH}（H 0 以上 注意）。`
		);
		// しきい値の無いペンを選ぶと帯は消え、文でもそう言う。
		await rampPen.click();
		await expect(rampPen).toHaveAttribute('aria-pressed', 'true');
		await expect(trend.locator('.chart-host svg text.band-label')).toHaveCount(0);
		await expect(trend.locator('.sr-only')).toContainText(
			`しきい値の帯: ${TAG_RAMP}（しきい値の設定なし）。`
		);
		await highPen.click();
		await expect(trend.locator('.chart-host svg text.band-label')).toHaveText(['H']);

		// 時間窓: 既定（グループの属性。作成時に何も渡さないとサーバーが 10 分を入れる）→
		// 1 分。端末に覚え、グループの定義には書かない。
		const groupAttributes = async (): Promise<unknown> => {
			const res = await page.request.get(`/api/display-groups/${groupIds[GROUP_TREND]}`, {
				headers
			});
			expect(res.ok(), `GET /api/display-groups/${groupIds[GROUP_TREND]}`).toBe(true);
			return ((await res.json()) as { attributes: unknown }).attributes;
		};
		const attributesBefore = await groupAttributes();
		const windowSelect = trend.getByLabel('時間窓');
		await expect(windowSelect).toHaveValue('600');
		await windowSelect.selectOption('60');
		await expect(windowSelect).toHaveValue('60');
		await page.reload();
		await expect(trend.getByLabel('時間窓')).toHaveValue('60', { timeout: 20_000 });
		expect(await groupAttributes()).toEqual(attributesBefore);

		// 収集を止めている間の刻みは null（0 ではない）で、線が切れる。履歴の待ち（2 秒）
		// の後で線が出ていることを確かめてから止める。
		await expect(rampPath).toHaveAttribute('d', /L/, { timeout: 20_000 });
		await page.waitForTimeout(3_000);
		const before = await segments();
		expect(before).toBeGreaterThanOrEqual(1);
		const stop = await page.request.post('/api/collect/stop', { headers });
		expect(stop.ok(), `POST /api/collect/stop が ${stop.status()}`).toBe(true);
		await expect(panel().getByText('収集が動いていません')).toBeVisible({ timeout: 15_000 });
		// 刻み（1 分窓・周期 500ms なら 1 秒）を何個か空ける。
		await page.waitForTimeout(4_000);
		const start = await page.request.post('/api/collect/start', { headers });
		expect(start.ok(), `POST /api/collect/start が ${start.status()}`).toBe(true);
		await expect(trend).toBeVisible({ timeout: 20_000 });
		await expect.poll(segments, { timeout: 20_000 }).toBeGreaterThan(before);
	});

	test('8. bit のタグ: デジタルは True / False、トレンドは全ペン bit なら縦軸が False / True（#551）', async () => {
		await page.goto(`/monitor?group=${groupIds[GROUP_BIT]}`);
		const bitCell = cell(TAG_BIT);
		const value = bitCell.locator('.value');
		await expect(value).toHaveText(/^(True|False)$/, { timeout: 20_000 });
		// 値が変わる（コイルがトグルする）。0 / 1 の数値は出さない。
		const seen = new Set<string>();
		await expect
			.poll(
				async () => {
					const text = (await value.textContent()) ?? '';
					expect(text).toMatch(/^(True|False)$/);
					seen.add(text);
					return seen.size;
				},
				{ timeout: 30_000 }
			)
			.toBe(2);
		await expect(bitCell).toContainText('正常');

		await page.goto(`/monitor?group=${groupIds[GROUP_BIT_TREND]}`);
		const trend = panel().getByRole('region', { name: `${GROUP_BIT_TREND} のトレンド表示` });
		await expect(trend).toBeVisible({ timeout: 20_000 });
		const yTicks = trend.locator('.chart-host svg text.y-tick');
		await expect
			.poll(async () => (await yTicks.allTextContents()).map((t) => t.trim()), {
				timeout: 30_000
			})
			.toEqual(expect.arrayContaining(['False', 'True']));
		// 間の目盛（0.2 刻み）の数字は出さない。
		for (const text of await yTicks.allTextContents()) {
			expect(text.trim()).toMatch(/^(False|True)?$/);
		}
		await expect(trend.locator('.sr-only')).toContainText('縦軸は False（0）と True（1）で');
	});
});
