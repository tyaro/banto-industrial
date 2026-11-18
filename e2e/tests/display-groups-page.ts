/**
 * #1179 PR6a: ChronoGazer の「表示グループ」ページ（`/groups`）を E2E から操作する helper。
 *
 * ページは左の一覧（表示グループの 1 階層）・中央のタブ（「ペン」「設定」）・
 * 右ペイン（選んだペンのタグと色）の 3 ペイン。保存・取り消し・未保存の表示は
 * 中央の上（どちらのタブでも見える所）にある。名前・表示種別は「設定」タブ、
 * ペンのタグ・色は右ペインにしか無いので、ロケータはここで必ず絞る。
 * 「記録」ページの helper は `records-page.ts`。
 *
 * spec ではないので Playwright は直接実行しない（`*.spec.ts` だけが対象）。
 */
import { expect, type Locator, type Page } from '@playwright/test';

/** 左の一覧（`aria-label="表示グループの一覧"`）。 */
export function groupList(page: Page): Locator {
	return page.getByRole('region', { name: '表示グループの一覧' });
}

/** 一覧の行を開くボタン（行の「上へ」「下へ」もグループ名を含むので、開くボタンだけに絞る）。 */
export function groupButton(page: Page, name: string): Locator {
	return groupList(page).locator('button.group-open', { hasText: name });
}

/** 中央（保存の帯とタブ）。 */
export function center(page: Page): Locator {
	return page.getByTestId('display-group-center');
}

/** 右ペイン（選んだペンのタグと色）。開いているときだけ在る。 */
export function penPane(page: Page): Locator {
	return page.getByTestId('display-group-pen-pane');
}

/** 左の「表示グループを追加」（新規作成の下書きを中央に開く）。 */
export function addGroupButton(page: Page): Locator {
	return page.getByRole('button', { name: '表示グループを追加', exact: true });
}

export async function openSettingsTab(page: Page): Promise<void> {
	await page.getByRole('tab', { name: '設定', exact: true }).click();
	await expect(page.getByRole('tab', { name: '設定', exact: true })).toHaveAttribute(
		'aria-selected',
		'true'
	);
}

export async function openPensTab(page: Page): Promise<void> {
	await page.getByRole('tab', { name: 'ペン', exact: true }).click();
	await expect(page.getByRole('tab', { name: 'ペン', exact: true })).toHaveAttribute(
		'aria-selected',
		'true'
	);
}

/** 「ペン」タブで n 本目（1 始まり）のペンの行を選び、右ペインを開く。 */
export async function openPen(page: Page, n: number): Promise<Locator> {
	await page.getByTestId(`display-group-pen-${n}`).click();
	const pane = penPane(page);
	await expect(pane.getByRole('heading', { level: 3, name: `ペン ${n} の設定` })).toBeVisible();
	return pane;
}

/** 一覧のグループを開き、中央に下書きが出るまで待つ（「設定」タブの名前欄で確かめる）。 */
export async function openGroupSettings(page: Page, name: string): Promise<void> {
	await groupButton(page, name).click();
	await openSettingsTab(page);
	await expect(center(page).getByLabel('名前')).toHaveValue(name);
}
