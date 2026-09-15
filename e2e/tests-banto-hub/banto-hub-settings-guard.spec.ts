/**
 * PR #371 の Copilot レビュー指摘（#359 banto-hub 分）の実 DOM 固定。
 * `chromium-locked-down` プロジェクト（`banto-hub.playwright.config.ts`）
 * 専用 - ロックダウン済みサーバーでしか検証できない/検証しやすい2点をまとめる:
 *
 * 1. **`guardCategory` の redirect**（`categories.ts`）: 非可視カテゴリへの
 *    直接 URL アクセスは先頭の可視カテゴリへ 307 redirect される。この
 *    経路は「非可視カテゴリが実際に存在する」状態でないと固定できない -
 *    メイン E2E サーバー（`banto-hub-viewport-settings-routes.spec.ts` が
 *    使う既定サーバー）はロックダウンしないため `sessionStore.
 *    commissioningMode` が常に true で `security` カテゴリが常に可視になり、
 *    検証できない（同 spec の doc comment 参照）。ロックダウン済みサーバー
 *    なら `session.svelte.ts::load()` が `commissioningMode = false` を
 *    設定するので `security` が非可視になり、redirect を再現できる。
 *
 * 2. **`/settings/data` 直接遷移時の構成パッケージ import ガード回帰**
 *    （`hubStatusStore.svelte.ts` の doc comment参照）: 収集状態の5秒
 *    ポーリングを `settings/+layout.svelte` へ引き上げる前は、
 *    `ConnectivitySection`（`/settings/connectivity`）を経由しないと
 *    `hubStatusStore.collectionState` が埋まらず、`/settings/data` へ
 *    直接来て収集中に構成パッケージを import しようとしてもガードが
 *    効かなかった。ここでは `/settings/connectivity` へは一度も遷移せず、
 *    収集を開始してから `/settings/data` へ直接遷移し、import 実行ボタンが
 *    無効化され警告が出ることを固定する。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const IMPORT_BLOCKED_MESSAGE = '構成パッケージの取り込みは収集を停止してから実行してください';

function emptyConfigPackageJson(): string {
	return JSON.stringify({
		schemaVersion: 1,
		product: 'banto-hub',
		exportedAt: new Date().toISOString(),
		excludedSecrets: [],
		plcConnections: [],
		collectionGroups: [],
		tags: [],
		sinkGroups: [],
		mqtt: {
			enabled: false,
			host: '',
			port: 1883,
			clientId: 'banto-hub',
			prefix: 'banto',
			qos: 1,
			minIntervalMs: 1000
		},
		grpc: { enabled: false, bind: '127.0.0.1', port: 50051 }
	});
}

test.describe.serial('banto-hub 設定ルートのガード（ロックダウン済み専用サーバー）', () => {
	let page: Page;
	let token = '';

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		// `banto-hub-status-pending-apply-cancel.spec.ts` と同じロックダウン
		// 手順。lock_down() は冪等なので、同じロックダウン済みサーバーで
		// あちらの spec が先に走っていても失敗しない。
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
			'この spec はロックダウン済み専用サーバーで走る前提（#341 と同じ）'
		).toBe(true);

		// ロックダウン後は `shouldBypassLoginForCommissioning` が false になり
		// 通常ログインが必須になる（`(app)/+layout.ts` 参照）。以降の
		// ナビゲーションで /login へ弾かれないよう、ロックダウン後の状態で
		// 改めてトークンを取得し直す。
		token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. ロックダウン後は非可視カテゴリ（security）への直接遷移が先頭の可視カテゴリへ redirect される', async () => {
		await page.goto('/settings/security');
		await expect(page).toHaveURL(/\/settings\/appearance$/);
		await expect(page.getByRole('heading', { level: 2, name: 'テーマ' })).toBeVisible();

		// ナビにも security が出ていないこと（guardCategory と +layout.ts の
		// 可視カテゴリ計算が一致していることの確認）。
		await expect(page.getByRole('link', { name: 'セキュリティ' })).toHaveCount(0);
	});

	test('2. /settings/data へ直接来ても収集中は構成パッケージの import が無効化される（回帰固定）', async () => {
		const authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		const startRes = await page.request.post('/api/collection/start-all-simulation', {
			headers: authedHeaders
		});
		expect(startRes.ok()).toBe(true);

		try {
			// `/settings/connectivity` には一度も遷移しない - 段階1の回帰は
			// まさに「connectivity を経由しないと collectionState が埋まらない」
			// ことだったため、ここを経由しては再現できない。
			await page.goto('/settings/data');
			await expect(page.getByRole('heading', { level: 2, name: 'データ保持' })).toBeVisible();

			await page.locator('input[type="file"]').setInputFiles({
				name: 'config-package.json',
				mimeType: 'application/json',
				buffer: Buffer.from(emptyConfigPackageJson())
			});

			const importButton = page.getByRole('button', { name: 'インポートを実行' });
			await expect(importButton).toBeVisible();

			// `settings/+layout.svelte` の admin 限定ポーリングが
			// `hubStatusStore.collectionState` を埋め、`importGuardActive` が
			// true になって初めてこの警告とボタン無効化が現れる。
			await expect(page.getByText(IMPORT_BLOCKED_MESSAGE)).toBeVisible();
			await expect(importButton).toBeDisabled();
		} finally {
			const stopRes = await page.request.post('/api/collection/stop', { headers: authedHeaders });
			expect(stopRes.ok()).toBe(true);
		}
	});
});
