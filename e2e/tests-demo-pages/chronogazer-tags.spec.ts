/**
 * ChronoGazer の `/tags` のデモ（docs/demo-pages-design.md §3.3・§6 の 4b）。組み立てた成果物で、
 * 初期のタグが見える・タグを 1 本追加・編集・削除して一覧に反映される・読み直すと初期に戻る。
 * その間、本物の `/api`・WebSocket・ほかのオリジンへの送信が 0 件で、模擬に口の無い `/api` も
 * 0 件（画面の操作で警告が出ない）。
 */
import { expect, test, type Page } from '@playwright/test';
import { BANNER, recordTraffic, SITE } from './helpers';

test.use({ viewport: { width: 1280, height: 720 } });

/** 削除の取り消しの猶予（`deferredDeleteCore.ts` の `UNDO_WINDOW_MS`）に余裕を足した待ち。 */
const UNDO_WAIT_MS = 6000 + 1500;
const TAGS_URL = `${SITE}/chronogazer/tags`;

/** ツリーのグループ行（アクセシブル名は「名前 (件数) 周期」）。 */
function groupNode(page: Page, name: string) {
	return page.getByRole('tree').getByRole('treeitem', { name: new RegExp(`${name} [(]`) });
}

/** 名前で引いた行（アドレス・単位など、ほかの列も含む）。 */
function row(page: Page, name: string) {
	return page.getByRole('row').filter({ has: page.getByRole('gridcell', { name, exact: true }) });
}

test('ChronoGazer /tags: 初期のタグ・グループ・接続が見える', async ({ page, baseURL }) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(TAGS_URL);
	await expect(page.getByText(BANNER)).toBeVisible();

	await expect(groupNode(page, 'press')).toBeVisible();
	await expect(groupNode(page, 'pump')).toBeVisible();
	await expect(page.getByRole('tree').getByText('line1')).toBeVisible();
	await expect(page.getByRole('tree').getByText('line2')).toBeVisible();
	await expect(page.getByRole('gridcell', { name: 'press_pressure', exact: true })).toBeVisible();
	await expect(row(page, 'press_pressure')).toContainText('MPa');
	await expect(row(page, 'press_running')).toContainText('bit');

	await groupNode(page, 'pump').click();
	await expect(page.getByRole('gridcell', { name: 'pump_flow', exact: true })).toBeVisible();
	await expect(page.getByRole('gridcell', { name: 'press_pressure', exact: true })).toHaveCount(0);

	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});

test('ChronoGazer /tags: タグの追加・編集・削除が一覧に反映され、読み直すと初期に戻る', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(TAGS_URL);
	await groupNode(page, 'pump').click();
	await expect(page.getByRole('gridcell', { name: 'pump_flow', exact: true })).toBeVisible();

	// 追加（保存して閉じる）。
	await page.getByRole('button', { name: '記録項目を追加' }).click();
	const create = page.getByRole('complementary', { name: '記録項目を追加' });
	await expect(create).toBeVisible();
	await create.getByLabel('名前').fill('demo_added');
	await create.getByLabel('デバイスアドレス').fill('40100');
	await create.getByLabel('データ型').selectOption({ value: 'u16' });
	await create.getByRole('button', { name: '作成', exact: true }).click();
	await expect(page.getByText('作成しました')).toBeVisible();
	await expect(row(page, 'demo_added')).toContainText('40100');
	await expect(row(page, 'demo_added')).toContainText('pump');

	// 編集（単位）。
	await page.getByRole('gridcell', { name: 'demo_added', exact: true }).click();
	const edit = page.getByRole('complementary', { name: 'demo_added' });
	await expect(edit).toBeVisible();
	await edit.getByLabel('単位').fill('rpm');
	await edit.getByRole('button', { name: /^保存$/ }).click();
	await expect(row(page, 'demo_added')).toContainText('rpm');

	// 検証の誤り: 同じグループの同じ名前は断られる（誤りの文言が出る）。
	await edit.getByLabel('名前').fill('pump_flow');
	await edit.getByRole('button', { name: /^保存$/ }).click();
	await expect(page.getByText('同じグループに同じ名前のタグがあります').first()).toBeVisible();
	await edit.getByLabel('名前').fill('demo_added');

	// 削除（確認の後、遅延実行: 一覧からすぐ消える）。
	page.once('dialog', (dialog) => void dialog.accept());
	await edit.getByRole('button', { name: '削除' }).click();
	await expect(page.getByRole('gridcell', { name: 'demo_added', exact: true })).toHaveCount(0);

	// 取り消しの猶予を過ぎて、遅延した DELETE が模擬に届いた後も消えたまま。
	await page.waitForTimeout(UNDO_WAIT_MS);
	await expect(page.getByRole('gridcell', { name: 'demo_added', exact: true })).toHaveCount(0);
	await expect(page.getByText('削除できません')).toHaveCount(0);

	// 初期のタグ（pump_flow）の単位も編集して残す。
	await page.getByRole('gridcell', { name: 'pump_flow', exact: true }).click();
	const editFlow = page.getByRole('complementary', { name: 'pump_flow' });
	await expect(editFlow).toBeVisible();
	await editFlow.getByLabel('単位').fill('rpm');
	await editFlow.getByRole('button', { name: /^保存$/ }).click();
	await expect(row(page, 'pump_flow')).toContainText('rpm');

	// 読み直すと、その編集は消えて初期のデータ（L/min）に戻る（保存しない）。
	await page.reload();
	await groupNode(page, 'pump').click();
	await expect(row(page, 'pump_flow')).toContainText('L/min');
	await expect(row(page, 'pump_flow')).not.toContainText('rpm');
	await expect(page.getByRole('gridcell', { name: 'demo_added', exact: true })).toHaveCount(0);

	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});

test('ChronoGazer /tags: 収集グループの追加と、タグが残るグループの削除の拒否', async ({
	page,
	baseURL
}) => {
	const traffic = recordTraffic(page, new URL(baseURL!).origin);
	await page.goto(TAGS_URL);

	// 追加: 接続 line1 の下に新しいグループ（周期は既定のまま）。
	await page.getByRole('button', { name: '収集グループを追加' }).first().click();
	const create = page.getByRole('complementary', { name: '収集グループを追加' });
	await expect(create).toBeVisible();
	await create.getByLabel('名前').fill('demo_group');
	await create.getByLabel('接続').selectOption({ index: 1 });
	await create.getByRole('button', { name: '作成', exact: true }).click();
	await expect(page.getByText('作成しました')).toBeVisible();
	await expect(groupNode(page, 'demo_group')).toBeVisible();

	// タグが残る既存のグループ（press）は削除を断られ、断りの文言が出る。
	await groupNode(page, 'press').click();
	await page.getByRole('tab', { name: '設定' }).click();
	page.once('dialog', (dialog) => void dialog.accept());
	await page.getByRole('button', { name: '削除' }).click();
	await expect(
		page.getByText('このグループに属するタグが10件あるため削除できません')
	).toBeVisible();
	await expect(groupNode(page, 'press')).toBeVisible();

	expect(traffic.forbidden()).toEqual([]);
	expect(traffic.unsupported).toEqual([]);
});
