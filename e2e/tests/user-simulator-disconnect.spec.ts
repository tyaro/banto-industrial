/**
 * 監視画面（`/monitor`）の断線 → Bad → 復旧の実 DOM 固定（R1-D の D-4、Q9）。
 *
 * 固定したい受入条件（docs/r1-plan.md の R1-D の完了条件「断線 → Bad 表示 →
 * 復旧が確認できる」）:
 * 1. 開発用 PLC 相手に収集を始めると、デジタル表示に値が出る（品質「正常」）。
 * 2. 開発用 PLC のプロセスを止める（断線）と、デジタル表示は**最後の値を出さず**
 *    「—」と品質「通信エラー」、最後に受け取った時刻を出す（Q2）。
 * 3. 断線の間にトレンドを開くと、断線の前の線は描かれている。開発用 PLC を
 *    同じポートで起動し直す（復旧）と、**新しい線の切れ端**が始まる（`path` の
 *    `d` の `M` が増える = 断線の区間は `null` で線が切れ、0 にしない）。
 * 4. 復旧後のデジタル表示は数値と「正常」に戻り、値が変わり続ける。
 *
 * ## 開発用 PLC はこのスペックが自分で起動・停止する
 *
 * `playwright.config.ts` の `webServer` の開発用 PLC（8803）はスイート全体で
 * 共有していて、止めると他のスペックが壊れる。このスペックは**同じバイナリ**
 * （`target/debug/examples/dev_plc`、`cargo build -p chronogazer-core --example
 * dev_plc` でビルド済みのもの）を**別のポート 8806** で子プロセスとして起動し、
 * それを止めて・起動し直す。8806 は e2e/README.md のポート表の続きの番号
 * （8798〜8805 は使用中。オーナーが別の作業で使う 8799・4173 とも重ならない）。
 *
 * **子プロセスは必ず止める**: `afterAll`（途中のテストが落ちても走る）で止め、
 * ワーカーのプロセスが先に終わる場合に備えて `process.once('exit')` でも止める。
 * 起動の待ちは「TCP で接続できるまで」、停止の待ちは「プロセスの終了」で、
 * 固定の sleep で済ませない。
 *
 * ## ファイル名（実行順）
 *
 * 収集を開始するので、「起動時の自動開始が空のレジストリで終わった状態」を
 * 前提にする `user-settings-collect.spec.ts` より**後**に来る名前にする
 * （`user-simulator-` は `user-settings-` より後）。`user-simulator-monitor.spec.ts`
 * のテスト 1 は「収集が動いていない」を前提にするので、`afterAll` で収集を止める
 * （`d` < `m` で monitor の直前に来る）。
 *
 * ## フィクスチャ
 *
 * monitor・roundtrip と同じ作法: 接続・収集グループ・タグ・表示グループは REST で
 * 作り、`beforeAll` の先頭と `afterAll` の両方で名前から掃除する（チェックリスト
 * §3）。**他のスペックの有効な接続は一時的に無効にし**、`afterAll` で戻す
 * （`tags.spec.ts` は LAN 上の実在しうるアドレスを指す接続を残す - E2E から実在しうる機器へ
 * 接続しに行かない）。
 *
 * ## 待ちの上限
 *
 * 断線を検知するのは次の読み取りの失敗（収集周期 500ms、応答の上限 1 秒）。
 * 復旧は banto-collect の再接続の待ち（1 秒から倍々、上限 30 秒、
 * `BackoffConfig::default`）次第で、断線が 10 秒前後なら 8 秒程度の待ちになる。
 * 余裕を見て復旧の待ちは 45 秒にする。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

/** このスペック専用の開発用 PLC のポート（上の doc）。 */
const OWN_DEV_PLC_PORT = 8806;
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const devPlcBin = path.join(
	repoRoot,
	'target',
	'debug',
	'examples',
	process.platform === 'win32' ? 'dev_plc.exe' : 'dev_plc'
);

const CONNECTION_NAME = 'E2E-DISC-PLC';
const COLLECTION_GROUP_NAME = 'E2E-DISC-収集';
const TAG_RAMP = 'E2E-DISC-ランプ';
const GROUP_DIGITAL = 'E2E-DISC-デジタル';
const GROUP_TREND = 'E2E-DISC-トレンド';
const TAG_NAMES = [TAG_RAMP];
const GROUP_NAMES = [GROUP_DIGITAL, GROUP_TREND];

const RECOVERY_TIMEOUT_MS = 45_000;

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

// --- 開発用 PLC の子プロセス ------------------------------------------------

/** `port` に TCP で接続できるか（1 回だけ試す）。 */
function canConnect(port: number): Promise<boolean> {
	return new Promise((resolve) => {
		const socket = net.connect({ host: '127.0.0.1', port });
		const done = (ok: boolean) => {
			socket.destroy();
			resolve(ok);
		};
		socket.once('connect', () => done(true));
		socket.once('error', () => done(false));
		socket.setTimeout(1_000, () => done(false));
	});
}

/** 起動した開発用 PLC。`stop()` は何度呼んでもよい。 */
interface DevPlc {
	stop(): Promise<void>;
}

/**
 * 開発用 PLC を起動し、待ち受けを始めるまで待つ。ポートが既に使われていれば
 * 起動前に落とす（他のプロセスの PLC を自分のものと取り違えない）。
 */
async function startDevPlc(port: number): Promise<DevPlc> {
	expect(await canConnect(port), `ポート ${port} が既に使われています`).toBe(false);
	const child: ChildProcess = spawn(devPlcBin, ['--protocol', 'modbus', '--port', String(port)], {
		stdio: ['ignore', 'pipe', 'pipe']
	});
	let output = '';
	child.stdout?.on('data', (chunk: Buffer) => (output += chunk.toString()));
	child.stderr?.on('data', (chunk: Buffer) => (output += chunk.toString()));
	const exited = new Promise<void>((resolve) => {
		if (child.exitCode !== null || child.signalCode !== null) resolve();
		else child.once('exit', () => resolve());
	});
	// ワーカーが先に終わっても子を残さない（afterAll の後ろ盾）。
	const killOnExit = () => child.kill();
	process.once('exit', killOnExit);

	const stop = async (): Promise<void> => {
		process.removeListener('exit', killOnExit);
		if (child.exitCode === null && child.signalCode === null) child.kill();
		await exited;
	};
	try {
		await expect
			.poll(
				async () => {
					if (child.exitCode !== null) {
						throw new Error(`dev_plc が終了しました（${child.exitCode}）: ${output}`);
					}
					return canConnect(port);
				},
				{ message: `dev_plc が ${port} で待ち受けを始めない: ${output}`, timeout: 15_000 }
			)
			.toBe(true);
	} catch (err) {
		await stop();
		throw err;
	}
	return { stop };
}

// --- REST ---------------------------------------------------------------------

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

test.describe.serial('chronogazer 監視画面の断線 → Bad → 復旧（R1-D の D-4）', () => {
	let page: Page;
	let headers: ApiHeaders;
	let disabledByUs: ConnectionRow[] = [];
	let devPlc: DevPlc | null = null;
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

		devPlc = await startDevPlc(OWN_DEV_PLC_PORT);

		const conn = await postJson<NamedRow>(page.request, headers, '/api/plc-connections', {
			name: CONNECTION_NAME,
			protocol: 'modbus-tcp',
			host: '127.0.0.1',
			port: OWN_DEV_PLC_PORT,
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
		const ramp = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			collectionGroupId: cg.id,
			name: TAG_RAMP,
			address: '40001',
			dataType: 'u16',
			decimals: 0,
			enabled: true,
			unit: 'cnt'
		});
		for (const [name, kind] of [
			[GROUP_DIGITAL, 'digital'],
			[GROUP_TREND, 'trend']
		] as const) {
			const created = await postJson<NamedRow>(page.request, headers, '/api/display-groups', {
				name,
				kind,
				attributes: {},
				pens: [{ tagId: ramp.id, colorSlot: null }]
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
		const failures: string[] = [];
		// 子プロセスは何があっても止める（REST の後始末より先に）。
		await attempt(failures, '開発用 PLC の停止', async () => {
			await devPlc?.stop();
			devPlc = null;
		});
		if (!headers) headers = await fetchApiHeaders(page.request);
		failures.push(...(await cleanupFixtures(page.request, headers)));
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
	const rampCell = () => panel().getByRole('listitem').filter({ hasText: TAG_RAMP });

	test('1. 収集を始めると、デジタル表示に値と「正常」が出る', async () => {
		const res = await page.request.post('/api/collect/start', { headers });
		expect(res.ok(), `POST /api/collect/start が ${res.status()}`).toBe(true);
		await page.goto(`/monitor?group=${groupIds[GROUP_DIGITAL]}`);
		await expect(tab(GROUP_DIGITAL)).toHaveAttribute('aria-selected', 'true');
		await expect(rampCell().locator('.value')).toHaveText(/^\d+$/, { timeout: 20_000 });
		await expect(rampCell().locator('.state')).toHaveText('正常');
		await expect(rampCell()).toHaveAttribute('data-state', 'good');
	});

	test('2. 開発用 PLC を止めると、最後の値を出さず「—」と「通信エラー」、最後に受け取った時刻', async () => {
		await devPlc?.stop();
		devPlc = null;
		expect(await canConnect(OWN_DEV_PLC_PORT), '止めた後もポートが開いている').toBe(false);

		await expect(rampCell()).toHaveAttribute('data-state', 'bad', { timeout: 20_000 });
		await expect(rampCell().locator('.value')).toHaveText('—');
		await expect(rampCell().locator('.state')).toHaveText('通信エラー');
		await expect(rampCell().locator('.last-received')).toHaveText(/^最後に受け取った値: \S/);
	});

	test('3. 断線中のトレンドは前の線を残し、復旧すると新しい線の切れ端が始まる', async () => {
		await tab(GROUP_TREND).click();
		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupIds[GROUP_TREND]}$`));
		const trend = panel().getByRole('region', { name: `${GROUP_TREND} のトレンド表示` });
		await expect(trend).toBeVisible({ timeout: 20_000 });
		const line = trend.locator('.chart-host svg path[fill="none"]').first();
		const segments = async (): Promise<number> =>
			((await line.getAttribute('d')) ?? '').split('M').length - 1;

		// 断線の前の区間は履歴（D-3a）から描かれる。履歴の待ち（2 秒）を見込む。
		await expect(line).toHaveAttribute('d', /L/, { timeout: 20_000 });
		await page.waitForTimeout(3_000);
		const before = await segments();
		expect(before).toBeGreaterThanOrEqual(1);

		devPlc = await startDevPlc(OWN_DEV_PLC_PORT);
		// 断線の区間は null（0 ではない）なので、復旧後の値は新しい切れ端になる。
		await expect.poll(segments, { timeout: RECOVERY_TIMEOUT_MS }).toBeGreaterThan(before);
	});

	test('4. 復旧後のデジタル表示は数値と「正常」に戻り、値が変わり続ける', async () => {
		await tab(GROUP_DIGITAL).click();
		await expect(tab(GROUP_DIGITAL)).toHaveAttribute('aria-selected', 'true');
		await expect(rampCell()).toHaveAttribute('data-state', 'good', {
			timeout: RECOVERY_TIMEOUT_MS
		});
		const value = rampCell().locator('.value');
		await expect(value).toHaveText(/^\d+$/);
		await expect(rampCell().locator('.state')).toHaveText('正常');
		await expect(rampCell().locator('.last-received')).toHaveCount(0);
		const first = await value.textContent();
		await expect(value).not.toHaveText(first ?? '', { timeout: 15_000 });
	});
});
