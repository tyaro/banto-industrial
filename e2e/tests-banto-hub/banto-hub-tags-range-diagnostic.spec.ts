/**
 * #845: the connection editor must only show diagnostics from its current
 * opening and current settings. All diagnostic responses are mocked; no PLC
 * connection is made by this spec.
 */
import { expect, test, type Page, type Route } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const runId = `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
const names = [`e2e-range-a-${runId}`, `e2e-range-b-${runId}`];

function report(reason: string) {
	return {
		status: 'current',
		model: null,
		modelReason: 'CPU 型式を取得できません',
		counts: [
			{
				area: 'D',
				scope: 'device',
				unit: 'points',
				bound: 'unknown',
				points: null,
				reason: '設定点数を取得できません',
				source: 'mock'
			}
		],
		tags: [
			{ tagId: 1, address: 'D100', verdict: 'unknown', reason },
			{ tagId: 2, address: 'D9999', verdict: 'outside', reason: '設定範囲を超過' }
		]
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

async function mockDiagnostics(page: Page) {
	const pending: Route[] = [];
	const pendingGets: Route[] = [];
	let holdGets = false;
	let wake: (() => void) | undefined;
	let wakeGet: (() => void) | undefined;
	await page.route('**/api/plc-connections/*/diagnostics', (route) => {
		if (route.request().method() === 'GET') {
			if (!holdGets) {
				void route.fulfill({ json: report('現在の結果') });
			} else {
				pendingGets.push(route);
				wakeGet?.();
			}
			return;
		}
		pending.push(route);
		wake?.();
	});
	return {
		holdGets() {
			holdGets = true;
		},
		async next() {
			if (pending.length === 0) await new Promise<void>((resolve) => (wake = resolve));
			wake = undefined;
			return pending.shift()!;
		},
		async nextGet() {
			if (pendingGets.length === 0) await new Promise<void>((resolve) => (wakeGet = resolve));
			wakeGet = undefined;
			return pendingGets.shift()!;
		}
	};
}

test.describe('#845 接続編集の範囲診断表示', () => {
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

	test('unknown の理由と outside を分け、接続成功を全タグ成功として扱わない', async ({ page }) => {
		await page.route('**/api/plc-connections/test', (route) =>
			route.fulfill({ json: { ok: true, elapsedMs: 5, error: null } })
		);
		await page.route('**/api/plc-connections/*/diagnostics', (route) =>
			route.fulfill({ json: report('点数取得失敗') })
		);
		const drawer = await openEditor(page, names[0]);
		await drawer.getByRole('button', { name: '接続テスト' }).click();
		await expect(drawer.getByText('接続成功（応答 5ms）')).toBeVisible();
		const diagnostic = drawer.getByRole('region', { name: 'Device range diagnostic' });
		await diagnostic.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		await expect(
			diagnostic.getByText('D: 不明 (設定点数を取得できません)', { exact: false })
		).toBeVisible();
		await diagnostic.getByText('タグ別の範囲判定（2件）').click();
		await expect(diagnostic.getByText('D100: 判定不能 — 点数取得失敗')).toBeVisible();
		await expect(diagnostic.getByText('D9999: 範囲外 — 設定範囲を超過')).toBeVisible();
		await expect(drawer.getByText('接続成功（応答 5ms）')).toBeVisible();
	});

	test('別接続に切り替えた後の遅着結果を表示しない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const first = await openEditor(page, names[0]);
		await expect(first.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await first.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const old = await mock.next();
		await closeEditor(first);
		const second = await openEditor(page, names[1]);
		await expect(second.getByText('現在の結果')).toHaveCount(0);
		const oldResponse = page.waitForResponse((response) => response.request() === old.request());
		await old.fulfill({ json: report('旧接続の結果') });
		await oldResponse;
		await expect(second.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
		await expect(second.getByText('旧接続の結果')).toHaveCount(0);
	});

	test('閉じて同じ接続を再表示した後の遅着結果を表示しない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const first = await openEditor(page, names[0]);
		await expect(first.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await first.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const old = await mock.next();
		await closeEditor(first);
		const reopened = await openEditor(page, names[0]);
		await expect(reopened.getByText('現在の結果')).toHaveCount(0);
		const oldResponse = page.waitForResponse((response) => response.request() === old.request());
		await old.fulfill({ json: report('前回開いた時の結果') });
		await oldResponse;
		await expect(reopened.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
		await expect(reopened.getByText('前回開いた時の結果')).toHaveCount(0);
	});

	test('未保存の接続先変更後に旧設定の結果を表示しない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const drawer = await openEditor(page, names[0]);
		await expect(drawer.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await drawer.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const old = await mock.next();
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		const oldResponse = page.waitForResponse((response) => response.request() === old.request());
		await old.fulfill({ json: report('旧設定の結果') });
		await oldResponse;
		await expect(drawer.getByRole('button', { name: '機器の設定範囲を確認' })).toBeDisabled();
		await expect(drawer.getByText('保存後に診断してください。')).toBeVisible();
		await expect(drawer.getByText('旧設定の結果')).toHaveCount(0);
	});

	test('未保存の接続先変更中は GET の current と POST を使用しない', async ({ page }) => {
		let gets = 0;
		let posts = 0;
		await page.route('**/api/plc-connections/*/diagnostics', (route) => {
			if (route.request().method() === 'GET') gets += 1;
			else posts += 1;
			void route.fulfill({ json: report('保存済み設定の結果') });
		});
		const drawer = await openEditor(page, names[0]);
		const diagnostic = drawer.getByRole('region', { name: 'Device range diagnostic' });
		await expect(diagnostic.getByText('保存済み設定の結果')).toHaveCount(1);
		await page.waitForTimeout(200);
		const initialGets = gets;
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		await expect(diagnostic.getByText('保存後に診断してください。')).toBeVisible();
		await expect(diagnostic.getByText('保存済み設定の結果')).toHaveCount(0);
		await expect(diagnostic.getByRole('button', { name: '機器の設定範囲を確認' })).toBeDisabled();
		await page.waitForTimeout(200);
		expect(gets).toBe(initialGets);
		expect(posts).toBe(0);
		await drawer.getByLabel('ホスト').fill('127.0.0.1');
		await expect(diagnostic.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
		await expect(diagnostic.getByText('保存済み設定の結果')).toHaveCount(1);
		await drawer.getByLabel('名前', { exact: true }).fill('名前のみ変更');
		await expect(diagnostic.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
		await expect(diagnostic.getByText('保存済み設定の結果')).toHaveCount(1);
	});

	test('変更前に開始した GET の current を未保存設定に表示しない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		mock.holdGets();
		const drawer = await openEditor(page, names[0]);
		const oldGet = await mock.nextGet();
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		await expect(drawer.getByText('保存後に診断してください。')).toBeVisible();
		const response = page.waitForResponse((item) => item.request() === oldGet.request());
		await oldGet.fulfill({ json: report('保存済み A の GET') });
		await response;
		await expect(drawer.getByText('保存済み A の GET')).toHaveCount(0);
	});

	test('旧 POST の失敗は新しい設定の診断と通知を壊さない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const drawer = await openEditor(page, names[0]);
		await expect(drawer.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await drawer.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const old = await mock.next();
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		await expect(drawer.getByText('保存後に診断してください。')).toBeVisible();
		await drawer.getByLabel('ホスト').fill('127.0.0.1');
		const freshGet = await mock.nextGet();
		await freshGet.fulfill({ json: report('現在の設定の結果') });
		await expect(drawer.getByText('現在の設定の結果')).toHaveCount(1);
		const response = page.waitForResponse((item) => item.request() === old.request());
		await old.fulfill({ status: 500, json: { kind: 'other', message: 'OLD_POST_ERROR' } });
		await response;
		await expect(drawer.getByText('現在の設定の結果')).toHaveCount(1);
		await expect(drawer.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
		await expect(page.getByText('OLD_POST_ERROR')).toHaveCount(0);
	});

	test('最新 POST の待機中に旧 POST が終わっても診断中を解除しない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const drawer = await openEditor(page, names[0]);
		await expect(drawer.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await drawer.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const old = await mock.next();
		await drawer.getByLabel('ホスト').fill('127.0.0.2');
		await expect(drawer.getByText('保存後に診断してください。')).toBeVisible();
		await drawer.getByLabel('ホスト').fill('127.0.0.1');
		await expect(drawer.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
		await drawer.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const fresh = await mock.next();
		await expect(drawer.getByRole('button', { name: '診断中…' })).toBeDisabled();
		const oldResponse = page.waitForResponse((item) => item.request() === old.request());
		await old.fulfill({ json: report('旧 POST') });
		await oldResponse;
		await expect(drawer.getByRole('button', { name: '診断中…' })).toBeDisabled();
		await expect(drawer.getByText('旧 POST')).toHaveCount(0);
		await fresh.fulfill({ json: report('最新 POST') });
		await expect(drawer.getByText('最新 POST')).toHaveCount(1);
		await expect(drawer.getByRole('button', { name: '機器の設定範囲を確認' })).toBeEnabled();
	});

	test('再診断中は連打を防ぎ、先行 GET の遅着で新結果を戻さない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const drawer = await openEditor(page, names[0]);
		await expect(drawer.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await drawer.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const first = await mock.next();
		await expect(drawer.getByRole('button', { name: '診断中…' })).toBeDisabled();
		await first.fulfill({ json: report('初回の結果') });
		await expect(drawer.getByText('初回の結果')).toHaveCount(1);
		const staleGet = await mock.nextGet();
		await drawer.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const second = await mock.next();
		await expect(drawer.getByRole('button', { name: '診断中…' })).toBeDisabled();
		await second.fulfill({ json: report('再診断の結果') });
		await expect(drawer.getByText('再診断の結果')).toHaveCount(1);
		const staleResponse = page.waitForResponse(
			(response) => response.request() === staleGet.request()
		);
		await staleGet.fulfill({ json: report('古い GET の結果') });
		await staleResponse;
		await expect(drawer.getByText('再診断の結果')).toHaveCount(1);
		await expect(drawer.getByText('古い GET の結果')).toHaveCount(0);
	});

	test('再表示後は旧リクエストを待たず再診断でき、連打で旧結果を戻さない', async ({ page }) => {
		const mock = await mockDiagnostics(page);
		const first = await openEditor(page, names[0]);
		await expect(first.getByText('現在の結果')).toHaveCount(1);
		mock.holdGets();
		await first.getByRole('button', { name: '機器の設定範囲を確認' }).click();
		const old = await mock.next();
		await closeEditor(first);
		const reopened = await openEditor(page, names[0]);
		await expect(reopened.getByText('現在の結果')).toHaveCount(0);
		const button = reopened.getByRole('button', { name: '機器の設定範囲を確認' });
		await expect(button).toBeEnabled();
		await button.click();
		const fresh = await mock.next();
		await expect(reopened.getByRole('button', { name: '診断中…' })).toBeDisabled();
		const freshResponse = page.waitForResponse(
			(response) => response.request() === fresh.request()
		);
		await fresh.fulfill({ json: report('最新の結果') });
		await freshResponse;
		await expect(reopened.getByText('最新の結果')).toHaveCount(1);
		const oldResponse = page.waitForResponse((response) => response.request() === old.request());
		await old.fulfill({ json: report('旧結果') });
		await oldResponse;
		await expect(reopened.getByText('旧結果')).toHaveCount(0);
	});
});
