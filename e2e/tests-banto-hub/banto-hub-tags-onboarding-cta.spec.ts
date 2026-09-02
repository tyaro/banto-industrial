/**
 * 登録後の「確認導線」CTA バナー（T18-4c、docs/banto-hub-t18-design.md
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
		groupId = ((await groupRes.json()) as { id: number }).id;
	});

	test.afterAll(async () => {
		// 共有 DB を成長させない（後続 spec が仮想化グリッドで自分の行を
		// 見失わないよう、作った接続/グループ/タグは実行後に片付ける）。
		await cleanupExistingFixtures(page.request, authedHeaders);
		await page.close();
	});

	test('1. UI でタグ登録すると確認 CTA が出て、href が /monitor?group=...&focus=... になる', async () => {
		await page.goto('/tags');
		// T19 S1-c（UX-33）: 「新規登録」はツリーでグループが選択されている
		// ときしか出ない上、開いた create Drawer は選択中グループへ確定済み
		// （収集グループの `<select>` が disabled）になる - まずツリーで対象
		// グループを選ぶ（`groupNodeByName` は `banto-hub-auth.ts` 参照）。
		await groupNodeByName(page, GROUP_NAME).click();
		await page.getByRole('button', { name: '新規登録' }).click();
		const drawer = page.getByRole('dialog', { name: '新規作成' });
		await expect(drawer).toBeVisible();

		await drawer.getByLabel('名前').fill(TAG_NAME);
		await drawer.getByLabel('アドレス').fill('40001');
		await drawer.getByRole('button', { name: '登録して閉じる' }).click();

		await expect(page.getByText('作成しました')).toBeVisible();

		const cta = page.getByRole('link', { name: CTA_TEXT });
		await expect(cta).toBeVisible();
		const href = await cta.getAttribute('href');
		expect(href).not.toBeNull();
		// 対象タグのグループへ絞り、対象タグを focus 対象にした /monitor リンク。
		expect(href!.startsWith(`/monitor?group=${groupId}`)).toBe(true);
		expect(href).toContain(`focus=${encodeURIComponent(EXTERNAL_NAME)}`);
	});

	test('2. CTA クリックで /monitor へ遷移し、ConnectionTree がそのグループに絞られる', async () => {
		await page.getByRole('link', { name: CTA_TEXT }).click();

		await expect(page).toHaveURL(new RegExp(`/monitor\\?group=${groupId}`));
		// 登録したタグ行が確認対象（focus ハイライト）として出る。
		await expect(page.getByText(EXTERNAL_NAME)).toBeVisible();
		const highlighted = page.locator('tr.confirm-target');
		await expect(highlighted).toHaveCount(1);
		await expect(highlighted).toContainText(EXTERNAL_NAME);
		// ツリーはそのグループを選択状態にしている。
		await expect(page.getByRole('treeitem', { selected: true })).toContainText(GROUP_NAME);
	});
});
