/**
 * 起動時に組み込みサーバーへ一時的に届かないとき、**demo に落ちずに起動待ちの
 * まま「サーバーに接続できません」になり、届くようになってから「再接続」すると
 * 実データ（server モード）で開く**ことの実 DOM 固定（banto v3.0.0 #286、I2c）。
 *
 * 判定の流れは単体テスト（`apps/chronogazer/src/lib/banto/startup.test.ts` と
 * `setup.test.ts`）が固定している。ここで見たいのは、**ビルドされた画面**が
 * `StartupSplash.svelte` の 2 状態を実際に出し、再接続ボタンが起動の待ちを
 * 解くこと。以前は `isEmbeddedServer()` が fetch の失敗を「サーバー無し」と
 * 読み、LAN ビルドでも demo（メモリ上の空データ、demo のログイン admin/admin）
 * に落ちて二度と戻らなかった。
 *
 * **なぜ `page.route` を使うのか**: このスイートの `webServer`（`banto-serve`）
 * は全スペックで共有しているので、止めると後続が壊れる。「サーバーに届かない」
 * は `GET /api/auth/check`（起動時の probe）を `abort` して作る。
 *
 * **なぜ `/login` を開くのか**: スプラッシュは root の `+layout.svelte` の
 * `{#await bantoReady}` が出す。`/`・`/monitor` などは `(app)/+layout.ts` の
 * `load` が `bantoReady` を待つので、SvelteKit はその間 root のレイアウトも
 * 描かず、届かない間は真っ白のまま（admin-template v3.0.0 と同じ既知の制約。
 * apps/chronogazer/README.md「起動時の判定」）。スプラッシュが出るのは保護
 * されていない画面だけなので、ここでは `/login` で確かめる。
 *
 * **server モードで開いたことの確かめ方**: 再接続の後、smoke.spec.ts が作った
 * 実アカウント（`e2e-admin`）でログインできること。demo の AuthProvider は
 * admin/admin しか受け付けないので、demo に落ちていればここで失敗する。
 *
 * ファイル名について: `smoke.spec.ts` の最初のテストが初回セットアップ（管理者
 * アカウント作成）を行うので、このファイルはそれより後の名前でなければ
 * ならない（`playwright.config.ts` は `workers: 1`/`fullyParallel: false` で
 * ファイル名の辞書順に実行する）。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

test.describe.serial('chronogazer 起動時の環境判定（banto #286）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
	});

	test.afterAll(async () => {
		await page.unrouteAll({ behavior: 'ignoreErrors' });
		await page.close();
	});

	test('1. 起動時に届かなければ demo に落ちず「接続できません」、再接続で実データの画面に入る', async () => {
		let probes = 0;
		await page.route('**/api/auth/check', async (route) => {
			probes += 1;
			await route.abort('connectionrefused');
		});

		await page.goto('/login');

		// 自動再試行（初回 + 2 回、間隔 1.5 秒）を使い切ると「接続できません」。
		const alert = page.getByRole('alert');
		await expect(alert.getByRole('heading', { name: 'サーバーに接続できません' })).toBeVisible({
			timeout: 15_000
		});
		// 起動待ちのままで、ログイン画面（demo でも server でも出る）は描かれていない。
		await expect(page.getByLabel('ユーザー名')).toHaveCount(0);
		expect(probes).toBe(3);

		// 押すまでは叩き続けない。
		await page.waitForTimeout(2_000);
		expect(probes).toBe(3);

		// サーバーに届くようにしてから「再接続」。
		await page.unroute('**/api/auth/check');
		await alert.getByRole('button', { name: '再接続' }).click();

		await page.getByLabel('ユーザー名').fill(ADMIN_USERNAME);
		await page.getByLabel('パスワード').fill(ADMIN_PASSWORD);
		await page.getByRole('button', { name: 'ログイン' }).click();
		await expect(page).toHaveURL(/\/monitor$/);
	});
});
