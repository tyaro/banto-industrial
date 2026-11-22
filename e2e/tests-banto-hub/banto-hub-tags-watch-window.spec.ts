/**
 * #1254: 値を見張る窓（`/tags` のドックの枠の追加パネル `watch`）の実 DOM の受け入れテスト
 * （docs/hub/banto-hub-ui-improvement-proposal.md §値を見張る窓）。
 *
 * 1. 属性に出している行を「見張りに足す」で足すと窓が開き、完全な外部名の行が出る。
 * 2. 再読み込みの後も一覧が残る（利用者ごとにサーバーへ保存。`hub.watch.v1`）。
 * 3. 行の右クリックのメニュー「見張りに足す」で足せる。同じタグを重ねて足さない。
 * 4. 行ごとの「外す」・「すべて外す」で外せ、保存も空になる。
 * 5. 保存値の初回の GET が失敗しても窓には何も出さず、足した見張りは「保存中」になり、
 *    読み直しで確定して保存される（再読み込みの後も残る）。PR #1282 の再レビュー。
 * 6. GET は通り、最初の PUT だけ失敗したら「保存できていません」と「今すぐ再試行」が出て、
 *    押すと保存される（再読み込みの後も残る）。PR #1282 の再レビュー P2-1。
 * 7. 見張りはタグ ID で購読し、値も ID で引く（`page.routeWebSocket` で `/api/v1/stream` を
 *    差し替えて値を注入する。`banto-hub-tags-values.spec.ts` と同じ流儀）。同じ外部名の別 ID の
 *    値は行に出ない。
 *
 * 最初に保存した見張りの一覧と配置を消し、最後も消す（後の spec のため）。
 */
import { expect, test, type Page, type WebSocketRoute } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-watch-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-watch-group-${RUN_ID}`;
const TAG_A = `e2e-watch-a-${RUN_ID}`;
const TAG_B = `e2e-watch-b-${RUN_ID}`;
const NAME_A = `${CONNECTION_NAME}.${GROUP_NAME}.${TAG_A}`;
const NAME_B = `${CONNECTION_NAME}.${GROUP_NAME}.${TAG_B}`;
const WATCH_SETTING = '/api/ui-settings/hub.watch.v1';
const TAGS_LAYOUT_SETTING = '/api/ui-settings/hub.dock.v2.tags';
const WIDE = { width: 1920, height: 1080 };

test.describe.serial('banto-hub 値を見張る窓 (#1254)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let connectionId: number;
	let groupId: number;
	const tagIds: number[] = [];

	const panel = () => page.getByTestId('watch-panel');
	const watchRows = () => panel().getByTestId('watch-row');

	async function clearSaved(): Promise<void> {
		for (const key of [WATCH_SETTING, TAGS_LAYOUT_SETTING]) {
			const res = await page.request.put(key, { headers: authedHeaders, data: { value: '' } });
			expect(res.ok(), await res.text()).toBe(true);
		}
		await page.evaluate(() => {
			for (const key of Object.keys(localStorage)) {
				if (key.startsWith('banto.dock.v2.') || key.startsWith('banto.watch.v')) {
					localStorage.removeItem(key);
				}
			}
		});
	}

	async function savedWatchIds(): Promise<number[]> {
		const res = await page.request.get(WATCH_SETTING, { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		const body = (await res.json()) as { value?: string | null };
		if (!body.value) return [];
		return (JSON.parse(body.value) as { tags: number[] }).tags;
	}

	async function openWatchFromViewMenu(): Promise<void> {
		await page.getByTestId('tag-view-menu').click();
		await page.getByRole('menuitemcheckbox', { name: '見張り' }).click();
		await expect(panel()).toBeVisible();
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage({ viewport: WIDE });
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await clearSaved();

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connectionRes.ok()).toBe(true);
		connectionId = ((await connectionRes.json()) as { id: number }).id;
		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connectionId, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;
		for (const [name, address] of [
			[TAG_A, '40001'],
			[TAG_B, '40002']
		]) {
			const tagRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: {
					name,
					collectionGroupId: groupId,
					address,
					dataType: 'i16',
					unit: null,
					decimals: 0,
					enabled: true,
					writable: false,
					tagKind: 'plc'
				}
			});
			expect(tagRes.ok()).toBe(true);
			tagIds.push(((await tagRes.json()) as { id: number }).id);
		}
	});

	test.afterAll(async () => {
		await page.unrouteAll({ behavior: 'ignoreErrors' });
		await clearSaved();
		for (const id of tagIds)
			await page.request.delete(`/api/tags/${id}`, { headers: authedHeaders });
		await page.request.delete(`/api/collection-groups/${groupId}`, { headers: authedHeaders });
		await page.request.delete(`/api/plc-connections/${connectionId}`, { headers: authedHeaders });
		await page.close();
	});

	test('1. 属性に出している行を足すと窓が開き、完全な外部名の行が出る', async () => {
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(`e2e-watch-`);
		// 対象が無い間はボタンが無効。
		await expect(page.getByTestId('tag-watch-add')).toBeDisabled();
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		await expect(page.getByTestId('tag-watch-add')).toBeEnabled();
		await page.getByTestId('tag-watch-add').click();
		await expect(panel()).toBeVisible();
		await expect(watchRows()).toHaveCount(1);
		await expect(watchRows().first()).toContainText(NAME_A);
	});

	test('2. 再読み込みの後も一覧が残る', async () => {
		await expect.poll(savedWatchIds).toEqual([tagIds[0]]);
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		if (!(await panel().isVisible())) await openWatchFromViewMenu();
		await expect(watchRows()).toHaveCount(1);
		await expect(watchRows().first()).toContainText(NAME_A);
	});

	test('3. 行の右クリックのメニューで足せる。同じタグは重ねない', async () => {
		await page.getByPlaceholder('名前・アドレスで検索').fill(`e2e-watch-`);
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '見張りに足す' }).click();
		await expect(watchRows()).toHaveCount(2);
		await expect(panel()).toContainText(NAME_B);
		await expect.poll(savedWatchIds).toEqual([tagIds[0], tagIds[1]]);

		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click({ button: 'right' });
		await page.getByRole('menuitem', { name: '見張りに足す' }).click();
		await expect(watchRows()).toHaveCount(2);
	});

	test('4. 行ごとの「外す」と「すべて外す」で外せ、保存も空になる', async () => {
		await watchRows().filter({ hasText: NAME_A }).getByTestId('watch-remove').click();
		await expect(watchRows()).toHaveCount(1);
		await expect(panel()).not.toContainText(NAME_A);
		await expect.poll(savedWatchIds).toEqual([tagIds[1]]);

		await panel().getByTestId('watch-clear').click();
		await expect(watchRows()).toHaveCount(0);
		await expect(panel()).toContainText('見張っているタグはありません');
		await expect.poll(savedWatchIds).toEqual([]);
	});

	test('5. 保存値の初回の GET が失敗しても、足した見張りは「保存中」を出し、読み直して保存する', async () => {
		// 初回の GET だけ失敗させ、2 回目以降の GET は `releaseGet()` まで返さない（保存中を
		// 確かめる間、周回が終わらないように）。PUT はそのまま通す。
		let gets = 0;
		let releaseGet: () => void = () => undefined;
		const released = new Promise<void>((resolve) => {
			releaseGet = resolve;
		});
		await page.route('**/api/ui-settings/hub.watch.v1', async (route) => {
			if (route.request().method() !== 'GET') return route.continue();
			gets += 1;
			if (gets === 1) return route.fulfill({ status: 500, body: 'injected failure' });
			await released;
			return route.continue();
		});
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		await expect.poll(() => gets).toBeGreaterThanOrEqual(1);

		await page.getByPlaceholder('名前・アドレスで検索').fill(`e2e-watch-`);
		await page.getByRole('gridcell', { name: TAG_A, exact: true }).click();
		await page.getByTestId('tag-watch-add').click();
		await expect(panel()).toBeVisible();
		await expect(watchRows()).toHaveCount(1);
		await expect(panel().getByTestId('watch-save-status')).toContainText('保存中');
		// 操作で再試行の待ちが打ち切られた（待っている 2 回目の GET）。まだ保存しない。
		await expect.poll(() => gets).toBeGreaterThanOrEqual(2);
		expect(await savedWatchIds()).toEqual([]);

		releaseGet();
		await expect(panel().getByTestId('watch-save-status')).toHaveCount(0);
		await expect.poll(savedWatchIds).toEqual([tagIds[0]]);

		await page.unroute('**/api/ui-settings/hub.watch.v1');
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		if (!(await panel().isVisible())) await openWatchFromViewMenu();
		await expect(watchRows()).toHaveCount(1);
		await expect(watchRows().first()).toContainText(NAME_A);
	});

	test('6. 最初の PUT だけ失敗したら「保存できていません」を出し、再試行で保存する', async () => {
		// 確かめ終わるまで PUT を失敗させる（口の自動の再送（2 秒後）が先に通らないように）。
		let puts = 0;
		let failPut = true;
		await page.route('**/api/ui-settings/hub.watch.v1', async (route) => {
			if (route.request().method() !== 'PUT') return route.continue();
			puts += 1;
			if (failPut) return route.fulfill({ status: 500, body: 'injected failure' });
			return route.continue();
		});
		await page.getByPlaceholder('名前・アドレスで検索').fill(`e2e-watch-`);
		await page.getByRole('gridcell', { name: TAG_B, exact: true }).click();
		await page.getByTestId('tag-watch-add').click();
		await expect(watchRows()).toHaveCount(2);
		await expect(panel().getByTestId('watch-save-status')).toContainText('保存できていません');
		const retry = panel().getByTestId('watch-save-retry');
		await expect(retry).toHaveText('今すぐ再試行');
		expect(await savedWatchIds()).toEqual([tagIds[0]]);

		failPut = false;
		// 押す前に自動の再送が通っていれば、ボタンはもう無い（どちらでも保存される）。
		await retry.click({ timeout: 2_000 }).catch(() => undefined);
		await expect(panel().getByTestId('watch-save-status')).toHaveCount(0);
		await expect.poll(savedWatchIds).toEqual([tagIds[0], tagIds[1]]);
		expect(puts).toBeGreaterThanOrEqual(2);

		await page.unroute('**/api/ui-settings/hub.watch.v1');
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		if (!(await panel().isVisible())) await openWatchFromViewMenu();
		await expect(watchRows()).toHaveCount(2);
		await expect(panel()).toContainText(NAME_B);

		// 次の試験のために B を外す（ふつうに保存される）。
		await watchRows().filter({ hasText: NAME_B }).getByTestId('watch-remove').click();
		await expect(watchRows()).toHaveCount(1);
		await expect.poll(savedWatchIds).toEqual([tagIds[0]]);
	});

	test('7. 見張りはタグ ID で購読し、値を ID で引く（同じ外部名の別 ID の値は出ない）', async () => {
		type Subscribe = { op: string; id: number; tags?: string[]; ids?: number[] };
		let watchSocket: WebSocketRoute | null = null;
		let watchSubscribe: Subscribe | null = null;
		await page.routeWebSocket(/\/api\/v1\/stream$/, (ws) => {
			ws.onMessage((message) => {
				if (typeof message !== 'string') return;
				const parsed = JSON.parse(message) as Subscribe;
				if (parsed.op === 'subscribe' && parsed.ids !== undefined) {
					watchSocket = ws;
					watchSubscribe = parsed;
				}
			});
		});
		await page.reload();
		await expect(page.getByRole('heading', { level: 2, name: 'タグ登録' })).toBeVisible();
		if (!(await panel().isVisible())) await openWatchFromViewMenu();
		await expect(watchRows()).toHaveCount(1);

		await expect.poll(() => watchSubscribe?.ids).toEqual([tagIds[0]]);
		// パターンは送らない（ドット入りの接続名・グループ名でも購読できる）。
		expect(watchSubscribe!.tags).toBeUndefined();

		const now = Date.now();
		const send = (values: unknown[]) =>
			watchSocket!.send(JSON.stringify({ op: 'data', id: watchSubscribe!.id, t: now, values }));
		// スナップショット: 見張っている ID の値（外部名は改名前のものでもよい）と、同じ外部名で
		// 作り直された別 ID の値（サーバーは選ばないが、来ても行に出してはいけない）。
		send([
			{ tag: `${NAME_A}-old-name`, id: tagIds[0], v: 42, q: 'good', t: now },
			{ tag: NAME_A, id: 999_999_999, v: 7, q: 'good', t: now }
		]);
		const value = watchRows().first().getByTestId('watch-value');
		await expect(value).toContainText('42');
		await expect(watchRows().first().getByTestId('watch-quality')).toHaveText('良好');
		// 差分も ID で引く。
		send([{ tag: NAME_A, id: 999_999_999, v: 99, q: 'good', t: now + 1 }]);
		send([{ tag: NAME_A, id: tagIds[0], v: 43, q: 'good', t: now + 2 }]);
		await expect(value).toContainText('43');
		// 名前の列はページのタグ一覧の名前。
		await expect(watchRows().first()).toContainText(NAME_A);
	});
});
