/**
 * サービス接続モードの画面（#392 B2-2、設計書 docs/chronogazer-service-design.md
 * §3.2・§4 の B2-2 の実装メモ）: `chronogazer-server` が配る画面に、この PC から
 * ログインなしで入る（grant `localDesktop`）。
 *
 * 固定したい受入条件（2026-10-14 オーナー決定 1〜7）:
 * 1. 新しいルートの `/` → ログインなしで監視画面。上に「管理者のアカウントがまだ
 *    ありません」の帯、ヘッダーに「この PC」。Tauri の内部オブジェクトがある窓
 *    （シェルがサービスへ navigate した窓と同じ。`__TAURI_INTERNALS__` を注入する）
 *    でも invoke を呼ばない（`isTauri()` の origin の判定）。
 * 2. 帯から初回セットアップへ → 「この PC からはログインなしで使う（管理者）」が
 *    既定でチェック → 作成すると、ログインせずに監視画面へ戻る（「この PC」のまま）。
 *    設定はデスクトップ専用（ウィンドウ効果・自動ログイン・フォルダを開く）を出さず、
 *    接続（LAN）はこの PC から変えられる。
 * 3. サーバーを再起動しても、新しいブラウザーの文脈で `/` → ログインなしで監視画面。
 * 4. ログアウト → ログイン画面（自動では入り直さない）に「この PC として続ける」→
 *    押すと監視画面。
 * 4b. 閲覧公開も ON のとき、ログイン画面の「閲覧のみで続ける」は閲覧者（`publicViewer`・
 *    viewer）になり、この PC からの接続（admin）を先に発行しない（#392 B2-2 のレビュー）。
 * 5. 設定 › セキュリティでこの PC からの接続を「使わない」にすると、自分のセッションも
 *    閉じてログイン画面へ。再起動の後も `/` はログイン画面。
 * 6. 別の新しいルートでチェックを外して作成すると、今までどおり作ったアカウントの
 *    セッションで監視画面へ（「この PC」ではない）。新しい文脈の `/` はログイン画面
 *    （grant は閉じている）。
 *
 * サーバーはこのスペックが子プロセスとして起動・再起動・停止する
 * （`chronogazer-server.playwright.config.ts` の doc）。データのルートはこの実行の
 * 一時ディレクトリの中（`%ProgramData%` には触れない）。止め方は `--control-stdin` の
 * `ctrl-c`（Ctrl-C と同じ経路）、終わらなければ上限の後に kill する。
 */
import {
	expect,
	test,
	type APIRequestContext,
	type Browser,
	type BrowserContext,
	type Page
} from '@playwright/test';
import { spawn, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { RUN_DIR_ENV } from '../chronogazer-e2e-run-dir';

const PORT = 8807;
const BASE_URL = `http://127.0.0.1:${PORT}`;
const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const serverBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'chronogazer-server.exe' : 'chronogazer-server'
);

const ADMIN_PASSWORD = 'E2eServerAdmin1';
const BANNER_TEXT = '管理者のアカウントがまだありません';
const OPT_IN_LABEL = 'この PC からはログインなしで使う（管理者）';

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

interface RunningServer {
	/** `ctrl-c` で止めて終了を待つ（上限を超えたら kill）。何度呼んでもよい。 */
	stop(): Promise<void>;
}

/** 新しいデータのルート（この実行の一時ディレクトリの中）。 */
function freshRoot(name: string): string {
	const runDir = process.env[RUN_DIR_ENV];
	if (!runDir) throw new Error(`${RUN_DIR_ENV} がありません（config から実行してください）`);
	const root = path.join(runDir, name);
	expect(fs.existsSync(root), `${root} が既にあります`).toBe(false);
	return root;
}

/**
 * `chronogazer-server` を `root` で起動し、`GET /api/host` が `server` を返すまで待つ。
 * ポートが既に使われていれば起動前に落とす（他のプロセスのサーバーと取り違えない）。
 */
async function startServer(root: string): Promise<RunningServer> {
	expect(await canConnect(PORT), `ポート ${PORT} が既に使われています`).toBe(false);
	expect(fs.existsSync(serverBin), `${serverBin} がありません（README のビルド前提）`).toBe(true);
	// 親の環境のテスト用の注入（`CHRONOGAZER_TEST_*`）・ルートは持ち込まない。
	const env: NodeJS.ProcessEnv = {};
	for (const [key, value] of Object.entries(process.env)) {
		if (!key.startsWith('CHRONOGAZER_')) env[key] = value;
	}
	env.CHRONOGAZER_ROOT = root;
	env.CHRONOGAZER_TEST_PORT = String(PORT);
	const child: ChildProcess = spawn(serverBin, ['--control-stdin'], {
		env,
		stdio: ['pipe', 'pipe', 'pipe'],
		windowsHide: true
	});
	let output = '';
	child.stdout?.on('data', (chunk: Buffer) => (output += chunk.toString()));
	child.stderr?.on('data', (chunk: Buffer) => (output += chunk.toString()));
	const exited = new Promise<void>((resolve) => {
		if (child.exitCode !== null || child.signalCode !== null) resolve();
		else child.once('exit', () => resolve());
	});
	const killOnExit = () => child.kill();
	process.once('exit', killOnExit);

	let stopped = false;
	const stop = async (): Promise<void> => {
		if (stopped) return exited;
		stopped = true;
		process.removeListener('exit', killOnExit);
		if (child.exitCode === null && child.signalCode === null) {
			child.stdin?.write('ctrl-c\n');
			const timedOut = await Promise.race([
				exited.then(() => false),
				new Promise<boolean>((resolve) => setTimeout(() => resolve(true), 30_000))
			]);
			if (timedOut) child.kill();
		}
		await exited;
		// 次の起動が同じポートを取れるまで待つ（listener の後始末）。
		await expect
			.poll(() => canConnect(PORT), { message: `ポート ${PORT} が空かない`, timeout: 15_000 })
			.toBe(false);
	};
	try {
		await expect
			.poll(
				async () => {
					if (child.exitCode !== null) {
						throw new Error(`chronogazer-server が終了しました（${child.exitCode}）: ${output}`);
					}
					try {
						const res = await fetch(`${BASE_URL}/api/host`, { headers: CSRF_HEADERS });
						if (!res.ok) return null;
						return ((await res.json()) as { hostKind?: string }).hostKind ?? null;
					} catch {
						return null;
					}
				},
				{ message: `chronogazer-server が ${PORT} で応答しない: ${output}`, timeout: 60_000 }
			)
			.toBe('server');
	} catch (err) {
		await stop();
		throw err;
	}
	return { stop };
}

/**
 * シェルがサービスへ navigate した窓と同じく、`__TAURI_INTERNALS__` を注入した文脈。
 * invoke を呼ぶと分かるよう、呼ばれたら印を残す（本物の Tauri は `invoke` を持つ）。
 * ページのエラーと invoke の印を集める。
 */
async function shellLikeContext(
	browser: Browser
): Promise<{ context: BrowserContext; page: Page; problems: string[] }> {
	const context = await browser.newContext({ baseURL: BASE_URL });
	await context.addInitScript(() => {
		(window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
			invoke: (cmd: string) => {
				(window as unknown as { __invoked?: string[] }).__invoked ??= [];
				(window as unknown as { __invoked: string[] }).__invoked.push(cmd);
				return Promise.reject(new Error(`invoke(${cmd}) は呼ばれないはず`));
			},
			transformCallback: () => 0
		};
	});
	const page = await context.newPage();
	const problems: string[] = [];
	page.on('pageerror', (err) => problems.push(`pageerror: ${err.message}`));
	page.on('console', (message) => {
		const text = message.text();
		if (message.type() === 'error' && /invoke|TAURI/i.test(text)) problems.push(text);
	});
	return { context, page, problems };
}

async function invokedCommands(page: Page): Promise<string[]> {
	return page.evaluate(() => (window as unknown as { __invoked?: string[] }).__invoked ?? []);
}

async function expectThisPcSession(page: Page): Promise<void> {
	await expect(page).toHaveURL(/\/monitor$/);
	await expect(page.locator('header').getByText('この PC', { exact: true })).toBeVisible();
	await expect(page.locator('header').getByRole('button', { name: 'ログアウト' })).toBeVisible();
}

async function createFirstAdmin(page: Page, username: string, optIn: boolean): Promise<void> {
	await expect(page.getByText('初回起動です。管理者アカウントを作成してください。')).toBeVisible();
	const checkbox = page.getByLabel(OPT_IN_LABEL);
	await expect(checkbox).toBeChecked();
	if (!optIn) await checkbox.uncheck();
	await page.getByLabel('表示名').fill(`E2E ${username}`);
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード（8文字以上）').fill(ADMIN_PASSWORD);
	await page.getByLabel('パスワード（確認）').fill(ADMIN_PASSWORD);
	await page.getByRole('button', { name: 'アカウントを作成' }).click();
}

/**
 * 閲覧公開（`server.viewerPublic`）を切り替える。この PC からの接続（admin）の
 * トークンを受け取り、`PUT /api/settings/server` で LAN は無効のまま保存する。
 */
async function setViewerPublic(request: APIRequestContext, on: boolean): Promise<void> {
	const granted = await request.post(`${BASE_URL}/api/auth/grant/localDesktop`, {
		headers: CSRF_HEADERS
	});
	expect(granted.ok(), await granted.text()).toBe(true);
	const { token } = (await granted.json()) as { token: string };
	const headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
	const current = await request.get(`${BASE_URL}/api/settings/server`, { headers });
	expect(current.ok(), await current.text()).toBe(true);
	const status = (await current.json()) as { bind: string; port: number };
	const saved = await request.put(`${BASE_URL}/api/settings/server`, {
		headers,
		data: { enabled: false, bind: status.bind, port: status.port, viewerPublic: on }
	});
	expect(saved.ok(), await saved.text()).toBe(true);
	expect(((await saved.json()) as { viewerPublic: boolean }).viewerPublic).toBe(on);
	await request.post(`${BASE_URL}/api/auth/logout`, { headers });
}

test.describe.serial('この PC からの接続（初回セットアップで選ぶ）', () => {
	let root: string;
	let server: RunningServer | null = null;

	test.beforeAll(async () => {
		root = freshRoot('opt-in');
		server = await startServer(root);
	});

	test.afterAll(async () => {
		await server?.stop();
	});

	test('1. 未初期化の / はログインなしで監視画面、上に作成の案内', async ({ browser }) => {
		const { context, page, problems } = await shellLikeContext(browser);
		await page.goto('/');
		await expectThisPcSession(page);
		await expect(page.getByRole('status').getByText(BANNER_TEXT)).toBeVisible();
		expect(await invokedCommands(page)).toEqual([]);
		expect(problems).toEqual([]);
		await context.close();
	});

	test('2. 案内から作成（既定でチェック）→ ログインなしで監視画面に戻る、デスクトップ専用の設定は出ない', async ({
		browser
	}) => {
		const { context, page, problems } = await shellLikeContext(browser);
		await page.goto('/');
		await expectThisPcSession(page);
		await page.getByRole('link', { name: '管理者を作成する' }).click();
		await expect(page).toHaveURL(/\/login$/);
		await createFirstAdmin(page, 'e2e-owner', true);

		await expectThisPcSession(page);
		await expect(page.getByText(BANNER_TEXT)).toHaveCount(0);

		// 外観: ウィンドウ効果（Acrylic）は出ない。
		await page.goto('/settings/appearance');
		await expect(page.getByRole('heading', { name: 'テーマ' })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'ウィンドウ効果' })).toHaveCount(0);
		// アカウント: 自動ログインは出ず、パスワード変更の代わりに案内。
		await page.goto('/settings/account');
		await expect(page.getByRole('heading', { name: 'アカウント' })).toBeVisible();
		await expect(page.getByRole('heading', { name: '自動ログイン' })).toHaveCount(0);
		await expect(
			page.getByText('この PC からの接続（ログインなし）ではアカウントがない')
		).toBeVisible();
		// データ: フォルダを開くは出ない。
		await page.goto('/settings/data');
		await expect(page.getByRole('heading', { name: 'バックアップ/リストア' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'フォルダを開く' })).toHaveCount(0);
		// 接続: この PC から開いた画面なので変えられる。
		await page.goto('/settings/connectivity');
		await expect(page.getByRole('button', { name: '保存して適用' })).toBeVisible();
		await expect(page.getByText('状態:')).toBeVisible();
		// セキュリティ: この PC からの接続は「管理者」。
		await page.goto('/settings/security');
		await expect(page.getByTestId('local-grant-select')).toHaveValue('admin');

		expect(await invokedCommands(page)).toEqual([]);
		expect(problems).toEqual([]);
		await context.close();
	});

	test('3. 再起動しても新しい文脈の / はログインなしで監視画面', async ({ browser }) => {
		await server?.stop();
		server = await startServer(root);
		const { context, page, problems } = await shellLikeContext(browser);
		await page.goto('/');
		await expectThisPcSession(page);
		await expect(page.getByText(BANNER_TEXT)).toHaveCount(0);
		expect(problems).toEqual([]);
		await context.close();
	});

	test('4. ログアウト → ログイン画面に「この PC として続ける」→ 監視画面', async ({ browser }) => {
		const { context, page } = await shellLikeContext(browser);
		await page.goto('/');
		await expectThisPcSession(page);
		await page.locator('header').getByRole('button', { name: 'ログアウト' }).click();
		await expect(page).toHaveURL(/\/login$/);
		// 自動では入り直さない（ログイン画面に留まる）。
		await expect(page.getByRole('button', { name: 'ログイン', exact: true })).toBeVisible();
		await page.getByRole('button', { name: 'この PC として続ける' }).click();
		await expectThisPcSession(page);
		await context.close();
	});

	test('4b. 両方使えるとき「閲覧のみで続ける」は閲覧者になる（localDesktop を先に発行しない）', async ({
		browser,
		request
	}) => {
		// 閲覧公開を ON にする（この PC からの接続の admin で設定の REST を呼ぶ）。
		await setViewerPublic(request, true);
		try {
			const { context, page } = await shellLikeContext(browser);
			await page.goto('/login');
			await expect(page.getByRole('button', { name: 'この PC として続ける' })).toBeVisible();
			await page.getByRole('button', { name: '閲覧のみで続ける' }).click();
			await expect(page).toHaveURL(/\/monitor$/);
			const me = await page.evaluate(async () => {
				const token = sessionStorage.getItem('banto.auth.token');
				const res = await fetch('/api/auth/identity', {
					headers: { 'X-Banto-Client': 'banto', Authorization: `Bearer ${token}` }
				});
				return (await res.json()) as { kind?: string; role?: string };
			});
			expect(me.kind).toBe('publicViewer');
			expect(me.role).toBe('viewer');
			// 閲覧公開の表示: ヘッダーは「ログイン」、「この PC」ではない。設定は許可リストの外。
			await expect(page.locator('header').getByRole('button', { name: 'ログイン' })).toBeVisible();
			await expect(page.locator('header').getByText('この PC', { exact: true })).toHaveCount(0);
			await page.goto('/settings/security');
			await expect(page).toHaveURL(/\/monitor$/);
			await expect(page.getByTestId('local-grant-select')).toHaveCount(0);
			await context.close();
		} finally {
			await setViewerPublic(request, false);
		}
	});

	test('5. セキュリティで「使わない」にすると自分も閉じてログイン画面、再起動後も / はログイン画面', async ({
		browser
	}) => {
		const { context, page } = await shellLikeContext(browser);
		await page.goto('/settings/security');
		const select = page.getByTestId('local-grant-select');
		await expect(select).toHaveValue('admin');
		await select.selectOption('off');
		const section = page.locator('section', {
			has: page.getByRole('heading', { name: 'この PC からの接続' })
		});
		await section.getByRole('button', { name: '保存して適用' }).click();
		await expect(page).toHaveURL(/\/login$/);
		await expect(page.getByRole('button', { name: 'この PC として続ける' })).toHaveCount(0);
		await context.close();

		await server?.stop();
		server = await startServer(root);
		const fresh = await shellLikeContext(browser);
		await fresh.page.goto('/');
		await expect(fresh.page).toHaveURL(/\/login$/);
		await expect(fresh.page.getByRole('button', { name: 'ログイン', exact: true })).toBeVisible();
		await fresh.context.close();
	});
});

test.describe.serial('この PC からの接続（初回セットアップで選ばない）', () => {
	let server: RunningServer | null = null;

	test.beforeAll(async () => {
		server = await startServer(freshRoot('opt-out'));
	});

	test.afterAll(async () => {
		await server?.stop();
	});

	test('6. チェックを外して作成 → 作ったアカウントで監視画面、新しい文脈の / はログイン画面', async ({
		browser
	}) => {
		const { context, page, problems } = await shellLikeContext(browser);
		await page.goto('/');
		await expectThisPcSession(page);
		await page.getByRole('link', { name: '管理者を作成する' }).click();
		await createFirstAdmin(page, 'e2e-optout', false);
		await expect(page).toHaveURL(/\/monitor$/);
		await expect(page.locator('header').getByRole('button', { name: 'ログアウト' })).toBeVisible();
		await expect(page.locator('header').getByText('この PC', { exact: true })).toHaveCount(0);
		expect(problems).toEqual([]);
		await context.close();

		const fresh = await shellLikeContext(browser);
		await fresh.page.goto('/');
		await expect(fresh.page).toHaveURL(/\/login$/);
		await expect(fresh.page.getByRole('button', { name: 'この PC として続ける' })).toHaveCount(0);
		await fresh.context.close();
	});
});
