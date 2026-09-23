/**
 * R1-C の完了条件「シミュレータ相手に、設定 → 収集開始 → データファイル生成 →
 * イベント記録まで一巡」の実 DOM 固定（C-4、2026-09-23。docs/r1-plan.md）。
 *
 * 相手の PLC は `playwright.config.ts` の 2 つ目の `webServer` が起動する
 * **開発用 PLC**（`apps/chronogazer/core/examples/dev_plc.rs`、Modbus TCP、
 * 127.0.0.1:8803、保持レジスタ 40001-40016 にランプ波）。ChronoGazer から見ると
 * **普通の Modbus TCP 接続**で、製品の接続単位シミュレーション
 * （`simulation`）は使わない - その値は tstore に記録されない
 * （`crates/banto-collect/src/task.rs` の `if ctx.simulation { return; }`
 * 付近）ため、データファイル生成まで含む一巡はこの経路でしか確かめられない
 * （製品の `simulation` の開放は #413 で別途扱う）。
 *
 * ## 画面で操作するところ・REST を使うところ
 *
 * - **画面**: 接続・収集グループ・タグの作成（`/tags`）、収集の開始・停止
 *   （`/settings/collect`）、状態・接続状態・イベントの確認（`/settings/collect`
 *   と `/events`）。利用者が実際に踏む経路を一巡で通すのがこのスペックの目的
 *   なので、UI がある操作はすべて画面から行う。
 * - **REST**: 後始末（収集の停止・作ったタグ/グループ/接続の削除）と、下の
 *   「先行スペックの接続を一時的に無効にする」操作、作った接続の `id` の取得
 *   （画面の接続状態の行は `conn:<id>` で出るため）。後始末は**途中で失敗した
 *   テストの後でも確実に走らせる**必要があり、画面の状態（開いているダイアログ・
 *   選択中の行）に左右されない REST のほうが確実。
 *
 * ## 先行スペックの接続を一時的に無効にする（なぜ必要か）
 *
 * `tags.spec.ts` は PLC接続 `E2E-PLC1`（modbus-tcp、192.168.11.200）とその下の
 * タグ `D3000` を作ったまま残す。`D3000` は Modbus のアドレスとして解釈できない
 * ので、それが有効なまま収集を開始すると**構成の組み立て自体が失敗して**
 * （`banto_collect::build_config` の `CollectError::Config`）状態が
 * 「開始に失敗しました」になる。加えて、実在しうる機器のアドレスへ E2E から
 * 接続しに行くことになる。そこで `beforeAll` で**このスペックのもの以外の有効な
 * 接続を無効化**し（収集対象から外れる - 無効な接続の下のグループ・タグは
 * 構成に入らない）、`afterAll` で**元に戻す**。削除はしない（他のスペックの
 * データを消さない）。
 *
 * ## 後始末
 *
 * `beforeAll` の先頭（前回の失敗で残ったもの）と `afterAll`（今回の分）の両方で
 * 行う。**個々の後始末の失敗は残りを止めない** - 全部試してから、失敗があれば
 * まとめて落とす（`afterAll` は途中のテストが落ちても走る）。
 *
 * ## データファイルの確認
 *
 * `data.dir` の既定 `./data` は DB ファイルの置き場を基準に解決される
 * （`banto-serve` の `resolve_data_dir`）。DB の置き場は `playwright.config.ts`
 * が 1 回だけ作って `BANTO_E2E_DB_DIR` に入れた一時ディレクトリで、ワーカーは
 * それを継承する。ここでは Node の `fs` で `<dbDir>/data` に
 * `YYYYMMDD-NNN.sqlite3` ができたことを確かめる（中のサンプル行までは Rust の
 * `apps/chronogazer/core/tests/collect_roundtrip.rs` が確かめている）。
 *
 * ## ファイル名（実行順）
 *
 * `playwright.config.ts` は `workers: 1`/`fullyParallel: false` でファイル名の
 * 辞書順に実行する。`user-settings-collect.spec.ts` は「収集対象がありません」
 * （起動時の自動開始が空のレジストリで終わった状態）を前提にしているので、
 * このスペックは**それより後**でなければならない。`user-settings-collect-*`
 * だと `-` < `.` で前に来てしまうため、`user-simulator-*`（`se` < `si`）にして
 * `user-settings-*` の全部より後、つまり**スイートの最後**に置いた
 * （`pnpm exec playwright test --config=e2e/playwright.config.ts --list` で確認）。
 */
import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// playwright.config.ts の `DEV_PLC_PORT` と同じ値。
const DEV_PLC_PORT = 8803;

const CONNECTION_NAME = 'E2E-開発用PLC';
const GROUP_NAME = 'E2E一巡グループ';
const TAG_NAME = 'E2E一巡ランプ';

const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };

/** REST に付けるヘッダー（CSRF + Bearer。{@link fetchApiHeaders} が作る）。 */
type ApiHeaders = Record<string, string>;

/** `YYYYMMDD-NNN.sqlite3`（banto-tstore の `schema.rs` のファイル名）。 */
const DATA_FILE_PATTERN = /^\d{8}-\d{3}\.sqlite3$/;

interface ConnectionRow {
	id: number;
	name: string;
	protocol: string;
	host: string;
	port: number;
	unitId: number;
	enabled: boolean;
	wordOrder: string;
}
interface NamedRow {
	id: number;
	name: string;
}

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

/**
 * REST 用のヘッダーを作る。画面のログインは Bearer トークンをブラウザの
 * ストレージに置くだけで Cookie を使わないので、`page.request` は画面の
 * セッションを共有しない - REST は自分で `POST /api/auth/login` して
 * トークンを得る（`e2e/tests-banto-hub/banto-hub-auth.ts` の
 * `fetchAuthToken` と同じ作法）。
 */
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

/** 接続の `enabled` だけを変える（PUT は全項目を送る形なので、読んだ値をそのまま返す）。 */
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
		throw new Error(
			`PUT /api/plc-connections/${conn.id}（enabled=${enabled}）が ${res.status()}: ${await res.text()}`
		);
	}
}

/**
 * 1 手ずつ試し、失敗は集めて返す（**1 つの失敗で残りを止めない**）。
 * 呼び出し側が最後にまとめて落とす。
 */
async function attempt(failures: string[], what: string, step: () => Promise<void>): Promise<void> {
	try {
		await step();
	} catch (err) {
		failures.push(`${what}: ${err instanceof Error ? err.message : String(err)}`);
	}
}

/**
 * 収集を止め、このスペックが作ったタグ → グループ → 接続を名前で探して消す
 * （FK の依存順）。見つからないものは何もしない（冪等）。
 */
async function cleanupFixtures(request: APIRequestContext, headers: ApiHeaders): Promise<string[]> {
	const failures: string[] = [];
	// 走ったままだと、消した接続を読みに行き続ける。停止は冪等。
	await attempt(failures, '収集の停止', async () => {
		const res = await request.post('/api/collect/stop', { headers });
		if (!res.ok()) throw new Error(`${res.status()}: ${await res.text()}`);
	});
	const deleteByName = async (listUrl: string, name: string): Promise<void> => {
		const rows = await getList<NamedRow>(request, headers, listUrl);
		for (const row of rows.filter((r) => r.name === name)) {
			const res = await request.delete(`${listUrl}/${row.id}`, { headers });
			if (res.status() !== 204 && res.status() !== 404) {
				throw new Error(`DELETE ${listUrl}/${row.id} が ${res.status()}: ${await res.text()}`);
			}
		}
	};
	await attempt(failures, 'タグの削除', () => deleteByName('/api/tags', TAG_NAME));
	await attempt(failures, '収集グループの削除', () =>
		deleteByName('/api/collection-groups', GROUP_NAME)
	);
	await attempt(failures, 'PLC接続の削除', () =>
		deleteByName('/api/plc-connections', CONNECTION_NAME)
	);
	return failures;
}

/**
 * `/settings/collect` の状態の見出し（`CollectSection.svelte` の `p.status`）。
 * 操作の完了通知（`operation-notice`）にも同じ状態の文言が入るので、
 * ページ全体の `getByText` では 2 か所に一致する。
 */
function statusLine(page: Page): Locator {
	return page.locator('p.status');
}

function dataFiles(dataDir: string): string[] {
	if (!fs.existsSync(dataDir)) return [];
	return fs.readdirSync(dataDir).filter((name) => DATA_FILE_PATTERN.test(name));
}

test.describe.serial('chronogazer 開発用 PLC 相手の収集の一巡（R1-C の C-4）', () => {
	let page: Page;
	let dbDir: string;
	let dataDir: string;
	/** beforeAll で無効化した、他のスペックの接続（afterAll で有効に戻す）。 */
	let disabledByUs: ConnectionRow[] = [];
	let connectionId: number;
	let apiHeaders: ApiHeaders;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
		apiHeaders = await fetchApiHeaders(page.request);

		// 前回の失敗で残ったものを先に片付ける。ここで失敗したら先へ進まない
		// （古いフィクスチャが残ったままでは名前の UNIQUE で作成が落ちる）。
		const failures = await cleanupFixtures(page.request, apiHeaders);
		expect(failures, `前回分の後始末に失敗しました: ${failures.join(' / ')}`).toEqual([]);

		// このスペックのもの以外の有効な接続を無効化する（ファイル冒頭の doc）。
		const connections = await getList<ConnectionRow>(
			page.request,
			apiHeaders,
			'/api/plc-connections'
		);
		for (const conn of connections.filter((c) => c.enabled && c.name !== CONNECTION_NAME)) {
			await setConnectionEnabled(page.request, apiHeaders, conn, false);
			disabledByUs.push(conn);
		}

		// ワーカーが見ている一時ディレクトリが、banto-serve が使っているものと
		// 同じであること（config を評価し直して別のディレクトリを作っていない
		// こと）を、DB ファイルの存在で確かめる。
		dbDir = process.env.BANTO_E2E_DB_DIR ?? '';
		expect(dbDir, 'BANTO_E2E_DB_DIR がワーカーに渡っていない').not.toBe('');
		expect(
			fs.existsSync(path.join(dbDir, 'chronogazer-e2e.sqlite3')),
			`${dbDir} に banto-serve の DB が無い（別の一時ディレクトリを見ている）`
		).toBe(true);
		dataDir = path.join(dbDir, 'data');
	});

	test.afterAll(async () => {
		// beforeAll がトークンを得る前に落ちていても、後始末は試みる。
		if (!apiHeaders) apiHeaders = await fetchApiHeaders(page.request);
		const failures = await cleanupFixtures(page.request, apiHeaders);
		for (const conn of disabledByUs) {
			await attempt(failures, `接続 ${conn.name} を有効に戻す`, () =>
				setConnectionEnabled(page.request, apiHeaders, conn, true)
			);
		}
		disabledByUs = [];
		await page.close();
		expect(failures, `後始末に失敗しました: ${failures.join(' / ')}`).toEqual([]);
	});

	test('1. 開発用 PLC への接続・収集グループ・タグを画面から作る', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: 'タグ設定' })).toBeVisible();

		const connSection = page.locator('section.registry-section').nth(0);
		const connForm = connSection.locator('div.create');
		await connForm.getByLabel('名前').fill(CONNECTION_NAME);
		await connForm.getByLabel('プロトコル').selectOption('modbus-tcp');
		await connForm.getByLabel('ホスト').fill('127.0.0.1');
		await connForm.getByLabel('ポート').fill(String(DEV_PLC_PORT));
		await connForm.getByRole('button', { name: '作成' }).click();
		await expect(connSection.locator('div.list').getByText(CONNECTION_NAME)).toBeVisible();

		const groupSection = page.locator('section.registry-section').nth(1);
		const groupForm = groupSection.locator('div.create');
		await groupForm.getByLabel('名前').fill(GROUP_NAME);
		await groupForm
			.getByLabel('PLC接続')
			.selectOption({ label: `${CONNECTION_NAME}（modbus-tcp）` });
		await groupForm.getByLabel('収集周期').selectOption({ label: '100ms' });
		await groupForm.getByRole('button', { name: '作成' }).click();
		await expect(groupSection.locator('div.list').getByText(GROUP_NAME)).toBeVisible();

		const tagSection = page.locator('section.registry-section').nth(2);
		const tagForm = tagSection.locator('div.create');
		await tagForm.getByLabel('名前').fill(TAG_NAME);
		await tagForm.getByLabel('収集グループ').selectOption({ label: GROUP_NAME });
		// 保持レジスタ 40001 = dev_plc のランプ波の先頭。
		await tagForm.getByLabel('デバイスアドレス').fill('40001');
		await tagForm.getByLabel('データ型').selectOption('u16');
		await tagForm.getByRole('button', { name: '作成' }).click();
		await expect(tagSection.locator('div.list').getByText(TAG_NAME)).toBeVisible();

		const created = (
			await getList<ConnectionRow>(page.request, apiHeaders, '/api/plc-connections')
		).find((c) => c.name === CONNECTION_NAME);
		expect(created, '作った接続が一覧に無い').toBeDefined();
		expect(created?.port).toBe(DEV_PLC_PORT);
		connectionId = created?.id ?? -1;
	});

	test('2. 収集を開始すると「収集中」になり、開発用 PLC への接続が「接続中」になる', async () => {
		await page.goto('/settings/collect');
		// 起動時の自動開始は空のレジストリで終わっている（レジストリの CRUD では
		// 自動再起動しない - C-2 の決定）ので、明示的に開始する。
		await expect(statusLine(page)).toHaveText('状態: 収集対象がありません');
		await page.getByRole('button', { name: '収集を開始' }).click();

		await expect(statusLine(page)).toHaveText('状態: 収集中（グループ1件 / タグ1件）');
		const row = page
			.locator('table.collect-connections tbody tr')
			.filter({ hasText: `conn:${connectionId}` });
		// 接続状態は画面のポーリングで更新される。既定の接続タイムアウト（3 秒）
		// より長く待つ。
		await expect(row).toContainText('接続中', { timeout: 20_000 });
		await expect(row).not.toContainText('再接続中');
	});

	test('3. データディレクトリに時系列ファイルができる', async () => {
		await expect
			.poll(() => dataFiles(dataDir), {
				message: `${dataDir} に YYYYMMDD-NNN.sqlite3 ができること`,
				timeout: 20_000
			})
			.not.toEqual([]);
	});

	test('4. イベント画面に「収集開始」と、この接続の「PLC接続」が出る', async () => {
		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		const started = page.getByRole('row').filter({ hasText: '収集開始' });
		const connected = page
			.getByRole('row')
			.filter({ hasText: 'PLC接続' })
			.filter({ hasText: `conn:${connectionId}` });
		// イベントの書き込みは接続状態の更新の直後に非同期で入るので、見えるまで
		// 「再読み込み」で新しい世代を取り直す。
		await expect(async () => {
			await page.getByRole('button', { name: '再読み込み' }).click();
			await expect(started.first()).toBeVisible({ timeout: 1_000 });
			await expect(connected.first()).toBeVisible({ timeout: 1_000 });
		}).toPass({ timeout: 15_000 });
	});

	test('5. 停止すると「停止」になり、イベント画面に「収集停止」が出る', async () => {
		await page.goto('/settings/collect');
		await expect(statusLine(page)).toHaveText(/^状態: 収集中/);
		await page.getByRole('button', { name: '収集を停止' }).click();
		await expect(statusLine(page)).toHaveText('状態: 停止');

		await page.goto('/events');
		const stopped = page.getByRole('row').filter({ hasText: '収集停止' });
		await expect(async () => {
			await page.getByRole('button', { name: '再読み込み' }).click();
			await expect(stopped.first()).toBeVisible({ timeout: 1_000 });
		}).toPass({ timeout: 15_000 });

		// 停止後もデータファイルは残っている（最終 flush 済み）。
		expect(dataFiles(dataDir)).not.toEqual([]);
	});
});
