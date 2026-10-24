/**
 * R2b-6c（docs/banto-hub-driver-ui-design.md §6・§7.1）: 機器のドライバの接続を
 * `GET /api/drivers` のスキーマから組み立てた欄で作る受け入れテスト。
 *
 * 固定する内容:
 * 1. 作成ウィザードの既定のプロトコルはドライバ一覧の先頭（登録順で SLMP）で、欄の既定は
 *    スキーマの `default`（ポート 5007）。必須のホストが空のうちは「次へ」に進めない。
 * 2. Modbus へ替えると、触っていないポートは Modbus の既定 502 に追従し、触って入れたホストは
 *    引き継がれる。SLMP へ戻すと既定 5007 に追従する。ヘルプ（CPU 種別）はスキーマの `help` で出る。
 *    ワード順は「詳細」に畳まれる。
 * 3. 範囲外のポートは保存前に欄の下で断られる（サーバーと同じ文言）。
 * 4. 作成すると `settings` は snake_case のまま保存され（REST で確かめる）、再設定の
 *    ドロワーが同じ値を読み戻す。
 * 5. ホストの接続（postgres）から替えたドライバは既定で埋まる（持ち越さない）。
 *
 * 前提データは REST で作る/消す。同期点は `waitForResponse`（クリックより前に張る）。
 */
import { expect, test, type APIRequestContext, type Page, type Route } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONN = `e2e-driver-conn-${RUN_ID}`;

interface ConnectionRow {
	id: number;
	name: string;
	protocol: string;
	settings: Record<string, unknown>;
}

async function cleanupFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const res = await request.get('/api/plc-connections', { headers });
	if (!res.ok()) return;
	for (const c of ((await res.json()) as ConnectionRow[]).filter((c) => c.name === CONN)) {
		await request.delete(`/api/plc-connections/${c.id}`, { headers });
	}
}

test.describe.serial('banto-hub ドライバの接続の作成・再設定（R2b-6c）', () => {
	let page: Page;
	let headers: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await cleanupFixtures(page.request, headers);
	});

	test.afterAll(async () => {
		await cleanupFixtures(page.request, headers);
		await page.close();
	});

	test('1〜3. 既定はドライバの先頭（SLMP）。Modbus へ替えると既定が追従し、範囲外は保存前に断られる', async () => {
		let testCalls = 0;
		page.on('request', (req) => {
			if (req.url().includes('/api/plc-connections/test')) testCalls += 1;
		});
		await page.goto('/tags');
		await page.getByRole('button', { name: 'PLC接続を追加', exact: true }).click();
		const wizard = page.getByRole('dialog', { name: '新規作成', exact: true });
		await expect(wizard).toBeVisible();
		await wizard.locator('#connection-name').fill(CONN);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();

		// 既定のプロトコルは drivers の先頭（登録順で slmp）、ポートはスキーマの既定。
		await expect(wizard.getByLabel('プロトコル')).toHaveValue('slmp');
		await expect(wizard.getByLabel('ポート')).toHaveValue('5007');
		await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeDisabled();

		// ホストを入れる（触った欄）→ Modbus へ替える。ホストは引き継がれ、ポートは Modbus の既定。
		await wizard.getByLabel('ホスト').fill('10.0.0.7');
		await wizard.getByLabel('プロトコル').selectOption({ value: 'modbus-tcp' });
		await expect(wizard.getByLabel('ホスト')).toHaveValue('10.0.0.7');
		await expect(wizard.getByLabel('ポート')).toHaveValue('502');

		// SLMP へ戻す。ホストは引き継がれ、触っていないポートは SLMP の既定に追従する。
		await wizard.getByLabel('プロトコル').selectOption({ value: 'slmp' });
		await expect(wizard.getByLabel('ホスト')).toHaveValue('10.0.0.7');
		await expect(wizard.getByLabel('ポート')).toHaveValue('5007');
		await expect(wizard.getByLabel('CPU種別')).toHaveValue('R');
		// help はスキーマから（欄の下）。ワード順は「詳細」に畳まれている。
		await expect(wizard.getByText('MELSEC のフレーム形式です')).toBeVisible();
		await expect(wizard.getByTestId('driver-advanced')).not.toHaveAttribute('open', '');

		// 範囲外のポートは送る前に欄の下で断る（サーバーと同じ文言）。
		// 手順 2 の「次へ」で断られ（手順 3 へ進まない）、接続テストの API も呼ばれない。
		await wizard.getByLabel('ポート').fill('70000');
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();
		await expect(wizard.getByText('1〜65535 で指定してください')).toBeVisible();
		await expect(wizard.getByLabel('プロトコル')).toBeVisible();
		await expect(wizard.getByRole('button', { name: '作成', exact: true })).toHaveCount(0);
		expect(testCalls).toBe(0);

		// 直して作成する。
		await wizard.getByLabel('ポート').fill('5200');
		await wizard.getByLabel('CPU種別').selectOption({ value: 'Q' });
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();
		await expect(wizard.getByText('Qシリーズ（Q）')).toBeVisible();
		const created = page.waitForResponse(
			(r) => r.url().includes('/api/plc-connections') && r.request().method() === 'POST'
		);
		await wizard.getByRole('button', { name: '作成', exact: true }).click();
		expect((await created).status()).toBe(200);
		await expect(page.getByText('作成しました')).toBeVisible();
	});

	test('4. settings は snake_case のまま保存され、再設定のドロワーが読み戻す', async () => {
		const res = await page.request.get('/api/plc-connections', { headers });
		const saved = ((await res.json()) as ConnectionRow[]).find((c) => c.name === CONN);
		expect(saved?.protocol).toBe('slmp');
		expect(saved?.settings).toMatchObject({
			host: '10.0.0.7',
			port: 5200,
			cpu_type: 'Q',
			word_order: 'low_high'
		});

		await page.goto('/tags');
		await page
			.getByRole('tree')
			.getByRole('button', { name: new RegExp(`^${CONN}`) })
			.click({ button: 'right' });
		await page.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();
		const drawer = page.getByRole('dialog', { name: `${CONN} を編集`, exact: true });
		await expect(drawer).toBeVisible();
		await expect(drawer.getByLabel('ホスト')).toHaveValue('10.0.0.7');
		await expect(drawer.getByLabel('ポート')).toHaveValue('5200');
		await expect(drawer.getByLabel('CPU種別')).toHaveValue('Q');
		await drawer.getByText('詳細').click();
		await expect(drawer.getByLabel('ワード順')).toHaveValue('low_high');
	});

	test('5. ホストの接続（postgres）から替えたドライバは既定で埋まる', async () => {
		await page.goto('/tags');
		await page.getByRole('button', { name: 'PLC接続を追加', exact: true }).click();
		const wizard = page.getByRole('dialog', { name: '新規作成', exact: true });
		await wizard.locator('#connection-name').fill(`${CONN}-x`);
		await wizard.getByRole('button', { name: '次へ', exact: true }).click();
		await wizard.getByLabel('ホスト').fill('leftover');
		await wizard.getByLabel('プロトコル').selectOption({ value: 'postgres' });
		await wizard.getByLabel('プロトコル').selectOption({ value: 'slmp' });
		await expect(wizard.getByLabel('ホスト')).toHaveValue('');
		await expect(wizard.getByLabel('ポート')).toHaveValue('5007');
	});

	test('6. 一覧の取得に失敗したまま名前だけ変えて保存しても settings は変わらない', async () => {
		const res = await page.request.get('/api/plc-connections', { headers });
		const before = ((await res.json()) as ConnectionRow[]).find((c) => c.name === CONN);
		expect(before).toBeTruthy();

		await page.route('**/api/drivers', (route) => route.fulfill({ status: 500, body: 'x' }));
		try {
			await page.goto('/tags');
			await page
				.getByRole('tree')
				.getByRole('button', { name: new RegExp(`^${CONN}`) })
				.click({ button: 'right' });
			await page.getByRole('menuitem', { name: '接続を再設定', exact: true }).click();
			const drawer = page.getByRole('dialog', { name: `${CONN} を編集`, exact: true });
			await expect(drawer).toBeVisible();
			await drawer.locator('#connection-name').fill(`${CONN}-r`);
			const saved = page.waitForResponse(
				(r) => r.url().includes('/api/plc-connections/') && r.request().method() === 'PUT'
			);
			await drawer.getByRole('button', { name: '保存', exact: true }).click();
			expect((await saved).status()).toBe(200);
		} finally {
			await page.unroute('**/api/drivers');
		}

		const after = (
			(await (
				await page.request.get('/api/plc-connections', { headers })
			).json()) as ConnectionRow[]
		).find((c) => c.id === before!.id);
		expect(after?.name).toBe(`${CONN}-r`);
		expect(after?.settings).toMatchObject({ host: '10.0.0.7', port: 5200 });
		await page.request.put(`/api/plc-connections/${before!.id}`, {
			headers,
			data: {
				name: CONN,
				protocol: before!.protocol,
				enabled: true,
				simulation: false,
				settings: before!.settings
			}
		});
	});

	test('7. 一覧の取得待ちに入力した名前・手順は、取得が終わっても巻き戻らない', async () => {
		await page.goto('/tags');
		let held: Route | null = null;
		let arrived!: () => void;
		const reached = new Promise<void>((resolve) => (arrived = resolve));
		await page.route('**/api/drivers', (route) => {
			held = route;
			arrived();
		});
		try {
			await page.getByRole('button', { name: 'PLC接続を追加', exact: true }).click();
			const wizard = page.getByRole('dialog', { name: '新規作成', exact: true });
			await expect(wizard).toBeVisible();
			await reached;

			// 取得待ちの間に名前を入れて手順 2 へ進む（ドライバの欄に頼る操作は止まっている）。
			await wizard.locator('#connection-name').fill(`${CONN}-hold`);
			await wizard.getByRole('button', { name: '次へ', exact: true }).click();
			await expect(wizard.getByTestId('drivers-loading')).toBeVisible();
			await expect(wizard.getByRole('button', { name: '次へ', exact: true })).toBeDisabled();

			// 取得が終わっても、手順は 2 のまま・名前は入力したまま。ドライバの欄だけが補われる。
			await held!.continue();
			await expect(wizard.getByTestId('drivers-loading')).toHaveCount(0);
			await expect(wizard.getByLabel('プロトコル')).toHaveValue('slmp');
			await expect(wizard.getByLabel('ポート')).toHaveValue('5007');
			await wizard.getByRole('button', { name: '戻る', exact: true }).click();
			await expect(wizard.locator('#connection-name')).toHaveValue(`${CONN}-hold`);
		} finally {
			await page.unroute('**/api/drivers');
		}
	});
});
