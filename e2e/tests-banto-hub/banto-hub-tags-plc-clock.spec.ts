/**
 * 接続の編集 Drawer の「PLC の時計」（H2 #1065・H3 #1068 の REST の画面）。
 * 時計の 4 本の REST（`GET`・`PUT`・`POST /api/plc-connections/{id}/clock-sync` と
 * `GET .../clock`）は全部 `page.route` でモックする。PLC とは通信しない。
 */
import { expect, test, type Page, type Route } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const runId = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
const names = [`e2e-clock-a-${runId}`, `e2e-clock-b-${runId}`];

const defaultView = {
	settings: { enabled: false, schedule: { kind: 'hourly', minute: 5 } },
	last: null
};

function reading(plcTime: string, offsetSeconds: number) {
	return {
		at: '2026-11-07T01:00:00Z',
		result: 'ok',
		code: null,
		message: 'PLC の時計を読みました',
		plcTime,
		pcTime: '2026-11-07T10:00:00',
		offsetSeconds,
		cached: false
	};
}

async function openEditor(page: Page, name: string) {
	await page
		.getByRole('tree')
		.getByRole('button', { name: new RegExp(`^${name}`) })
		.click({ button: 'right' });
	await page.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();
	const drawer = page.getByRole('dialog', { name: `${name} を編集`, exact: true });
	await expect(drawer).toBeVisible();
	await expect(drawer.getByLabel('ホスト')).toBeVisible();
	return drawer;
}

async function closeEditor(drawer: ReturnType<Page['getByRole']>) {
	await drawer.getByRole('button', { name: '閉じる' }).click();
	await expect(drawer).toBeHidden();
}

interface ClockMock {
	clockGets: number;
	puts: unknown[];
	posts: number;
	holdClock(): void;
	nextClock(): Promise<Route>;
	holdSyncGet(): void;
	nextSyncGet(): Promise<Route>;
	holdPut(): void;
	nextPut(): Promise<Route>;
}

/** 時計の 4 本をモックする（GET clock-sync は既定、clock は保留もできる）。 */
async function mockClock(
	page: Page,
	opts: { postResult?: unknown; clockResult?: unknown } = {}
): Promise<ClockMock> {
	// 保留できる要求（clock の GET・clock-sync の GET・PUT）。各キューは同じ形。
	const makeQueue = () => {
		const routes: Route[] = [];
		let wake: (() => void) | undefined;
		return {
			hold: false,
			push(route: Route) {
				routes.push(route);
				wake?.();
			},
			async next() {
				if (routes.length === 0) await new Promise<void>((resolve) => (wake = resolve));
				wake = undefined;
				return routes.shift()!;
			}
		};
	};
	const clockQueue = makeQueue();
	const syncGetQueue = makeQueue();
	const putQueue = makeQueue();
	const mock: ClockMock = {
		clockGets: 0,
		puts: [],
		posts: 0,
		holdClock() {
			clockQueue.hold = true;
		},
		nextClock: () => clockQueue.next(),
		holdSyncGet() {
			syncGetQueue.hold = true;
		},
		nextSyncGet: () => syncGetQueue.next(),
		holdPut() {
			putQueue.hold = true;
		},
		nextPut: () => putQueue.next()
	};
	await page.route('**/api/plc-connections/*/clock-sync', (route) => {
		const method = route.request().method();
		if (method === 'GET') {
			if (syncGetQueue.hold) {
				syncGetQueue.push(route);
				return;
			}
			return route.fulfill({ json: defaultView });
		}
		if (method === 'PUT') {
			const body = route.request().postDataJSON();
			mock.puts.push(body);
			if (putQueue.hold) {
				putQueue.push(route);
				return;
			}
			return route.fulfill({ json: { settings: body, last: null } });
		}
		mock.posts += 1;
		return route.fulfill({ json: opts.postResult });
	});
	await page.route('**/api/plc-connections/*/clock', (route) => {
		mock.clockGets += 1;
		if (clockQueue.hold) {
			clockQueue.push(route);
			return;
		}
		void route.fulfill({ json: opts.clockResult });
	});
	return mock;
}

test.describe('PLC の時計（接続編集 Drawer）', () => {
	let ids: number[];
	let headers: Record<string, string>;

	test.beforeEach(async ({ page }) => {
		ids = [];
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		for (const name of names) {
			const response = await page.request.post('/api/plc-connections', {
				headers,
				data: {
					name,
					protocol: 'modbus-tcp',
					enabled: false,
					simulation: true,
					settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
				}
			});
			expect(response.ok()).toBe(true);
			ids.push(((await response.json()) as { id: number }).id);
		}
		await page.goto('/tags');
	});

	test.afterEach(async ({ page }) => {
		for (const id of ids) {
			await page.request.delete(`/api/plc-connections/${id}`, { headers });
		}
	});

	test('開いただけでは時計を読まず、ボタンで 1 回読んで差を出す', async ({ page }) => {
		const mock = await mockClock(page, { clockResult: reading('2026-11-07T10:00:03', 3) });
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		await expect(section.getByRole('heading', { name: 'PLC の時計' })).toBeVisible();
		await expect(section.getByText('まだ実行していません', { exact: false })).toBeVisible();
		await page.waitForTimeout(300);
		expect(mock.clockGets).toBe(0);
		await section.getByRole('button', { name: 'PLC の時計を読む' }).click();
		await expect(section.getByText('PLC: 2026-11-07 10:00:03')).toBeVisible();
		await expect(section.getByText('Hub の PC: 2026-11-07 10:00:00')).toBeVisible();
		await expect(section.getByText('差: +3 秒（PLC が進んでいる）')).toBeVisible();
		await expect(section.getByText('差は ±1 秒の誤差があります')).toBeVisible();
		expect(mock.clockGets).toBe(1);
	});

	test('設定を毎日 03:30・有効で保存でき、保存前は Esc で閉じない', async ({ page }) => {
		const mock = await mockClock(page);
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		await section.getByLabel('時刻表で定期的に合わせる').check();
		await section.getByLabel('時刻表', { exact: true }).selectOption('daily');
		await section.getByLabel('時', { exact: true }).fill('3');
		await section.getByLabel('分', { exact: true }).fill('30');
		await page.keyboard.press('Escape');
		await expect(drawer).toBeVisible();
		await section.getByRole('button', { name: '時計合わせの設定を保存' }).click();
		await expect.poll(() => mock.puts.length).toBe(1);
		expect(mock.puts[0]).toEqual({
			enabled: true,
			schedule: { kind: 'daily', hour: 3, minute: 30 }
		});
		await expect(section.getByText('現在の設定: 有効・毎日 03:30')).toBeVisible();
		await page.keyboard.press('Escape');
		await expect(drawer).toBeHidden();
	});

	test('手動の時計合わせは confirm を拒否すると送らず、承諾すると 1 回送って結果を出す', async ({
		page
	}) => {
		const mock = await mockClock(page, {
			postResult: {
				at: '2026-11-07T01:05:00Z',
				trigger: 'manual',
				time: '2026-11-07T10:05:00',
				result: 'rejected',
				code: null,
				message: 'ゲストが時計合わせを断りました',
				before: null,
				after: null
			}
		});
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		const button = section.getByRole('button', { name: '今すぐ PLC の時計を合わせる' });
		const dialogs: string[] = [];
		page.once('dialog', (dialog) => {
			dialogs.push(dialog.message());
			void dialog.dismiss();
		});
		await button.click();
		await expect.poll(() => dialogs.length).toBe(1);
		expect(dialogs[0]).toContain('24000');
		await page.waitForTimeout(300);
		expect(mock.posts).toBe(0);
		page.once('dialog', (dialog) => void dialog.accept());
		await button.click();
		await expect(section.getByText('結果: ドライバが断った')).toBeVisible();
		await expect(section.getByText('ゲストが時計合わせを断りました')).toBeVisible();
		await expect(section.getByText('渡した時刻: 2026-11-07 10:05:00')).toBeVisible();
		expect(mock.posts).toBe(1);
	});

	test('保留した読取りの遅着を、別の接続を開いた後に表示しない', async ({ page }) => {
		const mock = await mockClock(page);
		mock.holdClock();
		const first = await openEditor(page, names[0]);
		await first
			.getByRole('region', { name: 'PLC の時計' })
			.getByRole('button', { name: 'PLC の時計を読む' })
			.click();
		const old = await mock.nextClock();
		await closeEditor(first);
		const second = await openEditor(page, names[1]);
		const section = second.getByRole('region', { name: 'PLC の時計' });
		const response = page.waitForResponse((item) => item.request() === old.request());
		await old.fulfill({ json: reading('2099-01-01T00:00:00', 42) });
		await response;
		await expect(section.getByRole('button', { name: 'PLC の時計を読む' })).toBeEnabled();
		await expect(section.getByText('2099-01-01 00:00:00')).toHaveCount(0);
		await expect(section.getByTestId('clock-read-result')).toHaveCount(0);
	});

	const okRecord = {
		at: '2026-11-07T01:05:00Z',
		trigger: 'manual',
		time: '2026-11-07T10:05:00',
		result: 'ok',
		code: null,
		message: 'PLC の時計を合わせました',
		before: null,
		after: null
	};

	test('遅れた設定の読込み（last: null）が、先に終わった手動実行の結果を戻さない', async ({
		page
	}) => {
		const mock = await mockClock(page, { postResult: okRecord });
		mock.holdSyncGet();
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		const held = await mock.nextSyncGet();
		page.once('dialog', (dialog) => void dialog.accept());
		await section.getByRole('button', { name: '今すぐ PLC の時計を合わせる' }).click();
		await expect(section.getByText('PLC の時計を合わせました')).toBeVisible();
		const response = page.waitForResponse((item) => item.request() === held.request());
		await held.fulfill({ json: defaultView });
		await response;
		// 設定の欄は読込みの応答で有効になる（応答の処理が済んだ印）。
		await expect(section.getByLabel('時刻表', { exact: true })).toBeEnabled();
		await expect(section.getByText('PLC の時計を合わせました')).toBeVisible();
		await expect(section.getByText('まだ実行していません', { exact: false })).toHaveCount(0);
	});

	test('遅れた設定の保存の応答（last: null）が、先に終わった手動実行の結果を戻さない', async ({
		page
	}) => {
		const mock = await mockClock(page, { postResult: okRecord });
		mock.holdPut();
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		await section.getByLabel('時刻表で定期的に合わせる').check();
		await section.getByRole('button', { name: '時計合わせの設定を保存' }).click();
		const held = await mock.nextPut();
		page.once('dialog', (dialog) => void dialog.accept());
		await section.getByRole('button', { name: '今すぐ PLC の時計を合わせる' }).click();
		await expect(section.getByText('PLC の時計を合わせました')).toBeVisible();
		const response = page.waitForResponse((item) => item.request() === held.request());
		await held.fulfill({
			json: { settings: { enabled: true, schedule: { kind: 'hourly', minute: 5 } }, last: null }
		});
		await response;
		await expect(section.getByRole('button', { name: '時計合わせの設定を保存' })).toBeEnabled();
		await expect(section.getByText('PLC の時計を合わせました')).toBeVisible();
		await expect(section.getByText('まだ実行していません', { exact: false })).toHaveCount(0);
	});

	test('フォームの接続先が保存済みと違う間は読取りと手動の時計合わせを止める', async ({ page }) => {
		const mock = await mockClock(page, { clockResult: reading('2026-11-07T10:00:03', 3) });
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		const read = section.getByRole('button', { name: 'PLC の時計を読む' });
		const run = section.getByRole('button', { name: '今すぐ PLC の時計を合わせる' });
		await expect(read).toBeEnabled();
		await expect(run).toBeEnabled();
		await expect(section.getByTestId('clock-unsaved')).toHaveCount(0);
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		await expect(read).toBeDisabled();
		await expect(run).toBeDisabled();
		await expect(section.getByTestId('clock-unsaved')).toBeVisible();
		// 設定の保存（接続 id に付く）は止めない。
		await expect(section.getByRole('button', { name: '時計合わせの設定を保存' })).toBeEnabled();
		await drawer.getByLabel('ホスト').fill('127.0.0.1');
		await expect(read).toBeEnabled();
		await expect(run).toBeEnabled();
		await expect(section.getByTestId('clock-unsaved')).toHaveCount(0);
		expect(mock.clockGets).toBe(0);
	});

	test('プロトコルを一時的に替えても時計合わせの未保存の編集が消えない', async ({ page }) => {
		await mockClock(page);
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		const enabled = section.getByLabel('時刻表で定期的に合わせる');
		await expect(enabled).toBeEnabled();
		await enabled.check();
		await drawer.getByLabel('プロトコル').selectOption({ value: 'postgres' });
		await expect(drawer.getByRole('region', { name: 'PLC の時計' })).toBeVisible();
		await expect(enabled).toBeChecked();
		await drawer.getByLabel('プロトコル').selectOption({ value: 'modbus-tcp' });
		await expect(drawer.getByRole('region', { name: 'PLC の時計' })).toBeVisible();
		await expect(enabled).toBeChecked();
	});

	test('確認ダイアログに保存済みの接続名が入る', async ({ page }) => {
		await mockClock(page, { postResult: okRecord });
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		const messages: string[] = [];
		page.once('dialog', (dialog) => {
			messages.push(dialog.message());
			void dialog.dismiss();
		});
		await section.getByRole('button', { name: '今すぐ PLC の時計を合わせる' }).click();
		await expect.poll(() => messages.length).toBe(1);
		expect(messages[0]).toContain(`接続「${names[0]}」の PLC の時計`);
	});

	test('接続先を変えて保存すると前の PLC の読取り結果を消し、設定の編集は残す', async ({
		page
	}) => {
		await mockClock(page, { clockResult: reading('2026-11-07T10:00:03', 3) });
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		await section.getByRole('button', { name: 'PLC の時計を読む' }).click();
		await expect(section.getByTestId('clock-read-result')).toBeVisible();
		const enabled = section.getByLabel('時刻表で定期的に合わせる');
		await enabled.check();
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		await drawer.getByRole('button', { name: '保存', exact: true }).click();
		await expect(page.getByText('更新しました')).toBeVisible();
		await expect(section.getByTestId('clock-read-result')).toHaveCount(0);
		await expect(section.getByRole('button', { name: 'PLC の時計を読む' })).toBeEnabled();
		await expect(enabled).toBeChecked();
	});

	test('時計合わせの設定の保存中は × でも閉じず、終わったら閉じられる', async ({ page }) => {
		const mock = await mockClock(page);
		mock.holdPut();
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		await section.getByLabel('時刻表で定期的に合わせる').check();
		await section.getByRole('button', { name: '時計合わせの設定を保存' }).click();
		const held = await mock.nextPut();
		page.on('dialog', (dialog) => void dialog.dismiss());
		await drawer.getByRole('button', { name: '閉じる' }).click();
		await page.waitForTimeout(300);
		await expect(drawer).toBeVisible();
		const response = page.waitForResponse((item) => item.request() === held.request());
		await held.fulfill({
			json: { settings: { enabled: true, schedule: { kind: 'hourly', minute: 5 } }, last: null }
		});
		await response;
		await expect(section.getByRole('button', { name: '時計合わせの設定を保存' })).toBeEnabled();
		await drawer.getByRole('button', { name: '閉じる' }).click();
		await expect(drawer).toBeHidden();
	});

	test('手動の時計合わせの実行中は（未保存の編集が無くても）× で閉じず、終わったら閉じられる', async ({
		page
	}) => {
		await mockClock(page, { postResult: okRecord });
		// 後から登録した route が先に当たる。POST だけ保留し、他は上のモックへ回す。
		let heldPost: Route | undefined;
		await page.route('**/api/plc-connections/*/clock-sync', (route) => {
			if (route.request().method() === 'POST') {
				heldPost = route;
				return;
			}
			void route.fallback();
		});
		const drawer = await openEditor(page, names[0]);
		const section = drawer.getByRole('region', { name: 'PLC の時計' });
		// 確認ダイアログは全部承諾する（閉じる側の「変更を破棄しますか？」が出れば閉じてしまう）。
		page.on('dialog', (dialog) => void dialog.accept());
		await section.getByRole('button', { name: '今すぐ PLC の時計を合わせる' }).click();
		await expect.poll(() => heldPost !== undefined).toBe(true);
		await expect(section.getByRole('button', { name: '時計合わせ中…' })).toBeDisabled();
		await drawer.getByRole('button', { name: '閉じる' }).click();
		await page.waitForTimeout(300);
		await expect(drawer).toBeVisible();
		await heldPost!.fulfill({ json: okRecord });
		await expect(section.getByText('PLC の時計を合わせました')).toBeVisible();
		await drawer.getByRole('button', { name: '閉じる' }).click();
		await expect(drawer).toBeHidden();
	});
});
