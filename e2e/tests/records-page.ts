/**
 * #1179 PR4b: ChronoGazer の「記録」ページ（`/tags`）を E2E から操作する helper。
 *
 * ページは左ツリー（接続先 → 収集グループ）・中央タブ（「記録項目」「設定」）・
 * 右ペイン（記録項目の詳細か作成フォーム）の 3 ペイン。右ペインと「設定」タブは
 * 同時に描かれ、どちらにも「名前」欄があるので、ロケータは必ずここの test id で
 * スコープする（スコープしないと strict mode 違反になる）。
 *
 * spec ではないので Playwright は直接実行しない（`*.spec.ts` だけが対象）。
 */
import { expect, type Locator, type Page } from '@playwright/test';

/** 左ペインとそのトグルの名前（ページの `TREE_PANE_LABEL`）。 */
export const TREE_PANE_LABEL = '接続先と収集グループ';

export function escapeRegExp(text: string): string {
	return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
}

export function recordTree(page: Page): Locator {
	return page.getByTestId('record-tree');
}

/** 左ペインの中身（ツールバー・読み込みの失敗の表示・ツリー）。 */
export function recordTreePane(page: Page): Locator {
	return page.getByTestId('record-tree-pane');
}

/**
 * ツリーの行。アクセシブル名は「名前 印…」（接続先は「PLC 直結」「Hub 経由」など、
 * グループは「(件数) 周期」）なので、名前で始まる行を探す。
 */
export function treeNode(page: Page, name: string): Locator {
	return recordTree(page).getByRole('treeitem', {
		name: new RegExp(`^${escapeRegExp(name)}( |$)`)
	});
}

/** ツリーで行を選び、選ばれたことを確かめる。 */
export async function selectTreeNode(page: Page, name: string): Promise<void> {
	await treeNode(page, name).click();
	await expect(treeNode(page, name)).toHaveAttribute('aria-selected', 'true');
}

/** 枠の帯に出る、選択中の対象の名前。 */
export function treeSelection(page: Page): Locator {
	return page.getByTestId('record-tree-selection');
}

/** 右ペイン（記録項目の詳細・作成フォーム）。 */
export function detailPane(page: Page): Locator {
	return page.getByTestId('record-detail-pane');
}

/** 中央の「設定」タブの中身（接続先・収集グループの編集）。 */
export function settingsPane(page: Page): Locator {
	return page.getByTestId('record-settings');
}

/** 中央の「記録項目」タブの一覧（BantoGrid を包む要素）。 */
export function recordList(page: Page): Locator {
	return page.getByTestId('record-list');
}

export async function openSettingsTab(page: Page): Promise<void> {
	await page.getByRole('tab', { name: '設定' }).click();
	await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
}

export async function openListTab(page: Page): Promise<void> {
	await page.getByRole('tab', { name: '記録項目' }).click();
	await expect(page.getByRole('tab', { name: '記録項目' })).toHaveAttribute(
		'aria-selected',
		'true'
	);
}

async function openCreate(page: Page, button: string): Promise<Locator> {
	await page.getByRole('button', { name: button, exact: true }).click();
	const pane = detailPane(page);
	await expect(pane.getByRole('heading', { level: 3, name: button })).toBeVisible();
	return pane;
}

/** 左のツールバーの「接続先を追加」。右ペインの作成フォームを返す。 */
export function openCreateConnection(page: Page): Promise<Locator> {
	return openCreate(page, '接続先を追加');
}

/** 左のツールバーの「収集グループを追加」。 */
export function openCreateGroup(page: Page): Promise<Locator> {
	return openCreate(page, '収集グループを追加');
}

/** 「記録項目」タブの「記録項目を追加」（収集グループを選んでいるときだけ出る）。 */
export function openCreateRecord(page: Page): Promise<Locator> {
	return openCreate(page, '記録項目を追加');
}
