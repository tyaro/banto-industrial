/**
 * サービス接続モードの画面（#392 B2-2、設計書 docs/chronogazer/chronogazer-service-design.md
 * §3.2・§4 の B2-2 の実装メモ）: `chronogazer-server` が配る画面に、この PC から
 * ログインなしで入る（grant `localDesktop`）。
 *
 * **banto v8 追従（ADR-0034、docs/banto-v8-upgrade-plan.md）**: 初回セットアップ（画面・
 * `POST /api/auth/setup`・「管理者のアカウントがまだありません」の帯・初回の
 * 「この PC からはログインなしで使う」のチェック）は無くなった。新しい DB はログインなしで
 * 始まり、この PC からの接続は組み込みの `admin` として入る。ログイン必須の DB は
 * 起動の前に `banto-serve create-admin`（管理者の作成とログイン必須への切り替えを 1 度に
 * 行う、上流と同じ形）で作り、この PC からの接続は設定 `auth.local_grant`（既定 off）で選ぶ。
 *
 * 固定したい受入条件（2026-10-14 オーナー決定 1〜7 を v8 の形に直した）:
 * 1. 新しいルートの `/` → ログインなしで監視画面（ヘッダーに「この PC」）。Tauri の内部
 *    オブジェクトがある窓（シェルがサービスへ navigate した窓と同じ。`__TAURI_INTERNALS__` を
 *    注入する）でも invoke を呼ばない（`isTauri()` の origin の判定）。設定はデスクトップ専用
 *    （ウィンドウ効果・自動ログイン・フォルダを開く）を出さず、接続（LAN）はこの PC から
 *    変えられる。
 * 3. サーバーを再起動しても、新しいブラウザーの文脈で `/` → ログインなしで監視画面。
 * 4. ログイン必須で `auth.local_grant = admin`: ログアウト → ログイン画面（自動では入り直さない）に
 *    「この PC として続ける」→ 押すと監視画面。
 * 4b. 閲覧公開も ON のとき、ログイン画面の「この PC として続ける」→ 監視画面 → 戻る →
 *    「閲覧のみで続ける」で閲覧者（`publicViewer`・viewer）に切り替わる（残っていた
 *    この PC の admin のセッションのまま進まない。#392 B2-2 のレビュー）。
 * 5. 設定 › セキュリティでこの PC からの接続を「使わない」にすると、自分のセッションも
 *    閉じてログイン画面へ。再起動の後も `/` はログイン画面。
 * 6. ログイン必須で既定（`auth.local_grant` = off）: 新しい文脈の `/` はログイン画面
 *    （grant は閉じている。「この PC として続ける」は出ない）。
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
import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
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

const ADMIN_USERNAME = 'e2e-owner';
const ADMIN_PASSWORD = 'E2eServerAdmin1';
const bantoServeBin = path.join(
	repoRoot,
	'target',
	'debug',
	process.platform === 'win32' ? 'banto-serve.exe' : 'banto-serve'
);

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

/**
 * 起動の前に、`root` の DB に管理者を作ってログイン必須にする（`banto-serve create-admin`、
 * banto v8 の最初の管理者。パスワードは標準入力）。サーバーは止めておく。
 */
function createAdminOffline(root: string): void {
	expect(fs.existsSync(bantoServeBin), `${bantoServeBin} がありません（README のビルド前提）`).toBe(
		true
	);
	fs.mkdirSync(root, { recursive: true });
	const created = spawnSync(
		bantoServeBin,
		['create-admin', '--username', ADMIN_USERNAME, '--display-name', 'E2E サーバー管理者'],
		{
			env: { ...process.env, BANTO_DB: path.join(root, 'chronogazer.sqlite3') },
			input: `${ADMIN_PASSWORD}\n`,
			encoding: 'utf8',
			windowsHide: true
		}
	);
	expect(created.status, `create-admin: ${created.stdout}${created.stderr}`).toBe(0);
}

/** 作った管理者でログインし、この PC からの接続（`auth.local_grant`）を `role` にする。 */
async function setLocalGrant(request: APIRequestContext, role: string): Promise<void> {
	const login = await request.post(`${BASE_URL}/api/auth/login`, {
		headers: CSRF_HEADERS,
		data: { username: ADMIN_USERNAME, password: ADMIN_PASSWORD }
	});
	expect(login.ok(), await login.text()).toBe(true);
	const { token } = (await login.json()) as { token: string };
	const headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
	const saved = await request.put(`${BASE_URL}/api/settings/auth`, {
		headers,
		data: { localGrant: role }
	});
	expect(saved.ok(), await saved.text()).toBe(true);
	await request.post(`${BASE_URL}/api/auth/logout`, { headers });
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

test.describe.serial('この PC からの接続（新しい DB はログインなし）', () => {
	let root: string;
	let server: RunningServer | null = null;

	test.beforeAll(async () => {
		root = freshRoot('no-login');
		server = await startServer(root);
	});

	test.afterAll(async () => {
		await server?.stop();
	});

	test('1. 新しい / はログインなしで監視画面、デスクトップ専用の設定は出ない', async ({
		browser
	}) => {
		const { context, page, problems } = await shellLikeContext(browser);
		await page.goto('/');
		await expectThisPcSession(page);

		// 外観: ウィンドウ効果（Acrylic）は出ない。
		await page.goto('/settings/appearance');
		await expect(page.getByRole('heading', { name: 'テーマ' })).toBeVisible();
		await expect(page.getByRole('heading', { name: 'ウィンドウ効果' })).toHaveCount(0);
		// アカウント: 自動ログインは出ない。
		await page.goto('/settings/account');
		await expect(page.getByRole('heading', { name: 'アカウント' })).toBeVisible();
		await expect(page.getByRole('heading', { name: '自動ログイン' })).toHaveCount(0);
		// データ: フォルダを開くは出ない。
		await page.goto('/settings/data');
		await expect(page.getByRole('heading', { name: 'バックアップ/リストア' })).toBeVisible();
		await expect(page.getByRole('button', { name: 'フォルダを開く' })).toHaveCount(0);
		// 接続: この PC から開いた画面なので変えられる。
		await page.goto('/settings/connectivity');
		await expect(page.getByRole('button', { name: '保存して適用' })).toBeVisible();
		await expect(page.getByText('状態:')).toBeVisible();

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
		expect(problems).toEqual([]);
		await context.close();
	});
});

test.describe.serial('この PC からの接続（ログイン必須で「管理者」を選ぶ）', () => {
	let root: string;
	let server: RunningServer | null = null;

	test.beforeAll(async ({ request }) => {
		root = freshRoot('opt-in');
		createAdminOffline(root);
		// ログイン必須で起動した直後はこの PC からの接続は閉じている（既定 off）ので、作った管理者で
		// ログインして `admin` にし、**再起動**で開く（開く方向は次の起動から。local_grant.rs）。
		server = await startServer(root);
		await setLocalGrant(request, 'admin');
		await server.stop();
		server = await startServer(root);
	});

	test.afterAll(async () => {
		await server?.stop();
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

	test('4b. 両方使えるとき「閲覧のみで続ける」は閲覧者になる（この PC のセッションから戻っても切り替わる）', async ({
		browser,
		request
	}) => {
		// 閲覧公開を ON にする（この PC からの接続の admin で設定の REST を呼ぶ）。
		await setViewerPublic(request, true);
		try {
			const { context, page } = await shellLikeContext(browser);
			await page.goto('/login');
			// 先に「この PC として続ける」で admin のセッションに入り、ブラウザの「戻る」で
			// ログイン画面へ戻ってから「閲覧のみで続ける」を押す（オーナー再レビュー P2:
			// 残っていた admin のセッションのまま監視画面へ進んでいた）。
			await page.getByRole('button', { name: 'この PC として続ける' }).click();
			await expectThisPcSession(page);
			await page.goBack();
			await expect(page).toHaveURL(/\/login$/);
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

test.describe.serial('この PC からの接続（ログイン必須で既定のまま）', () => {
	let server: RunningServer | null = null;

	test.beforeAll(async () => {
		const root = freshRoot('opt-out');
		createAdminOffline(root);
		server = await startServer(root);
	});

	test.afterAll(async () => {
		await server?.stop();
	});

	test('6. 新しい文脈の / はログイン画面（grant は閉じている）', async ({ browser }) => {
		const { context, page, problems } = await shellLikeContext(browser);
		await page.goto('/');
		await expect(page).toHaveURL(/\/login$/);
		await expect(page.getByRole('button', { name: 'ログイン', exact: true })).toBeVisible();
		await expect(page.getByRole('button', { name: 'この PC として続ける' })).toHaveCount(0);
		expect(problems).toEqual([]);
		await context.close();
	});
});
