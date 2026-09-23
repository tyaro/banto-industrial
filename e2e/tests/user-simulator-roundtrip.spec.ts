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
 * （製品の `simulation` は #413 で設定できるようにしたが、記録されない約束は
 * そのままなので、一巡はこの経路で確かめる）。
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
 * 他のスペックは作った接続を残すことがある（例: `tags.spec.ts` は PLC接続
 * `E2E-PLC1`（modbus-tcp、192.168.11.200）とその下のタグ `40001` を残す）。
 * それが有効なまま収集を開始すると、**実在しうる機器のアドレスへ E2E から
 * 接続しに行く**うえ、その接続の状態やイベントが一巡の確認に混ざる。そこで
 * `beforeAll` で**このスペックのもの以外の有効な接続を無効化**し（収集対象から
 * 外れる - 無効な接続の下のグループ・タグは構成に入らない）、構成を一巡の
 * ものだけにする。`afterAll` で**元に戻す**。削除はしない（他のスペックの
 * データを消さない）。
 *
 * （経緯: 当初は `tags.spec.ts` が Modbus 接続の下に `D3000` を残し、構成の
 * 組み立て自体が `CollectError::Config` で失敗するのを避ける目的もあった。
 * #414 段階1（PR #418）でそのタグは保存できなくなり `40001` に直ったが、上の
 * 理由でこの無効化は防御として残す。）
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
 * が実行ごとに 1 回だけ作った一時ディレクトリで、ワーカーは内部用の変数
 * `CHRONOGAZER_E2E_RUN_DIR`（`e2e/chronogazer-e2e-run-dir.ts`）で受け取る。
 * ここでは Node の `fs` で `<dbDir>/data` の `YYYYMMDD-NNN.sqlite3`（または
 * その WAL `-wal`）に**この試行の収集中に書き込みがあったこと**（大きさか
 * 更新時刻が変わったこと）を確かめる（中のサンプル行までは Rust の
 * `apps/chronogazer/core/tests/collect_roundtrip.rs` が確かめている）。
 *
 * ## CI の再試行（`retries: 1`）で同じサーバーに 2 回目が走る
 *
 * `describe.serial` は失敗すると**グループ全体**を同じサーバー・同じ DB で
 * やり直す（#412 オーナーレビュー、2026-09-23）。そのため**初回だけ成り立つ
 * 前提を置かない**:
 *
 * - 開始前の状態は「収集対象がありません」（起動直後）**か**「停止」
 *   （前回の試行の `afterAll` が止めた）。起動直後が「収集対象がありません」で
 *   あることの確認は `user-settings-collect.spec.ts` の担当。
 * - データファイルは同じ日・同じ構成なら**同じファイルを使い回す**
 *   （banto-tstore の `resolve_file`）ので、「ファイルがある」だけでは前回の
 *   試行のファイルで通ってしまう。書き込みの確認は**この試行の中での変化**で
 *   見る（上の節）。
 * - イベント一覧には前回の試行の「収集開始」「PLC接続」「収集停止」が残る。
 *   また接続を消して作り直すと、SQLite は同じ `id` を再び振ることがある
 *   （`conn:<id>` も一致しうる）。そこで**操作の直前に REST でイベントの最新
 *   `id`（`asOfId`）を控え**、それより新しいイベントを REST で探し、その行が
 *   画面に**その時刻の表示で**出ていることを確かめる。
 *
 * ## 不正なタグが残っていても、正常なタグは収集される（#414 段階2、テスト 6〜8）
 *
 * 2026-09-23 オーナー決定「開始時に不正なタグ・接続だけを外し、残りを動かす。
 * 不正なものがあることは必ず分かるようにする」の一巡。一巡のグループに、
 * **保存時の検証（#418）より前に入った**不正なタグ（Modbus 接続の下の
 * `D3000`）を足して収集を開始し、
 *
 * - 開始は失敗せず「収集中（グループ1件 / タグ1件）」になり、データファイルに
 *   書き込みが入る（正常なタグは収集される）、
 * - `/settings/collect` に「除外あり（1 件）」と、種類・名前・理由の一覧と
 *   `/tags` へのリンクが出る、
 * - 現在値は正常なタグが `good`、不正なタグが `value: null`・`invalid`、
 * - `/tags` に「収集の開始時に外される設定」が出る、
 * - 停止すると収集の画面から除外の一覧が消える（古い一覧を残さない）
 *
 * を確かめる。
 *
 * **不正なタグの作り方**: REST は #418 で保存時に拒否するので、**正常な
 * アドレスで作ってから DB の行を直接書き換える**（Node 組み込みの
 * `node:sqlite` で `UPDATE tags SET address = ...` を 1 文だけ）。これが
 * 「検証をくぐった既存データ」そのもの。DB は `playwright.config.ts` が
 * この実行のために作った一時ディレクトリの中（所有マーカーで確かめて
 * いる。`beforeAll`）で、banto-serve が開いたままなのでロック待ち
 * （`busy_timeout`）を入れる。外したタグの**履歴が null で読める**ことは
 * 画面から見えないので、Rust の `apps/chronogazer/core/tests/exclusion_roundtrip.rs`
 * が確かめる。
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
import { DatabaseSync } from 'node:sqlite';
import { RUN_DIR_ENV, RUN_TOKEN_ENV, ownsRunDir } from '../chronogazer-e2e-run-dir';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

// playwright.config.ts の `DEV_PLC_PORT` と同じ値。
const DEV_PLC_PORT = 8803;

const CONNECTION_NAME = 'E2E-開発用PLC';
const GROUP_NAME = 'E2E一巡グループ';
const TAG_NAME = 'E2E一巡ランプ';
/**
 * #414 段階2（テスト 6〜8）: **保存時の検証（#418）より前に入った**不正な
 * タグ。REST では作れないので、正常なアドレスで作ってから DB の行を直接
 * 書き換える（ファイル冒頭の doc「不正なタグの作り方」）。
 */
const LEGACY_TAG_NAME = 'E2E検証前の不正タグ';
/** Modbus 接続の下の MELSEC 表記（#418 以前に実際に残っていた形）。 */
const LEGACY_ADDRESS = 'D3000';
const DB_FILE_NAME = 'chronogazer-e2e.sqlite3';

const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };

/** REST に付けるヘッダー（CSRF + Bearer。{@link fetchApiHeaders} が作る）。 */
type ApiHeaders = Record<string, string>;

/** `YYYYMMDD-NNN.sqlite3`（banto-tstore の `schema.rs` のファイル名）。 */
const DATA_FILE_PATTERN = /^\d{8}-\d{3}\.sqlite3$/;
/**
 * 上に加えて WAL（`-wal`）。banto-tstore は WAL モードで書く
 * （`schema::connect_writable`）ので、収集中の書き込みはまず `-wal` に入り、
 * 本体の大きさ・更新時刻はチェックポイントまで変わらないことがある。
 */
const DATA_FILE_OR_WAL_PATTERN = /^(\d{8}-\d{3}\.sqlite3)(-wal)?$/;

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

/**
 * 接続の `enabled` だけを変える（PUT は全項目を送る形なので、読んだ値をそのまま返す）。
 * `simulation` は送らない - #417 で「更新で省略した simulation は保存値を保つ」に
 * なったので、この PUT が接続のシミュレーション設定を変えることは無い。
 */
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
	await attempt(failures, '不正なタグの削除', () => deleteByName('/api/tags', LEGACY_TAG_NAME));
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

/** データファイルと WAL の「大きさ:更新時刻」（書き込みがあったかの比較用）。 */
function dataFileSignatures(dataDir: string): Map<string, string> {
	const signatures = new Map<string, string>();
	if (!fs.existsSync(dataDir)) return signatures;
	for (const name of fs.readdirSync(dataDir)) {
		if (!DATA_FILE_OR_WAL_PATTERN.test(name)) continue;
		try {
			const stat = fs.statSync(path.join(dataDir, name));
			signatures.set(name, `${stat.size}:${stat.mtimeMs}`);
		} catch {
			// 列挙と stat の間に消えた（WAL が閉じられた）- 比較から外す。
		}
	}
	return signatures;
}

/**
 * `before` から変わった（新しくできた・大きさか更新時刻が変わった）ファイルの、
 * 本体（`YYYYMMDD-NNN.sqlite3`）の名前。WAL が変わったら本体の名前で返す。
 */
function changedDataFiles(before: Map<string, string>, dataDir: string): string[] {
	const changed = new Set<string>();
	for (const [name, signature] of dataFileSignatures(dataDir)) {
		if (before.get(name) === signature) continue;
		const base = DATA_FILE_OR_WAL_PATTERN.exec(name)?.[1];
		if (base) changed.add(base);
	}
	return [...changed].sort();
}

/** `chronogazer_core::collect::CollectEventRow`（`collectAdmin.ts` と同じ形）。 */
interface CollectEventRow {
	id: number;
	tsMs: number;
	kind: string;
	connectionKey: string | null;
}
type EventsReadout =
	| { state: 'notRunning' }
	| { state: 'unavailable' }
	| { state: 'ready'; data: { rows: CollectEventRow[]; totalCount: number; asOfId: number } };

/**
 * イベント一覧の先頭（新しい順）を REST で読む。`unavailable`（取り込み中で
 * 読めなかった）は失敗ではないので、読めるまで待つ。
 */
async function readEvents(
	request: APIRequestContext,
	headers: ApiHeaders,
	limit: number
): Promise<{ rows: CollectEventRow[]; asOfId: number }> {
	let result: { rows: CollectEventRow[]; asOfId: number } | undefined;
	await expect(async () => {
		const res = await request.get(`/api/collect/events?offset=0&limit=${limit}`, { headers });
		expect(res.ok(), `GET /api/collect/events が ${res.status()}`).toBe(true);
		const body = (await res.json()) as EventsReadout;
		expect(body.state, 'イベント一覧を読めていない').toBe('ready');
		if (body.state === 'ready') result = body.data;
	}).toPass({ timeout: 15_000 });
	if (!result) throw new Error('イベント一覧を読めなかった');
	return result;
}

/** いま記録されている最新のイベントの `id`（空なら 0）。操作の直前に控える。 */
async function latestEventId(request: APIRequestContext, headers: ApiHeaders): Promise<number> {
	return (await readEvents(request, headers, 1)).asOfId;
}

/**
 * `sinceId` より新しいイベントのうち、`kind`（と `connectionKey`）が一致する
 * 最初の 1 件（新しい順）を、記録されるまで待って返す。イベントの書き込みは
 * 状態の更新の直後に非同期で入る。
 */
async function waitForEventSince(
	request: APIRequestContext,
	headers: ApiHeaders,
	sinceId: number,
	kind: string,
	connectionKey?: string
): Promise<CollectEventRow> {
	let found: CollectEventRow | undefined;
	await expect(async () => {
		const { rows } = await readEvents(request, headers, 500);
		found = rows.find(
			(r) =>
				r.id > sinceId &&
				r.kind === kind &&
				(connectionKey === undefined || r.connectionKey === connectionKey)
		);
		expect(
			found,
			`id > ${sinceId} の ${kind} ${connectionKey ?? ''} が記録されること`
		).toBeDefined();
	}).toPass({ timeout: 15_000 });
	if (!found) throw new Error(`${kind} が見つからない`);
	return found;
}

/**
 * 画面の「時刻」列と同じ表示（`collectAdmin.ts` の `collectTimeLabel` =
 * ブラウザの `toLocaleString()`）。ロケール・タイムゾーンを画面と揃えるため、
 * ブラウザの中で作る。
 */
async function timeLabelInPage(page: Page, tsMs: number): Promise<string> {
	return page.evaluate((ms) => new Date(ms).toLocaleString(), tsMs);
}

/**
 * イベント画面に、REST で見つけた**この試行の**イベントの行が出るまで
 * 「再読み込み」する。種類の表示・時刻の表示（・接続）の全部で絞るので、
 * 前回の試行の同じ種類の行には一致しない。
 */
async function expectEventRowVisible(
	page: Page,
	event: CollectEventRow,
	kindLabel: string
): Promise<void> {
	const timeLabel = await timeLabelInPage(page, event.tsMs);
	let row = page.getByRole('row').filter({ hasText: kindLabel }).filter({ hasText: timeLabel });
	if (event.connectionKey) row = row.filter({ hasText: event.connectionKey });
	await expect(async () => {
		await page.getByRole('button', { name: '再読み込み' }).click();
		await expect(row.first()).toBeVisible({ timeout: 1_000 });
	}).toPass({ timeout: 15_000 });
}

test.describe.serial('chronogazer 開発用 PLC 相手の収集の一巡（R1-C の C-4）', () => {
	let page: Page;
	let dbDir: string;
	let dataDir: string;
	/** beforeAll で無効化した、他のスペックの接続（afterAll で有効に戻す）。 */
	let disabledByUs: ConnectionRow[] = [];
	let connectionId: number;
	let apiHeaders: ApiHeaders;
	/** 「収集を開始」を押す直前の最新イベント id（この試行のイベントの境界）。 */
	let eventIdBeforeStart: number;
	/** テスト 3 でこの試行の書き込みを確かめたデータファイル（本体の名前）。 */
	let writtenFiles: string[] = [];
	/** テスト 6 で作った、検証をくぐった不正なタグの id。 */
	let legacyTagId: number;

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

		// ワーカーが見ている一時ディレクトリが、この実行が作ったもの（所有
		// マーカーが一致する）で、banto-serve が使っているものと同じであること
		// （config を評価し直して別のディレクトリを作っていないこと）を、DB
		// ファイルの存在で確かめる。
		dbDir = process.env[RUN_DIR_ENV] ?? '';
		expect(dbDir, `${RUN_DIR_ENV} がワーカーに渡っていない`).not.toBe('');
		expect(
			ownsRunDir(dbDir, process.env[RUN_TOKEN_ENV]),
			`${dbDir} の所有マーカーがこの実行のトークンと一致しない`
		).toBe(true);
		expect(
			fs.existsSync(path.join(dbDir, DB_FILE_NAME)),
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
		// レジストリの CRUD では自動再起動しない（C-2 の決定）ので、明示的に
		// 開始する。開始前は、初回なら起動時の自動開始が空のレジストリで
		// 終わった「収集対象がありません」、CI の再試行なら前回の試行の
		// `afterAll` が止めた「停止」（ファイル冒頭の doc）。起動直後が
		// 「収集対象がありません」であることの確認は
		// `user-settings-collect.spec.ts` に任せる。
		await expect(statusLine(page)).toHaveText(/^状態: (収集対象がありません|停止)$/);
		eventIdBeforeStart = await latestEventId(page.request, apiHeaders);
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

	test('3. データディレクトリの時系列ファイルに、この試行の書き込みが入る', async () => {
		// 収集中（テスト 2 で「接続中」まで確かめた後）の状態を基準にして、
		// そこから大きさか更新時刻が変わるのを待つ。前回の試行が残したファイルが
		// あっても、この試行が書かなければ変わらない（ファイル冒頭の doc）。
		// 書き込みは既定 1 秒ごとの flush（banto-tstore の `WriterOptions`）。
		const before = dataFileSignatures(dataDir);
		await expect
			.poll(() => changedDataFiles(before, dataDir), {
				message: `${dataDir} の YYYYMMDD-NNN.sqlite3（か -wal）に書き込みが入ること`,
				timeout: 20_000
			})
			.not.toEqual([]);
		writtenFiles = changedDataFiles(before, dataDir);
		expect(dataFiles(dataDir)).toEqual(expect.arrayContaining(writtenFiles));
	});

	test('4. イベント画面に、この試行の「収集開始」と、この接続の「PLC接続」が出る', async () => {
		const started = await waitForEventSince(
			page.request,
			apiHeaders,
			eventIdBeforeStart,
			'collection_started'
		);
		const connected = await waitForEventSince(
			page.request,
			apiHeaders,
			eventIdBeforeStart,
			'plc_connected',
			`conn:${connectionId}`
		);
		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		await expectEventRowVisible(page, started, '収集開始');
		await expectEventRowVisible(page, connected, 'PLC接続');
	});

	test('5. 停止すると「停止」になり、イベント画面にこの試行の「収集停止」が出る', async () => {
		await page.goto('/settings/collect');
		await expect(statusLine(page)).toHaveText(/^状態: 収集中/);
		const eventIdBeforeStop = await latestEventId(page.request, apiHeaders);
		await page.getByRole('button', { name: '収集を停止' }).click();
		await expect(statusLine(page)).toHaveText('状態: 停止');

		const stopped = await waitForEventSince(
			page.request,
			apiHeaders,
			eventIdBeforeStop,
			'collection_stopped'
		);
		await page.goto('/events');
		await expectEventRowVisible(page, stopped, '収集停止');

		// 停止後も、この試行が書いたデータファイルは残っている（最終 flush 済み）。
		expect(writtenFiles).not.toEqual([]);
		expect(dataFiles(dataDir)).toEqual(expect.arrayContaining(writtenFiles));
	});

	test('6. 保存時の検証より前に入った不正なタグがあっても開始でき、「除外あり」と一覧が出る', async () => {
		// 正常なアドレスで作ってから、DB の行を直接書き換える（REST は #418 で
		// 拒否するので、検証をくぐった既存データはこうしてしか作れない）。
		const group = (
			await getList<NamedRow>(page.request, apiHeaders, '/api/collection-groups')
		).find((g) => g.name === GROUP_NAME);
		expect(group, '一巡のグループが無い').toBeDefined();
		const created = await page.request.post('/api/tags', {
			headers: apiHeaders,
			data: {
				name: LEGACY_TAG_NAME,
				collectionGroupId: group?.id,
				address: '40002',
				dataType: 'u16',
				decimals: 0,
				enabled: true
			}
		});
		expect(created.ok(), `POST /api/tags が ${created.status()}`).toBe(true);
		legacyTagId = ((await created.json()) as NamedRow).id;
		rewriteTagAddress(path.join(dbDir, DB_FILE_NAME), legacyTagId, LEGACY_ADDRESS);

		await page.goto('/settings/collect');
		await expect(statusLine(page)).toHaveText('状態: 停止');
		const before = dataFileSignatures(dataDir);
		await page.getByRole('button', { name: '収集を開始' }).click();

		// 開始は失敗しない。正常なタグ 1 件で走る。
		await expect(statusLine(page)).toHaveText('状態: 収集中（グループ1件 / タグ1件）');
		const exclusions = page.locator('.collect-exclusions');
		await expect(exclusions.getByRole('heading', { name: '除外あり（1 件）' })).toBeVisible();
		const row = exclusions.getByRole('row').filter({ hasText: LEGACY_TAG_NAME });
		await expect(row).toContainText('タグ');
		await expect(row).toContainText('Modbus TCP のアドレスとして解釈できません');
		await expect(exclusions.getByRole('link', { name: 'タグ設定' })).toHaveAttribute(
			'href',
			'/tags'
		);

		// 正常なタグは収集され、データファイルに書き込みが入る。
		await expect
			.poll(() => changedDataFiles(before, dataDir), {
				message: '不正なタグがあっても、正常なタグの書き込みが入ること',
				timeout: 20_000
			})
			.not.toEqual([]);
	});

	test('7. 現在値: 正常なタグは good、外したタグは value: null・品質 invalid', async () => {
		const tagId = (await getList<NamedRow>(page.request, apiHeaders, '/api/tags')).find(
			(t) => t.name === TAG_NAME
		)?.id;
		expect(tagId, '一巡のタグが無い').toBeDefined();
		await expect(async () => {
			const res = await page.request.get('/api/collect/values', { headers: apiHeaders });
			expect(res.ok()).toBe(true);
			const body = (await res.json()) as ValuesReadout;
			expect(body.state).toBe('ready');
			if (body.state !== 'ready') return;
			expect(body.data[`tag:${tagId}`]?.quality).toBe('good');
			expect(body.data[`tag:${legacyTagId}`]).toEqual({
				value: null,
				ptimeMs: null,
				quality: 'invalid'
			});
		}).toPass({ timeout: 15_000 });
	});

	test('8. /tags に「収集の開始時に外される設定」が出て、停止すると収集の画面から除外が消える', async () => {
		await page.goto('/tags');
		const marks = page.getByRole('region', { name: '収集の開始時に外される設定' });
		await expect(marks).toBeVisible();
		const mark = marks.getByRole('listitem').filter({ hasText: LEGACY_TAG_NAME });
		await expect(mark).toContainText('タグ');
		await expect(mark).toContainText('Modbus TCP のアドレスとして解釈できません');

		await page.goto('/settings/collect');
		await expect(statusLine(page)).toHaveText(/^状態: 収集中/);
		await page.getByRole('button', { name: '収集を停止' }).click();
		await expect(statusLine(page)).toHaveText('状態: 停止');
		// 走っていない状態に前回の一覧を残さない。
		await expect(page.locator('.collect-exclusions')).toHaveCount(0);
	});
});

/** `chronogazer_core::collect::CurrentSampleView` の読み出し（テスト 7）。 */
type ValuesReadout =
	| { state: 'notRunning' }
	| { state: 'unavailable' }
	| {
			state: 'ready';
			data: Record<
				string,
				{ value: number | null; ptimeMs: number | null; quality: string } | undefined
			>;
	  };

/**
 * DB のタグの行のアドレスを直接書き換える（テスト 6）。banto-serve が同じ
 * ファイルを開いたままなので、ロックの待ちを入れて 1 文だけ書き、すぐ閉じる。
 */
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
