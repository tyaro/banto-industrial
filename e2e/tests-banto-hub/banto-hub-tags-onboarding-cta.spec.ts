/**
 * 登録後の「確認導線」CTA バナー（T18-4c、docs/hub/banto-hub-t18-design.md
 * 「T18-4c 確認導線」、TAG-UX-H「新規／変更タグを『確認対象』として値・
 * 品質・時刻へ1クリックで移動できるようにする」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-*.spec.ts` と同じパターン: 別 `describe.serial` ブロック
 * （別 `page`）、認証・前提データ（PLC接続・収集グループ）は `page.request`
 * で直接 REST を叩いて作る（`simulation: true`、実 PLC 不要）。タグ自体は
 * UI 経由で登録し、成功後の CTA バナーとその遷移先を検証する。共有 DB を
 * 壊さないよう固定名は `RUN_ID` で一意化し、リトライ再走用の冪等掃除を持つ。
 *
 * ファイル名は `banto-hub-smoke.spec.ts` より辞書順で後にする必要がある
 * （`banto-hub-auth.ts` 参照）ため `banto-hub-tags-*`。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, groupNodeByName, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-cta-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-cta-group-${RUN_ID}`;
const TAG_NAME = `e2e-cta-tag-${RUN_ID}`;
const EXTERNAL_NAME = `${CONNECTION_NAME}.${GROUP_NAME}.${TAG_NAME}`;
const CTA_TEXT = '確認: 値・品質・時刻を見る';

async function cleanupExistingFixtures(
	request: APIRequestContext,
	headers: Record<string, string>
): Promise<void> {
	const groupsRes = await request.get('/api/collection-groups', { headers });
	if (groupsRes.ok()) {
		const groups = (await groupsRes.json()) as Array<{ id: number; name: string }>;
		const existingGroup = groups.find((g) => g.name === GROUP_NAME);
		if (existingGroup) {
			const tagsRes = await request.get('/api/tags', { headers });
			if (tagsRes.ok()) {
				const tags = (await tagsRes.json()) as Array<{ id: number; collectionGroupId: number }>;
				for (const tag of tags.filter((t) => t.collectionGroupId === existingGroup.id)) {
					await request.delete(`/api/tags/${tag.id}`, { headers });
				}
			}
			await request.delete(`/api/collection-groups/${existingGroup.id}`, { headers });
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

test.describe.serial('banto-hub 登録後の確認導線 CTA (T18-4c)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupId: number;

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
				enabled: true,
				simulation: true,
				settings: { host: '127.0.0.1', port: 502, unit_id: 1 }
			}
		});
		expect(connectionRes.ok()).toBe(true);
		const connection = (await connectionRes.json()) as { id: number };

		const groupRes = await page.request.post('/api/collection-groups', {
			headers: authedHeaders,
			data: { name: GROUP_NAME, plcConnectionId: connection.id, periodMs: 1000, enabled: true }
		});
		expect(groupRes.ok()).toBe(true);
		groupId = ((await groupRes.json()) as { id: number }).id;
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループ/タグは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	test('1. UI でタグ登録すると確認 CTA が出て、href が /tags?target=group:...&focus=... になる', async () => {
		await page.goto('/tags');
		// T19 S1-c（UX-33）: 「新規登録」はツリーでグループが選択されている
		// ときしか出ない上、開いた create Drawer は選択中グループへ確定済み
		// （収集グループの `<select>` が disabled）になる - まずツリーで対象
		// グループを選ぶ（`groupNodeByName` は `banto-hub-auth.ts` 参照）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: 'タグを追加' }).click();
		// #342 段階C: 新規作成フォームも広幅では非モーダルの右ペイン
		// （`<aside aria-label>` = role `complementary`）へ移った（#375 の編集
		// ペインと同じ型）ため `role="dialog"` では取れない。アクセシブル名
		// （`新規作成`）は Modal 時代と同じ。
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(TAG_NAME);
		await drawer.getByLabel('アドレス').fill('40001');
		await drawer.getByRole('button', { name: '保存して閉じる' }).click();

		await expect(page.getByText('作成しました')).toBeVisible();

		const cta = page.getByRole('link', { name: CTA_TEXT });
		await expect(cta).toBeVisible();
		const href = await cta.getAttribute('href');
		expect(href).not.toBeNull();
		// 対象タグのグループへ絞り、対象タグを focus 対象にした /tags リンク（同じページの中の遷移。#1255 段階 ③）。
		expect(href!.startsWith(`/tags?target=group:${groupId}`)).toBe(true);
		expect(href).toContain(`focus=${encodeURIComponent(EXTERNAL_NAME)}`);
	});

	test('2. CTA クリックで focus つきの URL へ替わり、ConnectionTree がそのグループに絞られる', async () => {
		await page.getByRole('link', { name: CTA_TEXT }).click();

		await expect(page).toHaveURL(new RegExp(`/tags\\?target=group:${groupId}&focus=`));
		// 登録したタグ行が確認対象（focus ハイライト）として出る。
		const highlighted = page.locator('.row.confirm-target');
		await expect(highlighted).toHaveCount(1);
		await expect(highlighted).toContainText(TAG_NAME);
		// ツリーはそのグループを選択状態にしている。
		await expect(page.getByRole('treeitem', { selected: true })).toContainText(GROUP_NAME);
	});

	// #1255: 確認のリンクは同じページの中の遷移なので、検索欄に前の語が残ると新しい名前の行が
	// 隠れる。押したときに検索語を消す。
	test('3. 合わない検索語を入れたまま作成して確認を押すと、検索が空になり新しい行が強調される', async () => {
		const name = `${TAG_NAME}-b`;
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		const search = page.getByPlaceholder('名前・アドレスで検索');
		await search.fill('zzz-no-such-tag');
		await page.getByRole('button', { name: 'タグを追加' }).click();
		const drawer = page.getByRole('complementary', { name: '新規作成' });
		await drawer.getByLabel('名前').fill(name);
		await drawer.getByLabel('アドレス').fill('40002');
		await drawer.getByRole('button', { name: '保存して閉じる' }).click();
		await expect(page.getByText('作成しました')).toBeVisible();

		await page.getByRole('link', { name: CTA_TEXT }).click();
		await expect(search).toHaveValue('');
		await expect(page.locator('.row.confirm-target')).toContainText(name);
	});

	test('4. 旧名の検索語を入れたまま改名して確認を押すと、検索が空になり新しい名前の行が強調される', async () => {
		const oldName = `${TAG_NAME}-b`;
		const newName = `${TAG_NAME}-renamed`;
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		const search = page.getByPlaceholder('名前・アドレスで検索');
		await search.fill(oldName);
		await page.getByRole('gridcell', { name: oldName, exact: true }).click();
		const drawer = page.getByRole('complementary', { name: `${oldName} を編集` });
		await expect(drawer).toBeVisible();
		await drawer.getByLabel('名前').fill(newName);
		await drawer.getByRole('button', { name: '保存', exact: true }).click();
		await expect(page.getByText('更新しました')).toBeVisible();

		await page.getByRole('link', { name: CTA_TEXT }).click();
		await expect(search).toHaveValue('');
		await expect(page.locator('.row.confirm-target')).toContainText(newName);
	});

	// #1255: ほかの所（API）で足されたタグが、開いている /tags に config_changed で出る。
	test('5. 別の経路でタグが足されると、開いている /tags の一覧に行が出る', async () => {
		const name = `${TAG_NAME}-external`;
		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByPlaceholder('名前・アドレスで検索').fill(name);
		await expect(page.getByRole('gridcell', { name, exact: true })).toHaveCount(0);

		const res = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name,
				collectionGroupId: groupId,
				address: '40003',
				dataType: 'i16',
				decimals: 0,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(res.ok(), await res.text()).toBe(true);
		await expect(page.getByRole('gridcell', { name, exact: true })).toBeVisible({
			timeout: 10_000
		});
	});

	// #1255: 編集フォームを開いている間に config_changed の読み直しが返っても、結果を当てず「読み直す」の知らせを出す。
	test('6. 読み直しの応答待ちの間に編集フォームを開くと、一覧は変えず知らせを出し、読み直すで反映される', async () => {
		const baseName = `${TAG_NAME}-base6`;
		const addedName = `${TAG_NAME}-added6`;
		const body = (name: string, address: string) => ({
			name,
			collectionGroupId: groupId,
			address,
			dataType: 'i16',
			decimals: 0,
			enabled: true,
			writable: false,
			tagKind: 'plc'
		});
		const baseRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: body(baseName, '40010')
		});
		expect(baseRes.ok(), await baseRes.text()).toBe(true);

		await page.goto('/tags');
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByPlaceholder('名前・アドレスで検索').fill(TAG_NAME);
		const baseCell = page.getByRole('gridcell', { name: baseName, exact: true });
		await expect(baseCell).toBeVisible({ timeout: 10_000 });

		let hold = false;
		let release!: () => void;
		const released = new Promise<void>((resolve) => (release = resolve));
		let reachedResolve!: () => void;
		const reached = new Promise<void>((resolve) => (reachedResolve = resolve));
		const pattern = '**/api/tags';
		await page.route(pattern, async (route) => {
			if (hold && route.request().method() === 'GET') {
				reachedResolve();
				await released;
			}
			await route.continue();
		});
		try {
			hold = true;
			const addRes = await page.request.post('/api/tags', {
				headers: authedHeaders,
				data: body(addedName, '40011')
			});
			expect(addRes.ok(), await addRes.text()).toBe(true);
			// config_changed → 500ms の debounce → reload() の GET が止まった所に当たる
			await reached;
			hold = false;

			await baseCell.click();
			await expect(page.getByRole('complementary', { name: `${baseName} を編集` })).toBeVisible();
			release();

			await expect(page.getByTestId('tag-config-changed')).toBeVisible({ timeout: 10_000 });
			await expect(page.getByRole('complementary', { name: `${baseName} を編集` })).toBeVisible();
			await expect(page.getByRole('gridcell', { name: addedName, exact: true })).toHaveCount(0);

			await page
				.getByRole('complementary', { name: `${baseName} を編集` })
				.getByRole('button', { name: '閉じる' })
				.click();
			await page.getByTestId('tag-config-changed-reload').click();
			await expect(page.getByTestId('tag-config-changed')).toHaveCount(0);
			await expect(page.getByRole('gridcell', { name: addedName, exact: true })).toBeVisible({
				timeout: 10_000
			});
		} finally {
			release();
			await page.unroute(pattern);
		}
	});
});
