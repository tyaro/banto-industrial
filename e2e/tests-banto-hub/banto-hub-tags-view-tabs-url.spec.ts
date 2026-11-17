/**
 * #1179 PR3a（docs/banto-hub-ui-improvement-proposal.md §HUB への当てはめ、
 * 2026-11-17 オーナー決定）の実 DOM 受け入れテスト。タグ登録ページの:
 *
 * - 中央のタブ「タグ一覧」「設定」（「すべて」には「設定」が無い）と、「設定」
 *   タブに埋め込んだ接続先・収集グループのフォーム（配下のグループの一覧から
 *   グループの設定へ移れる）
 * - 選択中の対象とタブの URL（`?target=connection:<id>` / `group:<id>`、
 *   `&tab=settings`）と、再読み込み・戻る・進むでの復元
 * - 存在しない id は「すべて」へ戻して URL を正規化し、案内を出す
 * - 「設定」タブのフォームが未保存のままタブ・ツリーの対象を変える（戻るを含む）
 *   ときの確認と、キャンセルしたときに選択・タブ・URL・入力が元のままであること
 * - 「設定」タブへ移ると右の編集ペインが閉じること（未保存なら確かめる）。対象を
 *   変えるだけならペインは開いたままで、確かめない（入力を失わないため）
 *
 * `banto-hub-tags-*.spec.ts` と同じパターン（別 `describe.serial`、前提データは
 * REST で作る、`simulation: true`）。ファイル名は `smoke` より辞書順で後。
 */
import { expect, test, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';
import { cleanupFixtures } from './banto-hub-fixture-cleanup';

const CONN = 'e2e-view-tabs-conn';
const GROUP = 'e2e-view-tabs-group';
const TAG = 'e2e-view-tabs-tag';

test.describe.serial('banto-hub タグ登録: 中央のタブと URL の状態 (#1179 PR3a)', () => {
	let page: Page;
	let headers: Record<string, string>;
	let connId: number;
	let groupId: number;

	const connUrl = (): RegExp => new RegExp(`/tags\\?target=connection:${connId}$`);
	const connSettingsUrl = (): RegExp =>
		new RegExp(`/tags\\?target=connection:${connId}&tab=settings$`);
	const groupSettingsUrl = (): RegExp =>
		new RegExp(`/tags\\?target=group:${groupId}&tab=settings$`);

	/** 接続先の行（シミュレーションの接続は名前の後ろに「⚠ SIM」が付く）。 */
	const connNode = (): ReturnType<Page['getByRole']> =>
		page.getByRole('tree').getByRole('treeitem', { name: new RegExp(`^${CONN}( |$)`) });

	const settingsPane = (): ReturnType<Page['getByTestId']> => page.getByTestId('tag-settings-pane');

	/** 次の `window.confirm` を閉じ（キャンセル）、文言を返す。 */
	function dismissNextConfirm(): () => string | null {
		let message: string | null = null;
		page.once('dialog', (dialog) => {
			message = dialog.message();
			void dialog.dismiss();
		});
		return () => message;
	}

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');
		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		headers = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };
		await cleanupFixtures(page.request, headers, {
			groupNames: [GROUP],
			connectionNames: [CONN]
		});

		const connRes = await page.request.post('/api/plc-connections', {
			headers,
			data: {
				name: CONN,
				protocol: 'modbus-tcp',
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connRes.ok()).toBe(true);
		connId = ((await connRes.json()) as { id: number }).id;

		const groupRes = await page.request.post('/api/collection-groups', {
			headers,
			data: { name: GROUP, plcConnectionId: connId, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;

		const tagRes = await page.request.post('/api/tags', {
			headers,
			data: {
				name: TAG,
				collectionGroupId: groupId,
				address: '40001',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
	});

	test.afterAll(async () => {
		await cleanupFixtures(page.request, headers, {
			groupNames: [GROUP],
			connectionNames: [CONN]
		});
		await page.close();
	});

	test('1. 「すべて」はタブ列なし・接続先を選ぶとタブが出て URL に載る', async () => {
		await page.goto('/tags');
		await expect(page.getByRole('tab', { name: '設定' })).toHaveCount(0);

		await connNode().click();
		await expect(page).toHaveURL(connUrl());
		await expect(page.getByRole('tab', { name: 'タグ一覧' })).toHaveAttribute(
			'aria-selected',
			'true'
		);
		await expect(page.getByRole('tab', { name: '設定' })).toBeVisible();

		await page.getByRole('treeitem', { name: 'すべて', exact: true }).click();
		await expect(page).toHaveURL(/\/tags$/);
		await expect(page.getByRole('tab', { name: '設定' })).toHaveCount(0);
	});

	test('2. 設定タブ: 接続先のフォームと配下のグループ、グループの設定へ移ってもタブを保つ', async () => {
		await page.goto(`/tags?target=connection:${connId}`);
		await page.getByRole('tab', { name: '設定' }).click();
		await expect(page).toHaveURL(connSettingsUrl());
		await expect(settingsPane().getByLabel('名前', { exact: true })).toHaveValue(CONN);

		await settingsPane().getByRole('button', { name: GROUP, exact: true }).click();
		await expect(page).toHaveURL(groupSettingsUrl());
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
		await expect(settingsPane().getByLabel('名前', { exact: true })).toHaveValue(GROUP);

		// 再読み込みで同じ対象とタブに戻る。
		await page.reload();
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
		await expect(settingsPane().getByLabel('名前', { exact: true })).toHaveValue(GROUP);
		await expect(page.getByTestId('tag-tree-selection')).toHaveText(GROUP);
	});

	test('3. 戻る・進むで対象とタブが戻る', async () => {
		await page.goto(`/tags?target=connection:${connId}`);
		await page.getByRole('tab', { name: '設定' }).click();
		await expect(page).toHaveURL(connSettingsUrl());
		await groupNodeByName(page, GROUP).click();
		await expect(page).toHaveURL(groupSettingsUrl());

		await page.goBack();
		await expect(page).toHaveURL(connSettingsUrl());
		await expect(settingsPane().getByLabel('名前', { exact: true })).toHaveValue(CONN);

		await page.goBack();
		await expect(page).toHaveURL(connUrl());
		await expect(page.getByRole('tab', { name: 'タグ一覧' })).toHaveAttribute(
			'aria-selected',
			'true'
		);

		await page.goForward();
		await expect(page).toHaveURL(connSettingsUrl());
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
	});

	test('4. 存在しない id は「すべて」へ戻して URL を直し、案内する', async () => {
		await page.goto('/tags?target=group:2147483647&tab=settings');
		await expect(page).toHaveURL(/\/tags$/);
		await expect(
			page.getByText('指定された接続先・収集グループが見つからないため', { exact: false })
		).toBeVisible();
		await expect(page.getByTestId('tag-tree-selection')).toHaveText('すべて');
		await expect(page.getByRole('tab', { name: '設定' })).toHaveCount(0);
	});

	test('5. 設定タブの未保存: タブ・ツリー・戻るをキャンセルすると、何も変わらない', async () => {
		await page.goto(`/tags?target=connection:${connId}`);
		await page.getByRole('tab', { name: '設定' }).click();
		await settingsPane().getByRole('button', { name: GROUP, exact: true }).click();
		await expect(page).toHaveURL(groupSettingsUrl());
		const nameInput = settingsPane().getByLabel('名前', { exact: true });
		await nameInput.fill(`${GROUP}-edited`);

		// タブ
		let message = dismissNextConfirm();
		await page.getByRole('tab', { name: 'タグ一覧' }).click();
		await expect.poll(message).toBe('変更を破棄しますか？');
		await expect(page.getByRole('tab', { name: '設定' })).toHaveAttribute('aria-selected', 'true');
		await expect(page).toHaveURL(groupSettingsUrl());
		await expect(nameInput).toHaveValue(`${GROUP}-edited`);

		// ツリーの対象
		message = dismissNextConfirm();
		await connNode().click();
		await expect.poll(message).toBe('変更を破棄しますか？');
		await expect(page.getByTestId('tag-tree-selection')).toHaveText(GROUP);
		await expect(groupNodeByName(page, GROUP)).toHaveAttribute('aria-selected', 'true');
		await expect(page).toHaveURL(groupSettingsUrl());
		await expect(nameInput).toHaveValue(`${GROUP}-edited`);

		// 戻る（SvelteKit が取り消して元の項目へ戻す）
		message = dismissNextConfirm();
		await page.evaluate(() => history.back());
		await expect.poll(message).toBe('変更を破棄しますか？');
		await expect(page).toHaveURL(groupSettingsUrl());
		await expect(nameInput).toHaveValue(`${GROUP}-edited`);

		// 受け入れれば移る（入力は捨てる）
		page.once('dialog', (dialog) => void dialog.accept());
		await page.getByRole('tab', { name: 'タグ一覧' }).click();
		await expect(page).toHaveURL(new RegExp(`/tags\\?target=group:${groupId}$`));
		await expect(page.getByRole('tab', { name: 'タグ一覧' })).toHaveAttribute(
			'aria-selected',
			'true'
		);
		await page.getByRole('tab', { name: '設定' }).click();
		await expect(settingsPane().getByLabel('名前', { exact: true })).toHaveValue(GROUP);
	});

	test('6. 右の編集ペイン: 設定タブへ移ると閉じる（未保存なら確かめる）・対象を変えても入力は残る', async () => {
		await page.goto(`/tags?target=group:${groupId}`);
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG);
		await page.getByRole('gridcell', { name: TAG, exact: true }).click();
		const pane = page.getByRole('complementary', { name: `${TAG} を編集` });
		await expect(pane).toBeVisible();

		// 変更なし: 設定タブへ移ると確認なしで閉じる。
		await page.getByRole('tab', { name: '設定' }).click();
		await expect(pane).toBeHidden();
		await page.getByRole('tab', { name: 'タグ一覧' }).click();

		// 未保存のまま対象を変えても、ペインは非モーダルで入力を失わないので確かめない
		// （#375・#342 段階C の「ツリーで別のグループへ移って式へ挿入する」流れを保つ）。
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG);
		await page.getByRole('gridcell', { name: TAG, exact: true }).click();
		await expect(pane).toBeVisible();
		await pane.getByLabel('名前').fill(`${TAG}-edited`);
		let dialogShown = false;
		const onDialog = (): void => {
			dialogShown = true;
		};
		page.on('dialog', onDialog);
		await connNode().click();
		await expect(page).toHaveURL(connUrl());
		await expect(pane).toBeVisible();
		await expect(pane.getByLabel('名前')).toHaveValue(`${TAG}-edited`);
		page.off('dialog', onDialog);
		expect(dialogShown).toBe(false);

		// 未保存のまま設定タブへ: キャンセルならタブもペインもそのまま。
		const message = dismissNextConfirm();
		await page.getByRole('tab', { name: '設定' }).click();
		await expect.poll(message).toBe('変更を破棄しますか？');
		await expect(page.getByRole('tab', { name: 'タグ一覧' })).toHaveAttribute(
			'aria-selected',
			'true'
		);
		await expect(page).toHaveURL(connUrl());
		await expect(pane.getByLabel('名前')).toHaveValue(`${TAG}-edited`);

		// 受け入れればペインを閉じて設定タブへ。
		page.once('dialog', (dialog) => void dialog.accept());
		await page.getByRole('tab', { name: '設定' }).click();
		await expect(page).toHaveURL(connSettingsUrl());
		await expect(pane).toBeHidden();
	});
});
