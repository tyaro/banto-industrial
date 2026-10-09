/**
 * ChronoGazer public-viewer (閲覧公開) E2E (#507; mirrors upstream banto's
 * e2e/tests-public-viewer/public-viewer.spec.ts, ADR-0012 / I2b #499).
 *
 * Runs against the `banto-serve` that `public-viewer.playwright.config.ts`
 * starts with `BANTO_VIEWER_PUBLIC=1` (own port 8804, own fresh DB). One
 * shared page in file order: scenarios 1-3 run against a DB with ZERO users;
 * scenario 4 creates the first admin and later state builds on it.
 *
 * What it proves end to end on the real REST path:
 *   1. without logging in, the allowed screens (監視・ヒストリカル・イベント)
 *      open as the synthetic viewer, with no redirect loop (the guard in
 *      `apps/chronogazer/src/routes/(app)/+layout.ts`; SvelteKit 3's
 *      `resolve('')` trap, #506);
 *   2. the other screens (タグ設定・ユーザー管理・監査ログ・設定) are not
 *      opened - the guard sends them to the first allowed screen (監視). They
 *      are not redirected to /login: the viewer session is already confirmed,
 *      and only an unconfirmed session goes to /login;
 *   3. the sidebar lists only the allowed items;
 *   4. "ログイン" leads to the setup/login screen and signing in switches to a
 *      normal session (full nav, ログアウト instead of ログイン, and the
 *      restricted screens open);
 *   5. (R1-D の D-4) 監視画面の 4 種（デジタル・バー・計器・トレンド）が閲覧者にも
 *      値付きで描かれ、読み取り専用である（本文に設定画面へのリンクを 1 つも
 *      出さない）。表示グループ・タグは scenario 4 で作った管理者が REST で作り、
 *      閲覧者はログインしていない別のブラウザコンテキストで開く。値の出どころは
 *      製品の接続単位シミュレーション（`simulation: true`、#413）- この config は
 *      開発用 PLC を起動しない（シミュレーションの値は記録されないが、現在値と
 *      トレンドの追従には足りる）。
 *
 * No `waitForTimeout`/`sleep`: every wait is a locator auto-retry.
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';

const ADMIN_USERNAME = 'pv-admin';
const ADMIN_PASSWORD = 'E2ePvAdminPass1';
const ADMIN_DISPLAY_NAME = 'E2E閲覧公開管理者';

const ALLOWED = [
	{ path: '/monitor', label: '監視' },
	{ path: '/historical', label: 'ヒストリカル' },
	{ path: '/events', label: 'イベント' }
] as const;

const RESTRICTED = [
	{ path: '/tags', label: 'タグ設定' },
	{ path: '/users', label: 'ユーザー管理' },
	{ path: '/audit-log', label: '監査ログ' },
	{ path: '/settings', label: '設定' }
] as const;

const CSRF_HEADERS = { 'X-Banto-Client': 'banto' };
type ApiHeaders = Record<string, string>;

// scenario 5 のフィクスチャ。名前で掃除する（CI の再試行は同じ DB で走る）。
const PV_CONNECTION = 'PV-シミュレーション';
const PV_COLLECTION_GROUP = 'PV-収集';
const PV_TAG_SCALED = 'PV-工学値';
const PV_TAG_UNSET = 'PV-レンジなし';
const PV_GROUPS = [
	{ name: 'PV-デジタル', kind: 'digital' },
	{ name: 'PV-バー', kind: 'bar' },
	{ name: 'PV-計器', kind: 'gauge' },
	{ name: 'PV-トレンド', kind: 'trend' }
] as const;

interface NamedRow {
	id: number;
	name: string;
}

async function adminHeaders(request: APIRequestContext): Promise<ApiHeaders> {
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

/** 収集を止め、scenario 5 のものを依存の逆順に消す（無ければ何もしない）。 */
async function cleanupMonitorFixtures(
	request: APIRequestContext,
	headers: ApiHeaders
): Promise<void> {
	const stop = await request.post('/api/collect/stop', { headers });
	expect(stop.ok(), `POST /api/collect/stop が ${stop.status()}`).toBe(true);
	const deleteByName = async (listUrl: string, names: readonly string[]): Promise<void> => {
		const list = await request.get(listUrl, { headers });
		expect(list.ok(), `GET ${listUrl} が ${list.status()}`).toBe(true);
		for (const row of ((await list.json()) as NamedRow[]).filter((r) => names.includes(r.name))) {
			const res = await request.delete(`${listUrl}/${row.id}`, { headers });
			expect([204, 404], `DELETE ${listUrl}/${row.id}`).toContain(res.status());
		}
	};
	await deleteByName(
		'/api/display-groups',
		PV_GROUPS.map((g) => g.name)
	);
	await deleteByName('/api/tags', [PV_TAG_SCALED, PV_TAG_UNSET]);
	await deleteByName('/api/collection-groups', [PV_COLLECTION_GROUP]);
	await deleteByName('/api/plc-connections', [PV_CONNECTION]);
}

/** The header's "ログイン" button that replaces "ログアウト" for the synthetic viewer. */
function loginButton(page: Page) {
	return page.getByRole('banner').getByRole('button', { name: 'ログイン' });
}

/** A sidebar link. Its accessible name is "<icon> <label>", and `設定` is a suffix of `タグ設定` - match the whole name. */
function navLink(page: Page, label: string) {
	return page
		.getByRole('navigation')
		.getByRole('link', { name: new RegExp(String.raw`^\S+ ${label}$`) });
}

test.describe.serial('ChronoGazer viewer-public mode', () => {
	let page: Page;
	let monitorFixturesCreated = false;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		// scenario 5 が収集を始めていたら止めて消す（scenario 5 まで来ていなければ
		// 何もしない）。
		if (page && monitorFixturesCreated) {
			await cleanupMonitorFixtures(page.request, await adminHeaders(page.request));
		}
		await page?.close();
	});

	test('1. the allowed screens open without logging in, with no redirect loop', async () => {
		for (const { path, label } of ALLOWED) {
			await page.goto(path);
			// A loop would never settle on the URL (the guard bounces every
			// path to the first entry); `toHaveURL` fails on the first miss.
			await expect(page).toHaveURL(new RegExp(`${path}$`));
			await expect(page.getByRole('heading', { level: 2, name: label })).toBeVisible();
			// Synthetic session: ログイン instead of ログアウト.
			await expect(loginButton(page)).toBeVisible();
			await expect(page.getByRole('button', { name: 'ログアウト' })).toHaveCount(0);
		}
		// The in-app path (sidebar click) is allowed too.
		await navLink(page, '監視').click();
		await expect(page).toHaveURL(/\/monitor$/);
	});

	test('2. the restricted screens are not opened: the guard sends them to 監視', async () => {
		for (const { path } of RESTRICTED) {
			await page.goto(path);
			await expect(page).toHaveURL(/\/monitor$/);
			await expect(page.getByRole('heading', { level: 2, name: '監視' })).toBeVisible();
			await expect(loginButton(page)).toBeVisible();
		}
	});

	test('3. the sidebar shows only the allowed items', async () => {
		await page.goto('/monitor');
		for (const { label } of ALLOWED) {
			await expect(navLink(page, label)).toBeVisible();
		}
		for (const { label } of RESTRICTED) {
			await expect(navLink(page, label)).toHaveCount(0);
		}
		await expect(page.getByRole('navigation').getByRole('link')).toHaveCount(ALLOWED.length);
		// R1-D: 監視画面の本体（表示グループの一覧の読み取り）まで閲覧者として届く。
		// 一覧の取得に失敗すると「読み込めませんでした」になり、この空状態は出ない。
		// 編集者向けのグループ設定へのリンクは、本文にも（サイドバーと同じく）出さない。
		await expect(page.getByText('表示グループが未設定です')).toBeVisible();
		await expect(page.getByRole('link', { name: 'グループ設定' })).toHaveCount(0);
	});

	test('4. "ログイン" leads to the setup screen; signing in switches to a normal session', async () => {
		await loginButton(page).click();
		await expect(page).toHaveURL(/\/login$/);

		// Zero users -> setup form (same as smoke scenario 1). Playwright's CI
		// retry re-runs this whole serial group against the SAME server and DB
		// (the admin already exists after a first attempt got this far), so
		// the screen is a login form then: accept either, with the same
		// credentials. Scenarios 1-3 are unaffected by an existing account -
		// an anonymous visit is still the synthetic viewer while the flag is on.
		const displayName = page.getByLabel('表示名');
		const usernameField = page.getByLabel('ユーザー名');
		// Both forms have the username field; 表示名 exists only in the setup form.
		await expect(usernameField).toBeVisible();
		if (await displayName.isVisible()) {
			await displayName.fill(ADMIN_DISPLAY_NAME);
			await usernameField.fill(ADMIN_USERNAME);
			await page.getByLabel('パスワード（8文字以上）').fill(ADMIN_PASSWORD);
			await page.getByLabel('パスワード（確認）').fill(ADMIN_PASSWORD);
			await page.getByRole('button', { name: 'アカウントを作成' }).click();
		} else {
			await usernameField.fill(ADMIN_USERNAME);
			await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
			await page.getByRole('button', { name: 'ログイン', exact: true }).click();
		}

		await expect(page).toHaveURL(/\/monitor$/);
		await expect(page.getByRole('button', { name: 'ログアウト' })).toBeVisible();
		await expect(loginButton(page)).toHaveCount(0);
		// Full nav: every allowed and restricted item.
		for (const { label } of [...ALLOWED, ...RESTRICTED]) {
			await expect(navLink(page, label)).toBeVisible();
		}
		// A restricted screen now opens instead of bouncing, and survives a reload
		// (the restored session is the account, not a fresh viewer session).
		await page.goto('/users');
		await expect(page).toHaveURL(/\/users$/);
		await page.reload();
		await expect(page).toHaveURL(/\/users$/);
		await expect(page.getByRole('button', { name: 'ログアウト' })).toBeVisible();
	});

	test('5. 監視画面の 4 種が閲覧者にも値付きで描かれ、設定へのリンクを出さない', async ({
		browser
	}) => {
		const headers = await adminHeaders(page.request);
		await cleanupMonitorFixtures(page.request, headers);
		monitorFixturesCreated = true;
		const conn = await postJson<NamedRow>(page.request, headers, '/api/plc-connections', {
			name: PV_CONNECTION,
			protocol: 'modbus-tcp',
			// 到達しない TEST-NET（RFC 5737）。シミュレーションなので接続しに行かない。
			host: '192.0.2.1',
			port: 502,
			unitId: 1,
			enabled: true,
			wordOrder: '',
			simulation: true
		});
		const cg = await postJson<NamedRow>(page.request, headers, '/api/collection-groups', {
			name: PV_COLLECTION_GROUP,
			plcConnectionId: conn.id,
			periodMs: 500,
			enabled: true
		});
		const tagBase = { collectionGroupId: cg.id, dataType: 'u16', decimals: 0, enabled: true };
		// 工学値レンジのあるタグ（バー・計器が描ける）と、レンジの無いタグ（「レンジ未設定」。
		// 編集者にはタグ設定へのリンクが出るが、閲覧者には出さない）。
		const scaled = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: PV_TAG_SCALED,
			address: '40001',
			unit: 'cnt',
			rawLo: 0,
			rawHi: 65535,
			engLo: 0,
			engHi: 65535
		});
		const unset = await postJson<NamedRow>(page.request, headers, '/api/tags', {
			...tagBase,
			name: PV_TAG_UNSET,
			address: '40002'
		});
		for (const { name, kind } of PV_GROUPS) {
			await postJson<NamedRow>(page.request, headers, '/api/display-groups', {
				name,
				kind,
				attributes: {},
				pens: [scaled.id, unset.id].map((tagId) => ({ tagId, colorSlot: null }))
			});
		}
		const start = await page.request.post('/api/collect/start', { headers });
		expect(start.ok(), `POST /api/collect/start が ${start.status()}`).toBe(true);

		// ログインしていない別のコンテキスト = 閲覧公開の合成セッション。
		const context = await browser.newContext();
		try {
			const viewer = await context.newPage();
			await viewer.goto('/monitor');
			await expect(viewer).toHaveURL(/\/monitor$/);
			await expect(loginButton(viewer)).toBeVisible();
			const panel = viewer.getByRole('tabpanel');
			const cell = (name: string) => panel.getByRole('listitem').filter({ hasText: name });

			for (const { name, kind } of PV_GROUPS) {
				const tab = viewer.getByRole('tab', { name, exact: true });
				await tab.click();
				await expect(tab).toHaveAttribute('aria-selected', 'true');
				if (kind === 'trend') {
					const trend = panel.getByRole('region', { name: `${name} のトレンド表示` });
					await expect(trend).toBeVisible({ timeout: 20_000 });
					await expect(trend.locator('.chart-host svg path[fill="none"]').first()).toHaveAttribute(
						'd',
						/L/,
						{ timeout: 20_000 }
					);
				} else if (kind === 'gauge') {
					await expect(cell(PV_TAG_SCALED).locator('[role="img"]')).toHaveAttribute(
						'aria-label',
						new RegExp(`^${PV_TAG_SCALED} \\d+ cnt$`),
						{ timeout: 20_000 }
					);
					await expect(cell(PV_TAG_UNSET)).toContainText('レンジ未設定');
				} else if (kind === 'bar') {
					await expect(cell(PV_TAG_SCALED).locator('.fill')).toHaveCount(1, { timeout: 20_000 });
					await expect(cell(PV_TAG_SCALED).locator('.value')).toHaveText(/^\d+$/);
					await expect(cell(PV_TAG_UNSET)).toContainText('レンジ未設定');
				} else {
					await expect(cell(PV_TAG_SCALED).locator('.value')).toHaveText(/^\d+$/, {
						timeout: 20_000
					});
					await expect(cell(PV_TAG_SCALED)).toContainText('正常');
				}
				// 読み取り専用: 本文（タブの中）に設定画面（タグ設定・グループ設定・収集の
				// 設定）へのリンクを 1 つも出さない。編集者ならレンジ未設定のタグに
				// 「タグ設定を開く」が出る（user-simulator-monitor.spec.ts のテスト 5・6）。
				await expect(panel.getByRole('link')).toHaveCount(0);
			}
			await expect(viewer.getByRole('button', { name: 'ログアウト' })).toHaveCount(0);
		} finally {
			await context.close();
		}
	});
});
