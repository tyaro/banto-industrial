/**
 * タグ一覧の初期読込失敗・空・検索結果0件の区別（T18-1、TAG-UX-C 6点目、
 * docs/banto-hub-desktop-plan.md §9.4「初期読込失敗、0件、検索結果0件、
 * 再読込中、stale 一覧を区別し、画面内に再試行を置く。通信失敗を『タグ0件』
 * と表示しない」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-form.spec.ts`/`banto-hub-tags-busy.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データ
 * （PLC接続・収集グループ・タグ）は `page.request` で直接 REST を叩いて
 * 作る。`GET /api/tags` だけを `page.route` で失敗させ、groups/connections
 * は成功させたまま `tags` の読込だけを落とす（`Promise.all` 全体が catch
 * されて `loadError` になる想定どおりの挙動）。
 *
 * トーストの通知文（`ToastHost.svelte` の `.message`）も同じエラー文言を
 * 一時的に表示するため、画面内バナーの検証は `.right-pane .err`
 * （右ペイン内のインラインエラーのみ）に限定し、トーストとの重複マッチを
 * 避ける。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const CONNECTION_NAME = 'e2e-load-state-plc';
const GROUP_NAME = 'e2e-load-state-group';
const TAG_NAME = 'e2e-load-state-tag';

test.describe.serial('banto-hub タグ一覧の初期読込状態の区別 (TAG-UX-C)', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// 前提データ: シミュレーションモードの PLC接続 + 収集グループ +
		// タグ1件（再試行成功後に一覧へ復帰することを確認するための1件、
		// 実 PLC/実ネットワークへは繋がない）。
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
			data: {
				name: GROUP_NAME,
				plcConnectionId: connection.id,
				periodMs: 1000,
				enabled: true
			}
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };

		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				// modbus-tcp 接続配下なので Modbus 参照番号形式が必要
				// （他 tags spec と同じ理由）。
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. 初回 GET /api/tags 失敗 → エラー文言と再試行ボタンが見える（「タグがありません」と誤認しない）', async () => {
		// GET /api/tags のみ落とす - POST（他テストでの作成用途）や
		// groups/connections への他エンドポイントには触れない。
		await page.route('**/api/tags', async (route) => {
			if (route.request().method() === 'GET') {
				await route.abort();
				return;
			}
			await route.continue();
		});

		await page.goto('/tags');

		// `$lib/banto/tagRegistryAdmin.ts` の httpRequest はネットワーク層の
		// 例外を ProviderError({ kind: 'other', message: NETWORK_ERROR_MESSAGE })
		// に変換する - その文言がそのまま画面内バナーに出る。
		const inlineError = page.locator('.right-pane .err');
		await expect(inlineError).toHaveText('サーバーに接続できません');
		await expect(page.getByRole('button', { name: '再試行' })).toBeVisible();

		// 「タグがありません。」（真の空）でも空の BantoGrid でもないこと -
		// 通信失敗を0件と誤認しないことの直接確認。
		await expect(page.getByText('タグがありません。', { exact: true })).toHaveCount(0);
		await expect(page.getByRole('grid')).toHaveCount(0);
	});

	test('2. 再試行で復旧 → GET が成功するようになれば一覧が表示される', async () => {
		// route を成功側に切り替える（unroute してデフォルトの実通信に戻す）。
		await page.unroute('**/api/tags');

		await page.getByRole('button', { name: '再試行' }).click();

		// 復旧後は stale 用のバナーも初回失敗用のエラーも残らず、前提データで
		// 作成した1件が一覧に表示される。
		await expect(page.locator('.right-pane .err')).toHaveCount(0);
		await expect(page.getByRole('grid')).toBeVisible();
		await expect(page.getByRole('gridcell', { name: TAG_NAME, exact: true })).toBeVisible();
	});

	test('3. 検索0件 → 「条件に一致するタグがありません」（真の空とは違う文言）が出る', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('gridcell', { name: TAG_NAME, exact: true })).toBeVisible();

		await page.getByPlaceholder('名前・アドレスで検索').fill('該当しないはずの検索語xyz');

		await expect(page.getByText('条件に一致するタグがありません。')).toBeVisible();
		await expect(page.getByText('タグがありません。', { exact: true })).toHaveCount(0);
		await expect(page.getByRole('grid')).toHaveCount(0);
	});
});
