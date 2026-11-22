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

/** 値の列の文字列（数値のセルが 1 つでも出ているか）を、行の外部名で引く。 */
function row(page: Page, name: string) {
	return page.getByRole('row').filter({ has: page.getByRole('gridcell', { name, exact: true }) });
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
	const pressure = row(page, 'press_pressure');
	await expect(pressure).toContainText(/\d+\.\d\d/);
	const first = await pressure.innerText();
	await expect.poll(async () => pressure.innerText(), { timeout: 30_000 }).not.toBe(first);

	await expect(page.getByText('再接続中…')).toHaveCount(0);
	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
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
	await expect(row(page, 'demo_added')).toContainText(/\d/);

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

	// 読み直すと初期のデータに戻る（保存しない）。
	await page.reload();
	await groupNode(page, 'pump').click();
	await expect(page.getByPlaceholder('外部名・名前・アドレスで検索')).toHaveValue('');
	await expect(page.getByRole('gridcell', { name: 'pump_flow', exact: true })).toBeVisible();

	await page.waitForTimeout(1000);
	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});
