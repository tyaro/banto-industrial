/**
 * TAG-P0-3: pending apply/cancel 導線の実 DOM 固定。
 *
 * - 収集中に構成変更を送ると pending へ積まれる
 * - status 画面の Pending changes からキャンセル/適用できる
 *
 * **この spec だけ「ロックダウン済み専用サーバー」で走る（#341、2026-09-14）**:
 * 2026-09-09 オーナー決定により、**試運転モード（未ロックダウン）では収集中の
 * 構成 CRUD が未適用キューを経由せず即時・無停止で反映される**ようになった
 * （`docs/tag-server-design.md` §4.3）。このスイートの webServer は一度も
 * ロックダウンしないので、そのままだと `queueTagWhileRunning` が 202 ではなく
 * 200 を受け取って成立しない。かといって共有サーバーを途中でロックダウンすると
 * 後続の spec（初回セットアップ前提の smoke、認証バイパス前提のもの）が壊れる。
 * そこで `banto-hub.playwright.config.ts` に**別ポート・別 profile の2台目**と
 * `chromium-locked-down` プロジェクトを用意し、この spec だけをそこで実行する
 * （upstream banto の e2e `public-viewer` プロジェクトと同じ型）。下の
 * `beforeAll` が初回セットアップ直後にそのサーバーをロックダウンする。
 *
 * ファイル名について: `banto-hub-smoke.spec.ts` の test 1「first-run setup」
 * だけが「DB 未初期化＝初回セットアップ画面が出る」ことを実 DOM で検証する
 * （`banto-hub-auth.ts` の `fetchAuthToken` 参照）。本 spec は #341 以降
 * 別サーバー・別 DB になったので smoke とは干渉しなくなったが、ファイル名は
 * `banto-hub-status-pending-...`（`st` > `sm`）のまま据え置く（辞書順の前提を
 * 崩さない）。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-pending-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-pending-group-${RUN_ID}`;

test.describe.serial('banto-hub pending apply/cancel', () => {
	let page: Page | undefined;
	let token = '';
	let groupId = 0;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// #341: この spec は**ロックダウン済み**の契約（収集中の CRUD は
		// 未適用キューへ、適用は明示操作で・ただし無停止）を固定する。
		// 専用サーバー（`chromium-locked-down` プロジェクトの baseURL）
		// なので、ここで不可逆のロックダウンを実行してよい。冪等なので
		// Playwright の retry で2回目に入っても失敗しない。
		const lockDownRes = await page.request.post('/api/commissioning/lock-down', {
			headers: authedHeaders
		});
		expect(lockDownRes.ok(), await lockDownRes.text()).toBe(true);
		const commissioningRes = await page.request.get('/api/commissioning/status', {
			headers: CSRF_HEADERS
		});
		expect(commissioningRes.ok()).toBe(true);
		const commissioning = (await commissioningRes.json()) as { lockedDown?: boolean };
		expect(
			commissioning.lockedDown,
			'この spec はロックダウン済み専用サーバーで走る前提（#341）'
		).toBe(true);

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				host: '127.0.0.1',
				port: 15022
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: {
				name: GROUP_NAME,
				plcConnectionId: connection.id,
				periodMs: 100
			}
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };
		groupId = group.id;
	});

	test.afterAll(async () => {
		if (page) {
			await page.close();
		}
	});

	async function queueTagWhileRunning(tagName: string): Promise<number> {
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		const startRes = await page.request.post('/api/collection/start-all-simulation', {
			headers: authedHeaders
		});
		expect(startRes.ok()).toBe(true);
		const startStatus = (await startRes.json()) as {
			state?: string;
			mode?: string;
		};
		expect(startStatus.state).toBe('running');
		expect(startStatus.mode).toBe('all_simulation');

		const queuedRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: tagName,
				collectionGroupId: groupId,
				address: '40001',
				dataType: 'i16'
			}
		});
		const queuedBody = await queuedRes.text();
		expect(
			queuedRes.status(),
			`running中のtags createは pending へ積まれる (body=${queuedBody})`
		).toBe(202);
		const queued = JSON.parse(queuedBody) as {
			pending: { id: number };
		};
		expect(typeof queued.pending.id).toBe('number');
		return queued.pending.id;
	}

	test('1. Pending changes 画面からキャンセルできる', async () => {
		const pendingId = await queueTagWhileRunning(`e2e-pending-cancel-${RUN_ID}`);

		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'Pending changes' })).toBeVisible();

		const row = page.locator('tbody tr', { hasText: `#${pendingId}` });
		await expect(row).toBeVisible();
		await row.getByRole('button', { name: 'キャンセル' }).click();

		await expect(row).toContainText('キャンセル済み');
		await expect(row.getByRole('button', { name: 'キャンセル' })).toHaveCount(0);

		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		const stopRes = await page.request.post('/api/collection/stop', { headers: authedHeaders });
		expect(stopRes.ok()).toBe(true);
	});

	// #341（2026-09-14 オーナー回答「明示適用も無停止」）: 以前はここで
	// 収集を止めてから適用していた（止めないと 409 だったため）。適用の
	// 収集停止要求は撤廃されたので、稼働させたまま適用する形へ改めた。
	test('2. Pending changes 画面から適用できる（収集を止めずに）', async () => {
		const pendingId = await queueTagWhileRunning(`e2e-pending-apply-${RUN_ID}`);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'Pending changes' })).toBeVisible();

		const row = page.locator('tbody tr', { hasText: `#${pendingId}` });
		await expect(row).toBeVisible();
		await row.getByRole('button', { name: '適用' }).click();

		await expect(row).toContainText('適用済み');
		await expect(row.getByRole('button', { name: '適用' })).toHaveCount(0);

		const tagsRes = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(tagsRes.ok()).toBe(true);
		const tags = (await tagsRes.json()) as Array<{ name: string }>;
		expect(tags.some((tag) => tag.name === `e2e-pending-apply-${RUN_ID}`)).toBe(true);

		// 収集は止まっていない（無停止適用）。
		const statusRes = await page.request.get('/api/status', { headers: authedHeaders });
		expect(statusRes.ok()).toBe(true);
		const runtime = (await statusRes.json()) as { collectionState?: string };
		expect(runtime.collectionState).toBe('running');

		const stopRes = await page.request.post('/api/collection/stop', { headers: authedHeaders });
		expect(stopRes.ok()).toBe(true);
	});

	// #341（2026-09-14）: 一過性の失敗の作り方を差し替えた。以前は
	// 「収集稼働中に適用 → 409 collection_edit_locked」を使っていたが、
	// その 409 自体が撤廃された（適用は無停止で通る）。代わりに、同じ名前の
	// タグを先に作っておいて一意制約で失敗させる - requeue 導線が対象と
	// している「一過性の失敗」の実例そのもの。
	test('3. 失敗した提案を再試行して適用できる（一過性失敗からの回復）', async () => {
		const tagName = `e2e-pending-requeue-${RUN_ID}`;
		const pendingId = await queueTagWhileRunning(tagName);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 収集を止めて（＝ queue を経由しない状態にして）同名タグを直接作る。
		const stopRes = await page.request.post('/api/collection/stop', { headers: authedHeaders });
		expect(stopRes.ok()).toBe(true);
		const blockerRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: tagName,
				collectionGroupId: groupId,
				address: '40002',
				dataType: 'i16'
			}
		});
		expect(blockerRes.ok()).toBe(true);
		const blocker = (await blockerRes.json()) as { id: number };

		await page.goto('/status');
		await expect(page.getByRole('heading', { level: 2, name: 'Pending changes' })).toBeVisible();

		const row = page.locator('tbody tr', { hasText: `#${pendingId}` });
		await expect(row).toBeVisible();

		// 一意制約に阻まれて failed になる。
		await row.getByRole('button', { name: '適用' }).click();
		await expect(row).toContainText('失敗');

		// 再試行で pending に差し戻る。
		await row.getByRole('button', { name: '再試行' }).click();
		await expect(row).toContainText('保留中');
		await expect(row.getByRole('button', { name: '再試行' })).toHaveCount(0);

		// 失敗要因（同名タグ）を取り除くと適用できる。
		const deleteRes = await page.request.delete(`/api/tags/${blocker.id}`, {
			headers: authedHeaders
		});
		expect(deleteRes.ok()).toBe(true);

		await row.getByRole('button', { name: '適用' }).click();
		await expect(row).toContainText('適用済み');
	});
});
