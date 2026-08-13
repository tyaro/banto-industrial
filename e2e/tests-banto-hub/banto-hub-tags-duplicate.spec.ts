/**
 * タグ複製（T18-3a、docs/banto-hub-t18-design.md「T18-3a タグ複製」、
 * TAG-UX-D 前半「『このタグを複製』、型/単位/スケーリング/しきい値を引継ぎ
 * 名前とアドレスのみ変更する」）の実 DOM 受け入れテスト。
 *
 * `banto-hub-tags-form.spec.ts`/`banto-hub-tags-revision.spec.ts` と同じ
 * パターン: 別 `describe.serial` ブロック（別 `page`）、認証・前提データ
 * （PLC接続・収集グループ・複製元タグ）は `page.request` で直接 REST を
 * 叩いて作る（実 PLC/実ネットワークへは繋がない — `simulation: true`）。
 * 共有 DB を壊さないよう固定名は `RUN_ID` で一意化し、失敗テストの
 * リトライで `beforeAll` が再走したときのための冪等掃除を持つ
 * （`banto-hub-tags-revision.spec.ts` の `cleanupExistingFixtures` と同型）。
 */
import { expect, test, type APIRequestContext, type Page } from '@playwright/test';
import { CSRF_HEADERS, fetchAuthToken, injectAuthToken } from './banto-hub-auth';

const RUN_ID = Date.now();
const CONNECTION_NAME = `e2e-dup-plc-${RUN_ID}`;
const GROUP_NAME = `e2e-dup-group-${RUN_ID}`;
const SOURCE_TAG_NAME = `e2e-dup-src-${RUN_ID}`;
const DUPLICATE_TAG_NAME = `${SOURCE_TAG_NAME}_copy`;
const SOURCE_ADDRESS = '40001';
const DUPLICATE_ADDRESS = '40011';
const SOURCE_UNIT = '℃';

interface TagResponse {
	id: number;
	name: string;
	address: string;
	unit: string | null;
	collectionGroupId: number;
}

/**
 * この spec が使う固定名の PLC接続・収集グループ・配下タグを、存在すれば
 * 掃除する（`plc_connections`/`collection_groups` は `name` UNIQUE のため、
 * 失敗リトライで `beforeAll` が再走すると同名 POST が UNIQUE 違反で落ちる -
 * `banto-hub-tags-revision.spec.ts` と同じ理由）。FK は RESTRICT なので
 * 削除順は タグ → グループ → 接続。
 */
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

test.describe.serial('banto-hub タグ複製 (T18-3a)', () => {
	let page: Page;
	let authedHeaders: Record<string, string>;
	let groupId: number;
	let sourceTagId: number;

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
		groupId = group.id;

		// 複製元タグ: 型/単位/小数桁を持たせ、複製で引き継がれることを確認できる
		// ようにする（modbus-tcp 配下なので Modbus 参照番号形式のアドレス）。
		const tagRes = await page.request.post('/api/tags', {
			headers: authedHeaders,
			data: {
				name: SOURCE_TAG_NAME,
				collectionGroupId: group.id,
				address: SOURCE_ADDRESS,
				dataType: 'i16',
				decimals: 0,
				unit: SOURCE_UNIT,
				enabled: true,
				writable: false,
				tagKind: 'plc'
			}
		});
		expect(tagRes.ok()).toBe(true);
		const tag = (await tagRes.json()) as TagResponse;
		sourceTagId = tag.id;
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループ/タグは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	test('1. 複製ボタンで create Drawer に切替わり、名前が _copy・アドレス空・差分パネルが出る', async () => {
		await page.goto('/tags');
		// 共有 DB には他 spec のタグも並ぶため、検索で複製元1件へ絞ってから開く。
		await page.getByPlaceholder('名前・アドレスで検索').fill(SOURCE_TAG_NAME);
		await page.getByRole('gridcell', { name: SOURCE_TAG_NAME, exact: true }).click();

		const editDrawer = page.getByRole('dialog', { name: `${SOURCE_TAG_NAME} を編集` });
		await expect(editDrawer).toBeVisible();
		await editDrawer.getByTestId('tag-duplicate-button').click();

		// 複製は「新規作成」（create Drawer）へ切り替わる。
		const createDrawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(createDrawer).toBeVisible();
		// 名前は衝突しない複製名（`{元名}_copy`）、アドレスは空。
		await expect(createDrawer.getByLabel('名前')).toHaveValue(DUPLICATE_TAG_NAME);
		await expect(createDrawer.getByLabel('アドレス')).toHaveValue('');
		// 単位（複製元 ℃）は引き継がれている。
		await expect(createDrawer.getByLabel('単位')).toHaveValue(SOURCE_UNIT);
		// 複製元との差分パネル。
		await expect(createDrawer.getByRole('heading', { name: '複製元との差分' })).toBeVisible();
	});

	test('2. アドレスを入れて登録すると複製タグが増え、複製元は上書きされない', async () => {
		const createDrawer = page.getByRole('dialog', { name: '新規作成' });
		await createDrawer.getByLabel('アドレス').fill(DUPLICATE_ADDRESS);
		await createDrawer.getByRole('button', { name: '登録して閉じる' }).click();

		await expect(page.getByText('作成しました')).toBeVisible();
		await expect(createDrawer).toBeHidden();

		const tagsRes = await page.request.get('/api/tags', { headers: authedHeaders });
		expect(tagsRes.ok()).toBe(true);
		const tags = (await tagsRes.json()) as TagResponse[];

		// 複製タグ: 新アドレスを持ち、単位/グループは複製元から引き継ぐ。
		const duplicated = tags.filter((t) => t.name === DUPLICATE_TAG_NAME);
		expect(duplicated).toHaveLength(1);
		expect(duplicated[0].collectionGroupId).toBe(groupId);
		expect(duplicated[0].unit).toBe(SOURCE_UNIT);
		expect(duplicated[0].address).toBe(DUPLICATE_ADDRESS);

		// 複製元は不変（新規 POST 経路なので上書きされない）。
		const source = tags.find((t) => t.id === sourceTagId);
		expect(source).toBeDefined();
		expect(source?.name).toBe(SOURCE_TAG_NAME);
		expect(source?.address).toBe(SOURCE_ADDRESS);
		expect(source?.unit).toBe(SOURCE_UNIT);
		// 複製元と同名のタグは依然1件だけ（複製で名前が衝突していない）。
		expect(tags.filter((t) => t.name === SOURCE_TAG_NAME)).toHaveLength(1);
	});
});
