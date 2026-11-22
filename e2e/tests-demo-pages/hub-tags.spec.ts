/**
 * Hub の `/tags` のデモ（docs/demo-pages-design.md §3.3・§6 の 3）。組み立てた成果物で、
 * 初期のタグが見える・値が出て変わる（「再接続中…」が出ない）・タグを 1 本追加・編集・削除して
 * 一覧に反映される。その間、本物の `/api`・WebSocket・ほかのオリジンへの送信が 0 件で、
 * 模擬に口の無い `/api` も 0 件（画面の操作で警告が出ない）。
 */
import { expect, test, type Page } from '@playwright/test';
import { BANNER, recordTraffic, SITE } from './helpers';

test.use({ viewport: { width: 1280, height: 720 } });

/** 削除の取り消しの猶予（`deferredDeleteCore.ts` の `UNDO_WINDOW_MS`）。 */
const UNDO_WINDOW_MS = 6000;
const TAGS_URL = `${SITE}/hub/tags`;

/** ツリーのグループ行（アクセシブル名は「名前 (件数) 周期」）。 */
function groupNode(page: Page, name: string) {
	return page.getByRole('tree').getByRole('treeitem', { name: new RegExp(`^${name} \\(`) });
}

/** 外部名で引いた行（アドレス・単位など、ほかの列も含む）。 */
function row(page: Page, name: string) {
	return page.getByRole('row').filter({ has: page.getByRole('gridcell', { name, exact: true }) });
}

/** 行の「現在値」の列のセルだけ（列見出しの位置で引く。アドレスの数字に当たらないように）。 */
async function valueCellText(page: Page, name: string): Promise<string> {
	const headers = await page.getByRole('columnheader').allInnerTexts();
	const index = headers.findIndex((h) => h.includes('現在値'));
	expect(index, '「現在値」の列見出し').toBeGreaterThanOrEqual(0);
	return row(page, name).getByRole('gridcell').nth(index).innerText();
}

test('Hub /tags: 初期のタグが見え、値が出て変わり、「再接続中…」が出ない', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(TAGS_URL);
	await expect(page.getByText(BANNER)).toBeVisible();

	// 初期のデータ: 接続 2 本・グループ 2 つ（ツリー）、タグ（press・pump）。
	await expect(groupNode(page, 'press')).toBeVisible();
	await expect(groupNode(page, 'pump')).toBeVisible();
	await expect(page.getByText('再接続中…')).toHaveCount(0);

	await groupNode(page, 'press').click();
	await expect(page.getByRole('gridcell', { name: 'press_pressure', exact: true })).toBeVisible();
	await expect(page.getByRole('gridcell', { name: 'press_running', exact: true })).toBeVisible();
	await expect(page.getByRole('gridcell', { name: 'press_mode', exact: true })).toBeVisible();

	// 値: 数値（f32・小数 2 桁）が出て、しばらくすると変わる。
	await expect.poll(() => valueCellText(page, 'press_pressure')).toMatch(/^\s*\d+\.\d\d/);
	const first = await valueCellText(page, 'press_pressure');
	await expect
		.poll(() => valueCellText(page, 'press_pressure'), { timeout: 30_000 })
		.not.toBe(first);

	await expect(page.getByText('再接続中…')).toHaveCount(0);
	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});

test('Hub /tags: 下のログのパネルの初回の取得が済み、「読み込めません」も警告も出ない', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	// 取得は、パネルが画面に出てから（`setVisible`）始まる。デモの模擬は `window.fetch` の差し替えで
	// ネットワークを通らないので、`waitForResponse` は使えない。模擬の fetch を包んで完了を数える。
	// 模擬が差し替える前の getter は native の fetch をそのまま返す（模擬が `originalFetch` として
	// 控えるのを native にし、静的な資産の取得が計測を経て模擬へ戻る再帰を作らない。#1316 のレビュー）。
	await page.addInitScript(() => {
		const w = window as unknown as { __logFeedDone: number };
		w.__logFeedDone = 0;
		const native = window.fetch;
		let current: typeof fetch = native;
		Object.defineProperty(window, 'fetch', {
			configurable: true,
			get: () => current,
			set: (next: typeof fetch) => {
				current = async (input, init) => {
					const url =
						typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
					const response = await next(input, init);
					if (/\/api\/log-feed(\?|$)/.test(url)) w.__logFeedDone += 1;
					return response;
				};
			}
		});
	});
	await page.goto(TAGS_URL);
	await expect(page.getByText(BANNER)).toBeVisible();
	// 既定の配置では「ログ」は下に畳まれていて、見えるまで取得は始まらない（これが、前の E2E が
	// 拾えなかった本当の理由）。見出しを押して出す。
	await page.getByText('ログ', { exact: true }).last().click();
	await page.waitForFunction(
		() => (window as unknown as { __logFeedDone: number }).__logFeedDone >= 1
	);

	// 正常のときは状態の文言（`log-status`）自体が出ない。
	await expect(page.getByTestId('log-status')).toHaveCount(0);
	await expect(page.getByText('表示する記録はありません。')).toBeVisible();
	await expect(page.getByText('読み込めません')).toHaveCount(0);
	// 計測を挟んでも、同じオリジン・ベースパスの静的な資産は模擬を素通りして取れる（再帰しない）。
	const asset = await page.evaluate(async () => {
		const src = document.querySelector<HTMLLinkElement>('link[rel="modulepreload"]')?.href;
		if (!src) return { src: null, status: 0 };
		const response = await window.fetch(src);
		return { src, status: response.status };
	});
	expect(asset.src).not.toBeNull();
	expect(asset.status).toBe(200);
	expect(traffic.unsupported).toEqual([]);
	expect(traffic.forbidden()).toEqual([]);
});

test('Hub /tags: タグの追加・編集・削除が一覧に反映され、本物へは送らない', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(TAGS_URL);
	await groupNode(page, 'pump').click();
	await expect(page.getByRole('gridcell', { name: 'pump_flow', exact: true })).toBeVisible();

	// 追加（保存して閉じる）。
	await page.getByRole('button', { name: 'タグを追加' }).click();
	const create = page.getByRole('complementary', { name: '新規作成' });
	await expect(create).toBeVisible();
	await create.getByLabel('名前').fill('demo_added');
	await create.getByLabel('アドレス').fill('40100');
	await create.getByLabel('データ型').selectOption({ value: 'u16' });
	await create.getByRole('button', { name: '保存して閉じる' }).click();
	await expect(page.getByText('作成しました')).toBeVisible();
	await expect(create).toBeHidden();
	// 画面の高さによっては追加した行が表示範囲（仮想化）の外なので、検索で絞ってから確かめる。
	const search = page.getByPlaceholder('外部名・名前・アドレスで検索');
	await search.fill('demo_added');
	const added = page.getByRole('gridcell', { name: 'demo_added', exact: true });
	await expect(added).toBeVisible();
	// 追加したタグにも値が出る（`config_changed` で catalog を引き直し、購読に入る）。
	// （アドレスは 40100 なので、値の列のセルだけを見る。）
	await expect.poll(() => valueCellText(page, 'demo_added')).toMatch(/\d/);

	// 編集（単位）。
	await added.click();
	const edit = page.getByRole('complementary', { name: 'demo_added を編集' });
	await expect(edit).toBeVisible();
	await edit.getByLabel('単位').fill('rpm');
	await edit.getByRole('button', { name: /^保存/ }).first().click();
	await expect(row(page, 'demo_added')).toContainText('rpm');

	// 削除（遅延実行: 確認の後、一覧からすぐ消える）。
	page.once('dialog', (dialog) => void dialog.accept());
	await edit.getByRole('button', { name: '削除' }).click();
	await expect(page.getByRole('gridcell', { name: 'demo_added', exact: true })).toHaveCount(0);

	// 取り消しの猶予（UNDO_WINDOW_MS）を過ぎて、遅延した DELETE が模擬に届いた後も消えたまま
	// （届いた DELETE が模擬の口で失敗すれば、一覧が戻るか失敗の表示が出る）。
	await page.waitForTimeout(UNDO_WINDOW_MS + 1500);
	await expect(page.getByRole('gridcell', { name: 'demo_added', exact: true })).toHaveCount(0);
	await expect(page.getByText('削除できません')).toHaveCount(0);

	// 初期のタグ（pump_flow）の単位も編集して残す。
	await search.fill('pump_flow');
	await expect(row(page, 'pump_flow')).toContainText('L/min');
	await page.getByRole('gridcell', { name: 'pump_flow', exact: true }).click();
	const editFlow = page.getByRole('complementary', { name: 'pump_flow を編集' });
	await expect(editFlow).toBeVisible();
	await editFlow.getByLabel('単位').fill('rpm');
	await editFlow.getByRole('button', { name: /^保存/ }).first().click();
	await expect(row(page, 'pump_flow')).toContainText('rpm');

	// 読み直すと、その編集は消えて初期のデータ（L/min）に戻る（保存しない）。
	await page.reload();
	await groupNode(page, 'pump').click();
	await expect(page.getByPlaceholder('外部名・名前・アドレスで検索')).toHaveValue('');
	await expect(page.getByRole('gridcell', { name: 'pump_flow', exact: true })).toBeVisible();
	await expect(row(page, 'pump_flow')).toContainText('L/min');
	await expect(row(page, 'pump_flow')).not.toContainText('rpm');
	await expect(page.getByRole('gridcell', { name: 'demo_added', exact: true })).toHaveCount(0);

	await page.waitForTimeout(1000);
	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});

test('Hub /tags: 連続登録（アドレスの計算 `offset-addresses` の模擬）で数本のタグが作れる', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(TAGS_URL);
	await groupNode(page, 'pump').click();
	await page.getByRole('button', { name: '連続登録' }).click();
	const pane = page.getByRole('complementary', { name: '連続登録' });
	await expect(pane).toBeVisible();
	await page.getByLabel('開始アドレス').fill('40200');
	await page.getByLabel('名前パターン').fill('demo_cont{n}');
	await page.getByLabel('開始番号').fill('1');
	await page.getByLabel('点数').fill('3');
	await expect(page.getByRole('heading', { level: 4, name: 'プレビュー（3件）' })).toBeVisible();
	const previewRows = page.locator('.preview-table tbody tr');
	await expect(previewRows).toHaveCount(3);
	await expect(previewRows.nth(2)).toContainText('demo_cont3');
	await expect(previewRows.nth(2)).toContainText('40202');
	await pane.getByRole('button', { name: '検証', exact: true }).click();
	await pane.getByRole('button', { name: '登録', exact: true }).click();
	// 画面の高さによっては追加した行が表示範囲（仮想化）の外なので、検索で絞ってから確かめる。
	await page.getByPlaceholder('外部名・名前・アドレスで検索').fill('demo_cont');
	for (const name of ['demo_cont1', 'demo_cont2', 'demo_cont3']) {
		await expect(page.getByRole('gridcell', { name, exact: true })).toBeVisible();
	}
	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});
