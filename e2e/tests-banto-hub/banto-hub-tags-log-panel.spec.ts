/**
 * #1253: 下のログのパネル（`/monitor`・`/tags` のドックの枠の追加パネル `log`）の実 DOM の
 * 受け入れテスト（docs/hub/banto-hub-ui-improvement-proposal.md §下のログのパネル）。
 *
 * 1. 両ページの既定の配置にパネルがあり、管理者には操作の監査（このテストのログインなど）の
 *    行が出る。種類のトグルで絞れる。
 * 2. 口（`GET /api/log-feed`）が 409（`cursor_expired`）を返したら、表示中の行を捨てて
 *    `cursor` 無しの初回から読み直す（`page.route` で 1 回だけ 409 を返させ、初回の応答に
 *    足した作り物の行が消えることで見る）。
 * 3. パネルを隠すと読みに行かない。見せ直すと続き（`cursor` 付き）から読む。
 *
 * 管理者でない利用者に操作・書き込みの行が出ないことは、ロックダウン済みの専用サーバーの
 * `banto-hub-user-log-panel-viewer.spec.ts`（試運転中は閲覧者を作っても権限の差が出ない）。
 * 最後は既定の配置へ戻す（後の spec のため）。ファイル名は smoke より後（`t` > `s`）。
 */
import { expect, test, type Page, type Request } from '@playwright/test';
import { ensureLoggedIn } from './banto-hub-auth';

const WIDE = { width: 1280, height: 800 };
const MONITOR_SETTING = '/api/ui-settings/hub.dock.v2.monitor';
const FEED_PATH = '/api/log-feed';
const FAKE_KEY = 'a:999999999';
/** 読む間隔（5 秒）を 1 回またぐ余裕。 */
const ONE_INTERVAL_MS = 12_000;

function feedCursor(req: Request): string | null | undefined {
	const url = new URL(req.url());
	if (url.pathname !== FEED_PATH) return undefined;
	return url.searchParams.get('cursor');
}

test.describe.serial('banto-hub 下のログのパネル (#1253)', () => {
	let page: Page;

	const panel = () => page.getByTestId('log-panel');
	const rows = (source?: string) =>
		source
			? panel().locator(`[data-testid="log-row"][data-source="${source}"]`)
			: panel().getByTestId('log-row');

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		await page.goto('/login');
		await ensureLoggedIn(page);
	});

	test.afterAll(async () => {
		await page.close();
	});

	test('1. タグモニタの既定の配置にパネルがあり、管理者には操作の監査の行が出て、種類で絞れる', async () => {
		await page.goto('/monitor');
		await expect(panel()).toBeVisible();
		await expect(rows('operation').first()).toBeVisible({ timeout: ONE_INTERVAL_MS });

		const filter = panel().getByTestId('log-filter-operation');
		await expect(filter).toHaveAttribute('aria-pressed', 'true');
		await filter.click();
		await expect(filter).toHaveAttribute('aria-pressed', 'false');
		await expect(rows('operation')).toHaveCount(0);
		await filter.click();
		await expect(rows('operation').first()).toBeVisible();
	});

	test('2. タグ登録の既定の配置にもパネルがあり、行が出る', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		await expect(panel()).toBeVisible();
		await expect(rows('operation').first()).toBeVisible({ timeout: ONE_INTERVAL_MS });
	});

	test('3. 409（cursor_expired）では表示中の行を捨て、初回から読み直す', async () => {
		test.setTimeout(90_000);
		const cursors: (string | null)[] = [];
		let phase: 'first' | 'expire' | 'pass' = 'first';
		await page.route(`**${FEED_PATH}**`, async (route) => {
			const cursor = feedCursor(route.request());
			if (cursor === undefined) return route.continue();
			cursors.push(cursor);
			if (phase === 'first' && cursor === null) {
				phase = 'expire';
				const response = await route.fetch();
				const body = (await response.json()) as { entries: unknown[] };
				body.entries.unshift({
					source: 'operation',
					id: 999_999_999,
					key: FAKE_KEY,
					ts_ms: Date.now() + 60_000,
					origin: 'rest',
					actor_username: 'e2e-fake',
					actor_role: 'admin',
					action: 'e2e_fake',
					resource: 'e2e-fake-row',
					entity_id: null,
					result: 'ok',
					detail: null
				});
				return route.fulfill({ response, json: body });
			}
			if (phase === 'expire' && cursor !== null) {
				phase = 'pass';
				return route.fulfill({
					status: 409,
					json: {
						kind: 'conflict',
						message: '続きの位置が使えません',
						current: { code: 'cursor_expired' }
					}
				});
			}
			return route.continue();
		});
		try {
			await page.goto('/monitor');
			const fake = panel().locator(`[data-key="${FAKE_KEY}"]`);
			await expect(fake).toBeVisible({ timeout: ONE_INTERVAL_MS });
			await expect(fake).toHaveCount(0, { timeout: ONE_INTERVAL_MS * 2 });
			await expect.poll(() => phase).toBe('pass');
			// 初回（null）→ 続き（cursor 付き、409）→ 初回から（null）。
			await expect.poll(() => cursors.length, { timeout: ONE_INTERVAL_MS }).toBeGreaterThan(2);
			expect(cursors[0]).toBeNull();
			expect(cursors[1]).not.toBeNull();
			expect(cursors[2]).toBeNull();
			// 読み直した後も本物の行は出る。
			await expect(rows('operation').first()).toBeVisible({ timeout: ONE_INTERVAL_MS });
		} finally {
			await page.unroute(`**${FEED_PATH}**`);
		}
	});

	test('4. パネルを隠すと読みに行かず、見せ直すと続きから読む。最後は既定の配置へ戻す', async () => {
		test.setTimeout(90_000);
		const loaded = page.waitForResponse(
			(r) => r.url().includes(MONITOR_SETTING) && r.request().method() === 'GET'
		);
		await page.goto('/monitor');
		await loaded;
		await expect(panel()).toBeVisible();
		// 1 回は読み終えて、続きの位置を持っている。
		await expect(rows('operation').first()).toBeVisible({ timeout: ONE_INTERVAL_MS });

		const requested: (string | null)[] = [];
		const onRequest = (req: Request) => {
			const cursor = feedCursor(req);
			if (cursor !== undefined) requested.push(cursor);
		};
		const saved = () =>
			page.waitForResponse(
				(r) => r.url().includes(MONITOR_SETTING) && r.request().method() === 'PUT'
			);
		const toggleLog = async () => {
			await page.getByTestId('monitor-view-menu').click();
			await page.getByRole('menuitemcheckbox', { name: 'ログ' }).click();
			await page.keyboard.press('Escape');
		};

		const hidden = saved();
		await toggleLog();
		await expect(panel()).toBeHidden();
		expect((await hidden).ok()).toBe(true);
		page.on('request', onRequest);
		try {
			await page.waitForTimeout(ONE_INTERVAL_MS);
			expect(requested).toEqual([]);

			const shown = saved();
			await toggleLog();
			await expect(panel()).toBeVisible();
			expect((await shown).ok()).toBe(true);
			await expect.poll(() => requested.length, { timeout: ONE_INTERVAL_MS }).toBeGreaterThan(0);
			expect(requested[0]).not.toBeNull();
		} finally {
			page.off('request', onRequest);
		}

		// 既定に戻す（見せ直した配置が既定と同じなら保存は起きない。後の spec のため）。
		await page.getByTestId('monitor-view-menu').click();
		await page.getByRole('menuitem', { name: 'レイアウトを既定に戻す' }).click();
		await expect(panel()).toBeVisible();
		await expect(page.getByTestId('monitor-tree-toggle')).toHaveAttribute('aria-pressed', 'true');
	});
});
