/**
 * BantoGrid セル編集/TSV貼付の接続（T18-3e、docs/banto-hub-t18-design.md
 * 「T18-3e BantoGrid セル編集/TSV貼付の接続」、TAG-UX-D 後半）の実 DOM
 * 受け入れテスト。
 *
 * `banto-hub-tags-bulk.spec.ts`（T18-3b）と同じパターン: 別
 * `describe.serial` ブロック（別 `page`）、認証・前提データは
 * `page.request` で直接 REST を叩いて作る。「表編集」トグルは
 * `tag-selection-mode-toggle` と相互排他・「収集停止中のみ」有効
 * （フレッシュな E2E サーバーは収集が既定で停止中 - `banto_hub_core::
 * controller::CollectionController::new` の初期状態が `Stopped`）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-celledit-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-celledit-group-${RUN_ID}`;
const TAG_NAME = `e2e-celledit-tag-${RUN_ID}`;

interface TagResponse {
	id: number;
	name: string;
	enabled: boolean;
	writable: boolean;
	unit: string | null;
	decimals: number;
	revision: number;
}

async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const targetGroup = groups.find((g) => g.name === GROUP_NAME);
		if (targetGroup) {
			const tagsRes = await request.get('/api/tags', { headers });
			if (tagsRes.ok()) {
				const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
				for (const tag of tags.filter((t) => t.collectionGroupId === targetGroup.id)) {
					await request.delete(`/api/tags/${tag.id}`, { headers });
				}
			}
			await request.delete(`/api/collection-groups/${targetGroup.id}`, { headers });
		}
	}
	const connectionsRes = await request.get('/api/plc-connections', { headers });
	if (connectionsRes.ok()) {
		const connections = (await connectionsRes.json()) as Array<{ id: number; name: string }>;
		const existingConnection = connections.find((c) => c.name === CONNECTION_NAME);
		if (existingConnection) {
			await request.delete(`/api/plc-connections/${existingConnection.id}`, { headers });
		}
	}
}

test.describe.serial('banto-hub タグ表編集 (T18-3e)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let tagId: number;

	test.beforeAll(async ({ browser }) => {
		page = await browser.newPage();
		await page.goto('/login');

		const token = await fetchAuthToken(page.request);
		await injectAuthToken(page, token);
		authedHeaders = { ...CSRF_HEADERS, Authorization: `Bearer ${token}` };

		await cleanupExistingFixtures(page.request, authedHeaders);

		const connectionRes = await page.request.post('/api/plc-connections', {
			headers: authedHeaders,
			data: {
				name: CONNECTION_NAME,
				protocol: 'modbus-tcp',
				host: '127.0.0.1',
				port: 502,
				unitId: 1,
				enabled: true,
				simulation: true
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		const group = (await groupRes.json()) as { id: number };

		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: TAG_NAME,
				collectionGroupId: group.id,
				address: '40001',
				dataType: 'i16',
				unit: 'kPa',
				decimals: 1,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
		tagId = ((await tagRes.json()) as { id: number }).id;
	});

	test.afterAll(async () => {
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	async function fetchTag(): Promise<TagResponse> {
		const res = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(res.ok()).toBe(true);
		const tags = (await res.json()) as TagResponse[];
		const tag = tags.find((t) => t.id === tagId);
		if (!tag) throw new Error('fixture tag not found');
		return tag;
	}

	test('0. 新規E2Eサーバーは収集停止中なので「表編集」トグルが有効', async () => {
		await page.goto('/tags');
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_NAME);
		await expect(page.getByRole('gridcell', { name: TAG_NAME, exact: true })).toBeVisible();
		await expect(page.getByTestId('tag-grid-edit-mode-toggle')).toBeEnabled();
	});

	test('1. 表編集モードON中は単一クリックで編集Drawerが開かない（ダブルクリックへ切替わる）', async () => {
		await page.getByTestId('tag-grid-edit-mode-toggle').click();
		await expect(page.getByTestId('tag-grid-edit-mode-toggle')).toHaveText('表編集を終了');

		// 単一クリック（アドレス列 - 編集不可列）では Drawer が開かない。
		await page.getByRole('gridcell', { name: '40001', exact: true }).click();
		await expect(page.getByRole('dialog', { name: `${TAG_NAME} を編集` })).toBeHidden();
	});

	test('2. enabled チェックボックスをオフにすると保留バーが出る', async () => {
		// `role=gridcell[name="はい"]` は編集モードへ入ると（BantoGrid が
		// テキストをチェックボックス input へ差し替えるため）アクセシブル名が
		// 変わってしまい、同じ Locator で二度目のアクション（uncheck）を
		// 引くと再マッチに失敗する - 編集状態でも変わらない
		// `data-cell-field` 属性で1つのセルを固定して掴む。
		const enabledCell = page.locator('[data-cell-field="enabled"]');
		await enabledCell.dblclick();
		// BantoGrid のチェックボックス editor はトグルした瞬間に commit され、
		// セルがテキスト表示へ再描画されて input が detach する。`uncheck()`/
		// `check()` は「クリック後に目的の checked 状態で安定する」ことを待つ
		// ため、detach した input に対して検証がタイムアウトする - 事後状態を
		// 待たない単純な `click()` でトグル（=commit）させる。
		await enabledCell.locator('input[type="checkbox"]').click();
		// checkbox-toggle で既に commit 済み（Tab は保険。詳細は上コメント）。
		await page.keyboard.press('Tab');

		const bar = page.getByTestId('tag-cell-edit-bar');
		await expect(bar).toBeVisible();
		await expect(bar).toContainText('保留中の編集 1 件');
	});

	test('3. 「保存」→ preflight → 差分確認パネルに変更内容が出る', async () => {
		await page.getByTestId('tag-cell-edit-save').click();
		const panel = page.getByTestId('tag-cell-edit-confirm-panel');
		await expect(panel).toBeVisible();
		await expect(panel).toContainText('対象 1 件');
		await expect(panel).toContainText('有効');
	});

	test('4. 「この内容で保存を適用」で all-or-nothing 適用され、DB に反映される', async () => {
		await page.getByTestId('tag-cell-edit-apply').click();
		await expect(page.getByTestId('tag-cell-edit-confirm-panel')).toBeHidden();
		await expect(page.getByTestId('tag-cell-edit-bar')).toBeHidden();

		const tag = await fetchTag();
		expect(tag.enabled).toBe(false);
	});

	test('5. 「破棄」で保留中の編集を戻せる（適用しない）', async () => {
		const enabledCell = page.locator('[data-cell-field="enabled"]');
		await enabledCell.dblclick();
		// テスト#2 と同じ理由で toggle は `click()`（commit で input が detach
		// するため `check()` の事後状態待ちはタイムアウトする）。
		await enabledCell.locator('input[type="checkbox"]').click();
		await page.keyboard.press('Tab');

		const bar = page.getByTestId('tag-cell-edit-bar');
		await expect(bar).toBeVisible();
		await page.getByTestId('tag-cell-edit-discard').click();
		await expect(bar).toBeHidden();

		// 破棄したので API 上はまだ enabled=false のまま。
		const tag = await fetchTag();
		expect(tag.enabled).toBe(false);
	});

	test('6. 「表編集を終了」で単一クリック編集に戻る', async () => {
		await page.getByTestId('tag-grid-edit-mode-toggle').click();
		await expect(page.getByTestId('tag-grid-edit-mode-toggle')).toHaveText('表編集');

		await page.getByRole('gridcell', { name: TAG_NAME, exact: true }).click();
		await expect(page.getByRole('dialog', { name: `${TAG_NAME} を編集` })).toBeVisible();
		await page.keyboard.press('Escape');
	});
});
