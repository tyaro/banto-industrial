/**
 * 連続登録の基数/bit 連番（T18-3c、docs/banto-hub-t18-design.md
 * 「T18-3c 連続登録の基数/bit 連番」、TAG-UX-D）の実 DOM 受け入れテスト。
 * 受け入れ条件: `X1E→X1F→X20`（16進デバイス番号の桁上がり）、
 * `D100.E→D100.F→D101.0`（ワード内 bit 連番。bit サフィックスは T20-④で
 * 16進表記に是正済み — 旧テストの `D100.14→D100.15` は10進表記だった）。
 *
 * `banto-hub-tags-continuous.spec.ts` のプレビュー検証手法（`.preview-table
 * tbody tr` のセル値を見る）に倣う DOM 専用テスト（登録までは行わない）。
 * 前提データ（PLC接続・収集グループ）は `page.request` で直接 REST を叩いて
 * 作る（`simulation: true`、実 PLC 不要）。共有 DB を壊さないよう固定名は
 * `RUN_ID` で一意化し、リトライ再走用の冪等掃除を持つ。
 */
import { expect, test, type APIRequestContext, type Locator, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-radix-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-radix-group-${RUN_ID}`;

async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const existingGroup = groups.find((g) => g.name === GROUP_NAME);
		if (existingGroup) {
			await request.delete(`/api/collection-groups/${existingGroup.id}`, { headers });
		}
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as Array<{ id: number; name: string }>;
		const existingConnection = connections.find((c) => c.name === CONNECTION_NAME);
		if (existingConnection) {
			await request.delete(`/api/plc-connections/${existingConnection.id}`, { headers });
		}
	}
}

test.describe.serial('banto-hub 連続登録の基数/bit 連番 (T18-3c)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupExistingFixtures(page.request, authedHeaders);

		// プレビュー生成（`generateContinuousTags`）は接続プロトコルに依存せず
		// フォームの開始アドレス文字列を `parseSlmpAddress` で解釈するだけなので、
		// 他 spec と同じ modbus-tcp のシミュレーション接続で足りる（X/D 等の
		// デバイス記法のプレビューはこの DOM テストの範囲では protocol 非依存）。
		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);

		// 連続登録 Drawer を開く（各 test はアドレス/型/点数だけ差し替えて
		// プレビュー行を検証する）。
		//
		// T19 S1-c（UX-33）: 「連続登録」はツリーでグループが選択されている
		// ときしか出ない上、開いた Drawer は選択中グループへ確定済み
		// （対象グループの `<select>` が disabled）になる - まずツリーで
		// 対象グループを選ぶ（`groupNodeByName` は `banto-hub-auth.ts`
		// 参照）。以前は Drawer を開いてから `<select>` で選ぶ実装だった。
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '連続登録' }).click();
		await expect(page.getByRole('dialog', { name: '連続登録' })).toBeVisible();
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	/** プレビュー各行のアドレス列（3列目）が `expected` と順に一致することを検証する。 */
	async function expectPreviewAddresses(expected: string[]): Promise<void> {
		const rows: Locator = page.locator('.preview-table tbody tr');
		await expect(rows).toHaveCount(expected.length);
		for (let i = 0; i < expected.length; i++) {
			// 列は「# / 名前 / アドレス」。アドレスは3列目（nth(2)）。
			await expect(rows.nth(i).locator('td').nth(2)).toHaveText(expected[i]);
		}
	}

	test('1. 16進デバイス番号連番: 開始 X1E・bit・点数3 → X1E, X1F, X20', async () => {
		const drawer = page.getByRole('dialog', { name: '連続登録' });
		await drawer.getByLabel('データ型').selectOption({ value: 'bit' });
		await drawer.getByLabel('開始アドレス').fill('X1E');
		await drawer.getByLabel('点数').fill('3');
		await expectPreviewAddresses(['X1E', 'X1F', 'X20']);
	});

	test('2. ワード内 bit 連番: 開始 D100.E・bit・点数3 → D100.E, D100.F, D101.0', async () => {
		const drawer = page.getByRole('dialog', { name: '連続登録' });
		await drawer.getByLabel('データ型').selectOption({ value: 'bit' });
		await drawer.getByLabel('開始アドレス').fill('D100.E');
		await drawer.getByLabel('点数').fill('3');
		await expectPreviewAddresses(['D100.E', 'D100.F', 'D101.0']);
	});
});
