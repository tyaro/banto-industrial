/**
 * `/events`（収集イベント一覧）の**「再読み込み」が正常時にも出る**ことの実
 * DOM 固定（#409 オーナーレビュー P2-4）。
 *
 * **なぜ画面をクリックして確かめるのか**: 欠陥は「表示条件」にあった
 * （`{#if view.failedBlockCount > 0}`）。ロジック側の `loader.reload()` を
 * 直接呼ぶテストでは、ボタンが**出ていない**ことを検出できない。このリポジトリ
 * には DOM/コンポーネントテストの足場が無い（jsdom・happy-dom・
 * testing-library のいずれも未導入）ので、**Playwright の E2E が「DOM
 * テスト」の等価物**。この 1 件のために足場を入れるのは、アプリ全体のテスト
 * 構成の話なのでここではやらない。
 *
 * **なぜ `page.route` を使うのか**（このスイートでは例外的）:
 * `user-settings-collect.spec.ts` の doc にあるとおり、実サーバー
 * （`banto-serve`）はレジストリが空なので収集が `noTargets` で止まり、
 * **収集イベントを実際に発生させる手段が現時点で無い**（シミュレータは C-4）。
 * このテストが見たいのは「**正常に読めた後にイベントが増えたとき、画面から
 * 新しい世代を始められるか**」なので、`GET /api/collect/events` に**世代ごとに
 * 違う応答**を返させるしかない。応答の形（`Readout` の判別共用体 +
 * `rows`/`totalCount`/`asOfId`）は `chronogazer_core::collect::CollectEventList`
 * のワイヤ形そのままで、Rust 側の形は `collect.rs`/`rest.rs` のテストが固定
 * している。
 *
 * **世代の区別**: 一覧は世代の最初の要求だけ `asOfId` を付けずに投げ、返って
 * きた境界を同じ世代の後続ブロックに渡す（#409 P2-2 の修正）。したがって
 * `asOfId` の無い要求 = 新しい世代の始まりで、スタブはそれを数えて次の世代の
 * 応答へ進む。
 *
 * ファイル名について: `smoke.spec.ts` の最初のテストが初回セットアップ（管理者
 * アカウント作成）を行うので、このファイルはそれより後の名前でなければ
 * ならない（`playwright.config.ts` は `workers: 1`/`fullyParallel: false` で
 * ファイル名の辞書順に実行する）。`user-events-reload` は `smoke`/`tags` より
 * 後、`user-settings-*` より前。
 */
import { expect, test, type Page } from '@playwright/test';

// smoke.spec.ts が初回セットアップで作成する唯一の管理者アカウント。
const ADMIN_USERNAME = 'e2e-admin';
const ADMIN_PASSWORD = 'E2eAdminPass1';

/** `chronogazer_core::collect::CollectEventRow` のワイヤ形。 */
interface StubRow {
	id: number;
	tsMs: number;
	kind: string;
	connectionKey: string | null;
	tagKey: string | null;
	level: string | null;
	value: number | null;
}

/** `Readout::Ready { data: CollectEventList }` のワイヤ形。 */
interface StubGeneration {
	rows: StubRow[];
	totalCount: number;
	asOfId: number;
}

function stubRow(id: number, connectionKey: string): StubRow {
	return {
		id,
		tsMs: 1_700_000_000_000 + id * 1000,
		kind: 'plc_connected',
		connectionKey,
		tagKey: null,
		level: null,
		value: null
	};
}

/** 世代ごとに違う応答を返すスタブ（`asOfId` の無い要求 = 新しい世代）。 */
async function stubEvents(page: Page, generations: StubGeneration[]): Promise<() => number> {
	await page.unrouteAll({ behavior: 'ignoreErrors' });
	let generation = -1;
	let calls = 0;
	await page.route('**/api/collect/events*', async (route) => {
		calls += 1;
		const url = new URL(route.request().url());
		if (url.searchParams.get('asOfId') === null) {
			generation = Math.min(generation + 1, generations.length - 1);
		}
		const data = generations[Math.max(generation, 0)];
		await route.fulfill({
			status: 200,
			contentType: 'application/json',
			body: JSON.stringify({ state: 'ready', data })
		});
	});
	return () => calls;
}

async function login(page: Page, username: string, password: string): Promise<void> {
	await page.goto('/login');
	await page.getByLabel('ユーザー名').fill(username);
	await page.getByLabel('パスワード').fill(password);
	await page.getByRole('button', { name: 'ログイン' }).click();
	await expect(page).toHaveURL(/\/monitor$/);
}

test.describe.serial('chronogazer イベント一覧の「再読み込み」（#409 P2-4）', () => {
	let page: Page;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await login(page, ADMIN_USERNAME, ADMIN_PASSWORD);
	});

	test.afterAll(async () => {
		await page.unrouteAll({ behavior: 'ignoreErrors' });
		await page.close();
	});

	test('1. 0 件で正常に読めた後も「再読み込み」が出て、押すと後から記録されたイベントが入る', async () => {
		const calls = await stubEvents(page, [
			{ rows: [], totalCount: 0, asOfId: 0 },
			{
				rows: [stubRow(2, 'conn:after-reload'), stubRow(1, 'conn:first-event')],
				totalCount: 2,
				asOfId: 2
			}
		]);

		await page.goto('/events');
		await expect(page.getByRole('heading', { level: 2, name: 'イベント' })).toBeVisible();
		// 正常に読めて 0 件（「読めなかった」ではない）。
		await expect(page.getByText('イベントはまだ1件も記録されていません。')).toBeVisible();

		// **失敗していなくてもボタンに手が届く**（空の一覧でも隠れない）。
		const reload = page.getByRole('button', { name: '再読み込み' });
		await expect(reload).toBeVisible();
		await expect(reload).toBeEnabled();
		const before = calls();

		await reload.click();

		await expect(page.getByText('2件の記録があります（新しい順）。')).toBeVisible();
		await expect(page.getByText('conn:after-reload')).toBeVisible();
		expect(calls()).toBeGreaterThan(before);
	});

	test('2. 非空の一覧を正常に読んだ後も「再読み込み」が出て、押すと最新のイベントが入る', async () => {
		const calls = await stubEvents(page, [
			{
				rows: [stubRow(3, 'conn:gen1-c'), stubRow(2, 'conn:gen1-b'), stubRow(1, 'conn:gen1-a')],
				totalCount: 3,
				asOfId: 3
			},
			{
				rows: [
					stubRow(4, 'conn:gen2-newest'),
					stubRow(3, 'conn:gen1-c'),
					stubRow(2, 'conn:gen1-b'),
					stubRow(1, 'conn:gen1-a')
				],
				totalCount: 4,
				asOfId: 4
			}
		]);

		await page.goto('/events');
		await expect(page.getByText('3件の記録があります（新しい順）。')).toBeVisible();
		await expect(page.getByText('conn:gen1-c')).toBeVisible();
		// 新しいイベントは、世代を切り直すまで入らない（境界を固定しているため）。
		await expect(page.getByText('conn:gen2-newest')).toHaveCount(0);

		const reload = page.getByRole('button', { name: '再読み込み' });
		await expect(reload).toBeVisible();
		const before = calls();

		await reload.click();

		await expect(page.getByText('4件の記録があります（新しい順）。')).toBeVisible();
		await expect(page.getByText('conn:gen2-newest')).toBeVisible();
		expect(calls()).toBeGreaterThan(before);
	});
});
